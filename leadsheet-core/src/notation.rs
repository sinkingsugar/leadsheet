//! The note-token grammar shared by the emitter and the parser.
//!
//! ABC-style pitches with grid-cell durations (fractions as in real ABC:
//! `/2` halves the unit), plus tuplet groups:
//!
//! ```text
//! A,,4       A1 (two octaves below A4), 4 cells (a beat at 1/16 grid)
//! ^F2        F#4, 2 cells
//! e          E5, 1 cell (duration 1 is implied)
//! e/2        E5, half a cell (a 32nd)
//! C3/2       C4, a dotted 16th (1.5 cells)
//! z2         rest, 2 cells
//! [CEG]4     C major triad voiced C4 E4 G4, 4 cells
//! C2-        C4, 2 cells, tied into the next bar
//! (3 C D E)4 triplet: three notes evenly dividing 4 cells (one beat)
//! ```
//!
//! Deliberate divergence from real ABC: accidentals are always explicit on
//! the note (`^`/`_`), never implied by a key signature and never sticky
//! within a bar. An LLM (or a human) can read any token in isolation.
//! Case marks the octave as in ABC: `C` = C4 (middle C), `c` = C5,
//! `,` lowers and `'` raises by an octave.
//!
//! Internally durations are ticks ([`MusicalTime`]); the text always
//! speaks 16th cells and lowest-terms fractions of them, so any tick
//! value the IR can hold has exactly one spelling.

use crate::grid::{MusicalTime, TICKS_PER_SIXTEENTH};

/// Per-note dynamic deviation from the pattern's base dynamic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mark {
    #[default]
    None,
    /// `>` prefix: noticeably louder than the pattern base.
    Accent,
    /// `~` prefix: ghost note, noticeably softer.
    Ghost,
}

/// One parsed bar token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Note {
        pitch: u8,
        dur: MusicalTime,
        tie: bool,
        mark: Mark,
    },
    Chord {
        pitches: Vec<u8>,
        dur: MusicalTime,
        tie: bool,
        mark: Mark,
    },
    Rest {
        dur: MusicalTime,
    },
    /// `(n M M …)S`: n members evenly dividing a span of S cells. Members
    /// are bare pitches / chords / rests (marks allowed, no durations);
    /// `tie` carries the group-final `-` into the next token/bar.
    Tuplet {
        n: u32,
        members: Vec<Tok>,
        span: MusicalTime,
        tie: bool,
    },
}

impl Tok {
    /// Total duration this token advances the cursor by.
    pub fn dur(&self) -> MusicalTime {
        match self {
            Tok::Note { dur, .. } | Tok::Chord { dur, .. } | Tok::Rest { dur } => *dur,
            Tok::Tuplet { span, .. } => *span,
        }
    }

    fn tie(&self) -> bool {
        match self {
            Tok::Note { tie, .. } | Tok::Chord { tie, .. } | Tok::Tuplet { tie, .. } => *tie,
            Tok::Rest { .. } => false,
        }
    }
}

