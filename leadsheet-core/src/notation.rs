//! The note-token grammar shared by the emitter and the parser.
//!
//! ABC-style pitches with grid-cell durations:
//!
//! ```text
//! A,,4     A1 (two octaves below A4), 4 cells (a beat at 1/16 grid)
//! ^F2      F#4, 2 cells
//! e        E5, 1 cell (duration 1 is implied)
//! z2       rest, 2 cells
//! [CEG]4   C major triad voiced C4 E4 G4, 4 cells
//! C2-      C4, 2 cells, tied into the next bar
//! ```
//!
//! Deliberate divergence from real ABC: accidentals are always explicit on
//! the note (`^`/`_`), never implied by a key signature and never sticky
//! within a bar. An LLM (or a human) can read any token in isolation.
//! Case marks the octave as in ABC: `C` = C4 (middle C), `c` = C5,
//! `,` lowers and `'` raises by an octave.

/// One parsed bar token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Note { pitch: u8, dur: u32, tie: bool },
    Chord { pitches: Vec<u8>, dur: u32, tie: bool },
    Rest { dur: u32 },
}

impl Tok {
    pub fn dur(&self) -> u32 {
        match self {
            Tok::Note { dur, .. } | Tok::Chord { dur, .. } | Tok::Rest { dur } => *dur,
        }
    }
}

/// Sharps-only spelling for now; key-aware spelling arrives with Layer 2.
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

pub fn pitch_to_abc(pitch: u8) -> String {
    let (letter, acc) = SHARP_NAMES[(pitch % 12) as usize];
    let octave = (pitch / 12) as i32 - 1; // MIDI 60 = C4
    let mut s = String::new();
    if acc == 1 {
        s.push('^');
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

fn parse_dur_tie(s: &str, tok: &str) -> Result<(u32, bool), String> {
    let (dur_str, tie) = match s.strip_suffix('-') {
        Some(rest) => (rest, true),
        None => (s, false),
    };
    let dur = if dur_str.is_empty() {
        1
    } else {
        dur_str
            .parse::<u32>()
            .ok()
            .filter(|d| *d >= 1)
            .ok_or_else(|| format!("bad duration {dur_str:?} in token {tok:?}"))?
    };
    Ok((dur, tie))
}

/// Parse one whitespace-delimited bar token.
pub fn parse_token(tok: &str) -> Result<Tok, String> {
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
        return Ok(Tok::Chord { pitches, dur, tie });
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
    Ok(Tok::Note { pitch, dur, tie })
}

pub fn emit_token(tok: &Tok) -> String {
    let mut s = String::new();
    let (dur, tie) = match tok {
        Tok::Note { pitch, dur, tie } => {
            s.push_str(&pitch_to_abc(*pitch));
            (*dur, *tie)
        }
        Tok::Chord { pitches, dur, tie } => {
            s.push('[');
            for p in pitches {
                s.push_str(&pitch_to_abc(*p));
            }
            s.push(']');
            (*dur, *tie)
        }
        Tok::Rest { dur } => {
            s.push('z');
            (*dur, false)
        }
    };
    if dur != 1 {
        s.push_str(&dur.to_string());
    }
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

    #[test]
    fn token_roundtrip() {
        for t in [
            Tok::Note { pitch: 45, dur: 4, tie: false },
            Tok::Note { pitch: 61, dur: 1, tie: true },
            Tok::Chord { pitches: vec![60, 64, 67], dur: 16, tie: false },
            Tok::Chord { pitches: vec![36, 42], dur: 2, tie: true },
            Tok::Rest { dur: 3 },
        ] {
            let s = emit_token(&t);
            assert_eq!(parse_token(&s).unwrap(), t, "via {s:?}");
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_token("H4").is_err());
        assert!(parse_token("[CE").is_err());
        assert!(parse_token("z2-").is_err());
        assert!(parse_token("A0").is_err());
        assert!(parse_token("[]4").is_err());
    }
}