/// Spell a tick duration in the text unit: whole cells as an integer
/// (empty when 1), anything else as a lowest-terms fraction of a cell
/// (`/2`, `3/2`, `4/3`). Total: every positive tick value has a spelling.
pub fn dur_text(d: MusicalTime) -> String {
    let t = d.ticks();
    debug_assert!(t > 0, "durations are positive");
    let g = gcd(t, TICKS_PER_SIXTEENTH);
    let (num, den) = (t / g, TICKS_PER_SIXTEENTH / g);
    match (num, den) {
        (1, 1) => String::new(),
        (n, 1) => n.to_string(),
        (1, d) => format!("/{d}"),
        (n, d) => format!("{n}/{d}"),
    }
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

/// Dynamic marks: name ↔ velocity bucket. Unmarked patterns are `f` (96) —
/// the historical default (MuScriptor has no velocity; 96 was always the
/// fallback).
pub const DYNAMICS: [(&str, u8); 6] =
    [("pp", 32), ("p", 48), ("mp", 64), ("mf", 80), ("f", 96), ("ff", 112)];
pub const DEFAULT_VEL: u8 = 96;
/// Rendered offsets for `>` and `~` relative to the pattern base.
pub const ACCENT_DELTA: i16 = 16;
pub const GHOST_DELTA: i16 = -24;
/// Emission thresholds: how far a note's velocity must sit from the base
/// bucket to earn a mark.
pub const ACCENT_THRESHOLD: i16 = 12;
pub const GHOST_THRESHOLD: i16 = -16;

/// Nearest dynamic bucket (ties resolve to the softer one).
pub fn vel_to_dynamic(vel: u8) -> (&'static str, u8) {
    DYNAMICS.iter().copied().min_by_key(|(_, v)| (vel as i16 - *v as i16).abs()).unwrap()
}

pub fn dynamic_to_vel(name: &str) -> Option<u8> {
    DYNAMICS.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

pub fn apply_mark(base: u8, mark: Mark) -> u8 {
    let delta = match mark {
        Mark::None => 0,
        Mark::Accent => ACCENT_DELTA,
        Mark::Ghost => GHOST_DELTA,
    };
    (base as i16 + delta).clamp(1, 127) as u8
}

/// The mark a velocity earns relative to a base bucket (emission side).
pub fn mark_for_vel(vel: u8, base: u8) -> Mark {
    let delta = vel as i16 - base as i16;
    if delta >= ACCENT_THRESHOLD {
        Mark::Accent
    } else if delta <= GHOST_THRESHOLD {
        Mark::Ghost
    } else {
        Mark::None
    }
}

const SHARP_NAMES: [(char, i8); 12] = [
    ('C', 0),
    ('C', 1),
    ('D', 0),
    ('D', 1),
    ('E', 0),
    ('F', 0),
    ('F', 1),
    ('G', 0),
    ('G', 1),
    ('A', 0),
    ('A', 1),
    ('B', 0),
];

const FLAT_NAMES: [(char, i8); 12] = [
    ('C', 0),
    ('D', -1),
    ('D', 0),
    ('E', -1),
    ('E', 0),
    ('F', 0),
    ('G', -1),
    ('G', 0),
    ('A', -1),
    ('A', 0),
    ('B', -1),
    ('B', 0),
];

/// Key-aware spelling: flat keys write `_B`, sharp keys `^A`.
pub fn pitch_to_abc_spelled(pitch: u8, flats: bool) -> String {
    let (letter, acc) =
        if flats { FLAT_NAMES[(pitch % 12) as usize] } else { SHARP_NAMES[(pitch % 12) as usize] };
    let octave = (pitch / 12) as i32 - 1; // MIDI 60 = C4
    let mut s = String::new();
    match acc {
        1 => s.push('^'),
        -1 => s.push('_'),
        _ => {}
    }
    if octave >= 5 {
        s.push(letter.to_ascii_lowercase());
        for _ in 5..octave {
            s.push('\'');
        }
    } else {
        s.push(letter);
        for _ in octave..4 {
            s.push(',');
        }
    }
    s
}

pub fn pitch_to_abc(pitch: u8) -> String {
    pitch_to_abc_spelled(pitch, false)
}

/// Parse one pitch from the start of `s`, returning it and the rest of `s`.
pub fn parse_pitch(s: &str) -> Result<(u8, &str), String> {
    let mut rest = s;
    let acc: i32 = if let Some(r) = rest.strip_prefix("^^") {
        rest = r;
        2
    } else if let Some(r) = rest.strip_prefix('^') {
        rest = r;
        1
    } else if let Some(r) = rest.strip_prefix("__") {
        rest = r;
        -2
    } else if let Some(r) = rest.strip_prefix('_') {
        rest = r;
        -1
    } else if let Some(r) = rest.strip_prefix('=') {
        rest = r;
        0
    } else {
        0
    };
    let c = rest.chars().next().ok_or_else(|| format!("expected a pitch in {s:?}"))?;
    if !c.is_ascii_alphabetic() || !('A'..='G').contains(&c.to_ascii_uppercase()) {
        return Err(format!("bad pitch letter {c:?} in {s:?}"));
    }
    rest = &rest[1..];
    let pc: i32 = match c.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        _ => 11,
    };
    let mut octave: i32 = if c.is_ascii_lowercase() { 5 } else { 4 };
    loop {
        if let Some(r) = rest.strip_prefix('\'') {
            octave += 1;
            rest = r;
        } else if let Some(r) = rest.strip_prefix(',') {
            octave -= 1;
            rest = r;
        } else {
            break;
        }
    }
    let midi = (octave + 1) * 12 + pc + acc;
    let pitch = u8::try_from(midi).ok().filter(|p| *p <= 127);
    match pitch {
        Some(p) => Ok((p, rest)),
        None => Err(format!("pitch out of MIDI range in {s:?}")),
    }
}

fn parse_dur_tie(s: &str, tok: &str) -> Result<(MusicalTime, bool), String> {
    let (dur_str, tie) = match s.strip_suffix('-') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let bad = || format!("bad duration {dur_str:?} in token {tok:?}");
    let (num, den): (u32, u32) = if dur_str.is_empty() {
        (1, 1)
    } else if let Some((n, d)) = dur_str.split_once('/') {
        let num =
            if n.is_empty() { 1 } else { n.parse().ok().filter(|v| *v >= 1).ok_or_else(bad)? };
        let den = d.parse().ok().filter(|v| (1..=960).contains(v)).ok_or_else(bad)?;
        (num, den)
    } else {
        (dur_str.parse::<u32>().ok().filter(|d| *d >= 1).ok_or_else(bad)?, 1)
    };
    let raw = num as i64 * TICKS_PER_SIXTEENTH;
    if raw % den as i64 != 0 {
        return Err(format!(
            "duration {num}/{den} in {tok:?} doesn't land on the tick grid \
             ({TICKS_PER_SIXTEENTH} ticks per 16th) — denominators must divide {num}x{TICKS_PER_SIXTEENTH}"
        ));
    }
    Ok((MusicalTime(raw / den as i64), tie))
}

/// Parse a voice's whitespace-separated tokens, assembling `(n … )S`
/// tuplet groups (whose members are bare pitches/chords/rests, marks
/// allowed, durations forbidden — the group's span divides evenly).
pub fn parse_tokens(voice: &str) -> Result<Vec<Tok>, String> {
    let mut out = Vec::new();
    let mut words = voice.split_whitespace();
    while let Some(word) = words.next() {
        if let Some(head) = word.strip_prefix('(') {
            let n: u32 = head
                .parse()
                .ok()
                .filter(|n| (2..=24).contains(n))
                .ok_or_else(|| format!("bad tuplet arity in {word:?} (want 2..=24)"))?;
            let mut members: Vec<Tok> = Vec::new();
            let (span, tie) = loop {
                let Some(w) = words.next() else {
                    return Err(format!("unclosed tuplet ({n} … — missing `)S`"));
                };
                if let Some(i) = w.find(')') {
                    if i > 0 {
                        members.push(parse_member(&w[..i])?);
                    }
                    break parse_dur_tie(&w[i + 1..], w)?;
                }
                members.push(parse_member(w)?);
            };
            if members.len() != n as usize {
                return Err(format!(
                    "tuplet ({n} …) has {} members, needs exactly {n} (use z for silent slots)",
                    members.len()
                ));
            }
            if span.ticks() % n as i64 != 0 {
                return Err(format!(
                    "a ({n} …){} tuplet doesn't land on the tick grid — {n} must divide \
                     {} ticks; try a different span or arity",
                    dur_text(span),
                    span.ticks(),
                ));
            }
            if tie && matches!(members.last(), Some(Tok::Rest { .. })) {
                return Err("a tuplet ending in a rest cannot be tied".into());
            }
            out.push(Tok::Tuplet { n, members, span, tie });
            continue;
        }
        out.push(parse_token(word)?);
    }
    Ok(out)
}

/// A tuplet member: marked pitch/chord/rest, no duration, no tie.
fn parse_member(word: &str) -> Result<Tok, String> {
    let tok = parse_token(word)?;
    match &tok {
        Tok::Tuplet { .. } => Err(format!("tuplets cannot nest: {word:?}")),
        _ if tok.tie() => Err(format!("tie the whole tuplet after `)S`, not a member: {word:?}")),
        _ if tok.dur() != MusicalTime(TICKS_PER_SIXTEENTH) || ends_with_dur(word) => {
            Err(format!("tuplet members carry no durations (the span divides evenly): {word:?}"))
        }
        _ => Ok(tok),
    }
}

/// Whether the raw token text spells an explicit duration (to tell `C`
/// apart from `C1`, which parse identically).
fn ends_with_dur(word: &str) -> bool {
    word.trim_end_matches('-').ends_with(|c: char| c.is_ascii_digit() || c == '/')
}

/// If a run of equal tick durations reads as an n-tuplet, its (arity,
/// span): the reduced denominator when it isn't a power of two (a run of
/// 32nds is plain notation, not a duplet). Span is always whole cells.
pub fn tuplet_shape(d: MusicalTime) -> Option<(u32, MusicalTime)> {
    let t = d.ticks();
    if t <= 0 {
        return None;
    }
    let g = gcd(t, TICKS_PER_SIXTEENTH);
    let den = TICKS_PER_SIXTEENTH / g;
    if den < 3 || (den as u64).is_power_of_two() || den > 24 {
        return None;
    }
    Some((den as u32, MusicalTime(t * den)))
}

/// Canonical grouping: greedy left-to-right, a maximal-prefix run of n
/// equal tuplet-shaped durations (ties only on the last member) becomes
/// one `(n …)S` group. Deterministic, so emission stays a fixpoint.
pub fn detect_tuplets(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if let Some((n, span)) = tuplet_shape(toks[i].dur()) {
            let end = i + n as usize;
            let groupable = end <= toks.len()
                && toks[i..end]
                    .iter()
                    .all(|t| t.dur() == toks[i].dur() && !matches!(t, Tok::Tuplet { .. }))
                && toks[i..end - 1].iter().all(|t| !t.tie());
            if groupable {
                let mut members: Vec<Tok> = toks[i..end].to_vec();
                let tie = members.last().is_some_and(Tok::tie);
                if let Some(Tok::Note { tie: t, .. } | Tok::Chord { tie: t, .. }) =
                    members.last_mut()
                {
                    *t = false;
                }
                out.push(Tok::Tuplet { n, members, span, tie });
                i = end;
                continue;
            }
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// Parse one whitespace-delimited bar token.
pub fn parse_token(tok: &str) -> Result<Tok, String> {
    let (mark, tok_body) = if let Some(rest) = tok.strip_prefix('>') {
        (Mark::Accent, rest)
    } else if let Some(rest) = tok.strip_prefix('~') {
        (Mark::Ghost, rest)
    } else {
        (Mark::None, tok)
    };
    if mark != Mark::None && tok_body.starts_with('z') {
        return Err(format!("rest cannot carry a dynamic mark: {tok:?}"));
    }
    let parsed = parse_token_unmarked(tok_body)?;
    Ok(match parsed {
        Tok::Note { pitch, dur, tie, .. } => Tok::Note { pitch, dur, tie, mark },
        Tok::Chord { pitches, dur, tie, .. } => Tok::Chord { pitches, dur, tie, mark },
        rest => rest,
    })
}

fn parse_token_unmarked(tok: &str) -> Result<Tok, String> {
    if let Some(rest) = tok.strip_prefix('[') {
        let (inner, after) =
            rest.split_once(']').ok_or_else(|| format!("unclosed chord in {tok:?}"))?;
        let mut pitches = Vec::new();
        let mut cur = inner;
        while !cur.is_empty() {
            let (p, rest) = parse_pitch(cur)?;
            pitches.push(p);
            cur = rest;
        }
        if pitches.is_empty() {
            return Err(format!("empty chord in {tok:?}"));
        }
        let (dur, tie) = parse_dur_tie(after, tok)?;
        return Ok(Tok::Chord { pitches, dur, tie, mark: Mark::None });
    }
    if let Some(rest) = tok.strip_prefix('z') {
        let (dur, tie) = parse_dur_tie(rest, tok)?;
        if tie {
            return Err(format!("rest cannot be tied: {tok:?}"));
        }
        return Ok(Tok::Rest { dur });
    }
    let (pitch, rest) = parse_pitch(tok)?;
    let (dur, tie) = parse_dur_tie(rest, tok)?;
    Ok(Tok::Note { pitch, dur, tie, mark: Mark::None })
}

pub fn emit_token(tok: &Tok) -> String {
    emit_token_spelled(tok, false)
}

pub fn emit_token_spelled(tok: &Tok, flats: bool) -> String {
    let mut s = String::new();
    match tok {
        Tok::Tuplet { n, members, span, tie } => {
            s.push('(');
            s.push_str(&n.to_string());
            for m in members {
                s.push(' ');
                // Members: mark + pitch content only; span carries the time.
                match m {
                    Tok::Note { mark: Mark::Accent, .. }
                    | Tok::Chord { mark: Mark::Accent, .. } => s.push('>'),
                    Tok::Note { mark: Mark::Ghost, .. } | Tok::Chord { mark: Mark::Ghost, .. } => {
                        s.push('~')
                    }
                    _ => {}
                }
                match m {
                    Tok::Note { pitch, .. } => s.push_str(&pitch_to_abc_spelled(*pitch, flats)),
                    Tok::Chord { pitches, .. } => {
                        s.push('[');
                        for p in pitches {
                            s.push_str(&pitch_to_abc_spelled(*p, flats));
                        }
                        s.push(']');
                    }
                    Tok::Rest { .. } => s.push('z'),
                    Tok::Tuplet { .. } => unreachable!("tuplets don't nest"),
                }
            }
            s.push(')');
            s.push_str(&(span.ticks() / TICKS_PER_SIXTEENTH).to_string());
            if *tie {
                s.push('-');
            }
            return s;
        }
        Tok::Note { mark: Mark::Accent, .. } | Tok::Chord { mark: Mark::Accent, .. } => {
            s.push('>');
        }
        Tok::Note { mark: Mark::Ghost, .. } | Tok::Chord { mark: Mark::Ghost, .. } => {
            s.push('~');
        }
        _ => {}
    }
    let (dur, tie) = match tok {
        Tok::Note { pitch, dur, tie, .. } => {
            s.push_str(&pitch_to_abc_spelled(*pitch, flats));
            (*dur, *tie)
        }
        Tok::Chord { pitches, dur, tie, .. } => {
            s.push('[');
            for p in pitches {
                s.push_str(&pitch_to_abc_spelled(*p, flats));
            }
            s.push(']');
            (*dur, *tie)
        }
        Tok::Rest { dur } => {
            s.push('z');
            (*dur, false)
        }
        Tok::Tuplet { .. } => unreachable!(),
    };
    s.push_str(&dur_text(dur));
    if tie {
        s.push('-');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitch_roundtrip_full_range() {
        for pitch in 0u8..=127 {
            let abc = pitch_to_abc(pitch);
            let (parsed, rest) = parse_pitch(&abc).unwrap();
            assert_eq!(parsed, pitch, "abc {abc:?}");
            assert!(rest.is_empty());
        }
    }

    #[test]
    fn known_spellings() {
        assert_eq!(pitch_to_abc(60), "C");
        assert_eq!(pitch_to_abc(61), "^C");
        assert_eq!(pitch_to_abc(72), "c");
        assert_eq!(pitch_to_abc(45), "A,,");
        assert_eq!(pitch_to_abc(86), "d'");
        assert_eq!(parse_pitch("_B").unwrap().0, 70);
        assert_eq!(parse_pitch("=C").unwrap().0, 60);
    }

    fn cells(n: u32) -> MusicalTime {
        MusicalTime::from_sixteenths(n)
    }

    #[test]
    fn token_roundtrip() {
        for t in [
            Tok::Note { pitch: 45, dur: cells(4), tie: false, mark: Mark::None },
            Tok::Note { pitch: 61, dur: cells(1), tie: true, mark: Mark::None },
            Tok::Note { pitch: 69, dur: cells(2), tie: false, mark: Mark::Accent },
            Tok::Note { pitch: 40, dur: cells(8), tie: true, mark: Mark::Ghost },
            Tok::Chord { pitches: vec![60, 64, 67], dur: cells(16), tie: false, mark: Mark::None },
            Tok::Chord { pitches: vec![36, 42], dur: cells(2), tie: true, mark: Mark::Accent },
            Tok::Rest { dur: cells(3) },
            // Fractions of a cell: 32nd, dotted 16th, a triplet member.
            Tok::Note { pitch: 60, dur: MusicalTime(120), tie: false, mark: Mark::None },
            Tok::Note { pitch: 60, dur: MusicalTime(360), tie: true, mark: Mark::None },
            Tok::Rest { dur: MusicalTime(320) },
        ] {
            let s = emit_token(&t);
            assert_eq!(parse_token(&s).unwrap(), t, "via {s:?}");
        }
    }

    #[test]
    fn fraction_spellings() {
        assert_eq!(emit_token(&Tok::Rest { dur: MusicalTime(120) }), "z/2");
        assert_eq!(emit_token(&Tok::Rest { dur: MusicalTime(360) }), "z3/2");
        assert_eq!(emit_token(&Tok::Rest { dur: MusicalTime(320) }), "z4/3");
        assert_eq!(emit_token(&Tok::Rest { dur: MusicalTime(137) }), "z137/240");
        assert_eq!(parse_token("C/2").unwrap().dur(), MusicalTime(120));
        assert_eq!(parse_token("C3/2").unwrap().dur(), MusicalTime(360));
        assert_eq!(parse_token("[CE]/4-").unwrap().dur(), MusicalTime(60));
        // Not on the tick grid: 240 isn't divisible by 7.
        assert!(parse_token("C/7").is_err());
        assert!(parse_token("C0/2").is_err());
    }

    #[test]
    fn tuplets_parse_place_and_spell() {
        let toks = parse_tokens("(3 C D E)4 z12").unwrap();
        assert_eq!(toks.len(), 2);
        let Tok::Tuplet { n, members, span, tie } = &toks[0] else { panic!("{toks:?}") };
        assert_eq!((*n, *span, *tie), (3, cells(4), false));
        assert_eq!(members.len(), 3);
        assert_eq!(emit_token(&toks[0]), "(3 C D E)4");
        // Rests and marks as members; group tie.
        let toks = parse_tokens("(3 >C z [EG])2-").unwrap();
        assert_eq!(emit_token(&toks[0]), "(3 >C z [EG])2-");
        // Quintuplet over a beat: 192-tick members.
        let toks = parse_tokens("(5 C D E F G)4").unwrap();
        assert_eq!(toks[0].dur(), cells(4));
        // Inexact division is refused (septuplet needs the Document layer).
        assert!(parse_tokens("(7 C D E F G A B)4").is_err());
        // Member counts and member durations are strict.
        assert!(parse_tokens("(3 C D)4").is_err());
        assert!(parse_tokens("(3 C2 D E)4").is_err());
        assert!(parse_tokens("(3 C D E-)4").is_err());
        assert!(parse_tokens("(3 C D z)4-").is_err(), "cannot tie a rest");
        assert!(parse_tokens("(3 C D E").is_err(), "unclosed");
    }

    #[test]
    fn tuplet_detection_is_greedy_and_skips_plain_fractions() {
        // Three 320-tick tokens group; two do not; 32nds never group.
        let n = |d: i64| Tok::Note { pitch: 60, dur: MusicalTime(d), tie: false, mark: Mark::None };
        let out = detect_tuplets(vec![n(320), n(320), n(320)]);
        assert_eq!(out.len(), 1);
        assert!(matches!(&out[0], Tok::Tuplet { n: 3, .. }));
        let out = detect_tuplets(vec![n(320), n(320), n(240)]);
        assert_eq!(out.len(), 3, "incomplete run stays fractions");
        let out = detect_tuplets(vec![n(120), n(120)]);
        assert_eq!(out.len(), 2, "a run of 32nds is not a duplet");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_token("H4").is_err());
        assert!(parse_token("[CE").is_err());
        assert!(parse_token("z2-").is_err());
        assert!(parse_token("A0").is_err());
        assert!(parse_token("[]4").is_err());
        assert!(parse_token(">z2").is_err(), "rests carry no dynamics");
        assert!(parse_token("~z").is_err());
        assert!(parse_token("C/0").is_err());
        assert!(parse_token("C/961").is_err());
    }

    #[test]
    fn dynamics_mapping() {
        assert_eq!(vel_to_dynamic(96), ("f", 96));
        assert_eq!(vel_to_dynamic(70), ("mp", 64));
        assert_eq!(vel_to_dynamic(127), ("ff", 112));
        assert_eq!(vel_to_dynamic(1), ("pp", 32));
        assert_eq!(dynamic_to_vel("mf"), Some(80));
        assert_eq!(dynamic_to_vel("fff"), None);
        assert_eq!(apply_mark(96, Mark::Accent), 112);
        assert_eq!(apply_mark(96, Mark::Ghost), 72);
        assert_eq!(apply_mark(120, Mark::Accent), 127, "clamped");
        assert_eq!(mark_for_vel(112, 96), Mark::Accent);
        assert_eq!(mark_for_vel(72, 96), Mark::Ghost);
        assert_eq!(mark_for_vel(90, 96), Mark::None);
    }
}
