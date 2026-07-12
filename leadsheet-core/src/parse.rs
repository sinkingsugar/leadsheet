//! Text → QSong. The inverse of [`crate::emit`]; the renderer's front door.
//!
//! Three bar-body forms, matching the emitter:
//!
//! - Melodic: `P1 bass | A,,4 ^F2 z2 [CEG]4 |` (voices with `&`, ties `-`)
//! - Chordal: `P3 piano* | Am . F G7 |` — `*` marks chord mode; 1 column =
//!   1 beat, `.` holds, `z` rests; symbols render as canonical voicings.
//! - Drums: a `P2 drums` line followed by indented lanes `K |x...x...|`.
//!
//! Notes can also be placed directly with `b<n>` instead of `P<n>` +
//! arrangement (handy when writing by hand). Arrangement rows may carry a
//! label (`chorus: [P1+P2] x4`); labels are ignored. `[z]` is a silent bar.
//!
//! Tolerant of whitespace and of anything after the closing `|` (annotation
//! comments). Strict about what matters: unknown instruments or patterns,
//! bad tokens, and voices that don't sum to a full bar are hard errors
//! carrying a structured [`Diagnostic`] (code, line/col, message,
//! suggestion) precise enough for an LLM to repair the text from the
//! diagnostic alone.

use crate::chord;
use crate::drums;
use crate::error::{Diagnostic, Error};
use crate::grid::{MusicalTime, QNote, QSong, QTrack};
use crate::key::Key;
use crate::notation::{self, Mark, Tok};
use std::collections::HashMap;

/// Hard ceilings against pathological input — parse must stay panic-free
/// with bounded time and memory on garbage. No real song gets anywhere
/// close (100k bars ≈ two days of 4/4 at 120 BPM).
const MAX_BARS: u32 = 100_000;
const MAX_METER_NUMERATOR: u32 = 64;

/// A diagnostic under construction: everything except the location, which
/// the line loop fills in (finding `at` on the raw line for the column).
struct Raw {
    code: &'static str,
    msg: String,
    hint: Option<String>,
    at: Option<String>,
}

fn raw(code: &'static str, msg: impl Into<String>) -> Raw {
    Raw { code, msg: msg.into(), hint: None, at: None }
}

impl Raw {
    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }

    fn at(mut self, t: impl Into<String>) -> Self {
        self.at = Some(t.into());
        self
    }
}

/// Locate `r.at` on the raw line and produce the final error.
fn diag(lineno: usize, raw_line: &str, r: Raw) -> Error {
    let col = r.at.as_deref().and_then(|t| raw_line.find(t)).map(|i| i + 1).unwrap_or(0);
    Error::Parse(Diagnostic { code: r.code, line: lineno, col, message: r.msg, suggestion: r.hint })
}

const MUSIC_LINE_SHAPE: &str =
    "music lines look like `P1 name | tokens |` (pattern) or `b3 name | tokens |` (direct bar)";
const TOKEN_SHAPE: &str = "melodic tokens are a pitch (A-G/a-g with ^ _ = accidentals, , ' \
                           octaves), a cell count, and an optional `-` tie: ^F2, [CEG]4-, z8";

struct Header {
    name: String,
    bpm: f64,
    meter: (u32, u32),
    key: Option<Key>,
    swing: Option<crate::grid::Swing>,
}

enum RecBody {
    Melodic(String),
    Chordal(String),
    /// Lane cells: 0 empty, 1 ghost (`o`), 2 hit (`x`), 3 accent (`X`).
    Drums(Vec<(u8, Vec<u8>)>),
}

struct PatternRec {
    track: usize,
    /// Base velocity from the `@dyn` mark (default `f` = 96).
    base: u8,
    /// One entry per bar: patterns may span several bars
    /// (`P3 piano* | Am . . . | F . C . |`). Drum patterns are one bar.
    bars: Vec<RecBody>,
}

/// Split `name`, `name*`, `name@mf`, `name*@mf` into (name, chordal, base).
fn parse_inst_token(tok: &str) -> Result<(&str, bool, u8), Raw> {
    let (left, base) = match tok.split_once('@') {
        None => (tok, notation::DEFAULT_VEL),
        Some((left, dynamic)) => (
            left,
            notation::dynamic_to_vel(dynamic).ok_or_else(|| {
                raw("unknown-dynamic", format!("unknown dynamic {dynamic:?} in {tok:?}"))
                    .hint("dynamics are @pp @p @mp @mf @f @ff (soft to loud)")
                    .at(tok)
            })?,
        ),
    };
    match left.strip_suffix('*') {
        Some(name) => Ok((name, true, base)),
        None => Ok((left, false, base)),
    }
}

/// One parsed chord-line column.
enum ChordCol {
    Sym(Vec<u8>),
    Hold,
    Rest,
}

fn parse_chord_cols(content: &str, cpb: u32) -> Result<Vec<ChordCol>, Raw> {
    let beats = (cpb / 4) as usize;
    let cols: Vec<&str> = content.split_whitespace().collect();
    if cols.len() != beats {
        return Err(raw(
            "bar-length",
            format!("chord line has {} columns, expected {beats} (1 per beat)", cols.len()),
        )
        .hint("chord mode (`*`) takes exactly one symbol, `.` (hold) or `z` (rest) per beat"));
    }
    let mut out = Vec::with_capacity(beats);
    let mut have_chord = false;
    for col in cols {
        out.push(match col {
            "." => {
                if !have_chord {
                    return Err(raw("hold-across-bar", "`.` hold with no chord before it").hint(
                        "a hold cannot open a bar or follow a rest — restate the chord symbol \
                         (holds never cross barlines)",
                    ));
                }
                ChordCol::Hold
            }
            "z" => {
                have_chord = false;
                ChordCol::Rest
            }
            sym => {
                let sym_parsed = chord::parse_symbol(sym).map_err(|m| {
                    raw("bad-chord", m)
                        .hint(
                            "chord symbols look like C, Am7, F/A, G7(2); a voicing that has no \
                             exact name must be written as explicit pitches, e.g. [CEG]4",
                        )
                        .at(sym)
                })?;
                have_chord = true;
                ChordCol::Sym(chord::voicing(&sym_parsed).expect("parse_symbol validated the bass"))
            }
        });
    }
    Ok(out)
}

/// Where a pending drum-lane block will land once complete.
enum BlockTarget {
    Pattern(usize),
    Direct(u32), // bar number (1-based)
}

struct DrumBlock {
    track: usize,
    target: BlockTarget,
    /// Line the block opened on (for diagnostics at flush time).
    line: usize,
    /// Base velocity from the `@dyn` mark.
    base_vel: u8,
    /// Variant base: lanes not listed here are inherited from it.
    base: Option<usize>,
    lanes: Vec<(u8, Vec<u8>)>,
}

#[derive(Default)]
struct Builder {
    tracks: Vec<QTrack>,
    track_index: HashMap<String, usize>,
    /// Open ties per (track, pitch): (index into track notes, end tick so
    /// far). Several can be open at once (doubled pitches in a chord), so
    /// continuations match by end position.
    open_ties: HashMap<(usize, u8), Vec<(usize, MusicalTime)>>,
}

/// Diagnostics-friendly cell count for a tick position ("8", "7.5").
fn cells_display(t: MusicalTime) -> String {
    let cells = t.ticks() as f64 / MusicalTime::from_sixteenths(1).ticks() as f64;
    if cells.fract() == 0.0 { format!("{cells:.0}") } else { format!("{cells}") }
}

impl Builder {
    /// Place one note/chord (or tuplet member) at `onset`, joining an open
    /// tie that ends exactly there; register a new tie when asked.
    fn place(
        &mut self,
        ti: usize,
        onset: MusicalTime,
        dur: MusicalTime,
        pitches: &[u8],
        tie: bool,
        vel: u8,
    ) {
        for &pitch in pitches {
            let key = (ti, pitch);
            // Only a tie ending exactly at the onset is consumed — other
            // open ties on this pitch stay registered.
            let open = self.open_ties.get_mut(&key).and_then(|v| {
                v.iter().position(|&(_, end)| end == onset).map(|i| v.swap_remove(i).0)
            });
            let idx = match open {
                Some(idx) => {
                    self.tracks[ti].notes[idx].dur += dur;
                    idx
                }
                None => {
                    self.tracks[ti].notes.push(QNote { pitch, onset, dur, strokes: 1, vel });
                    self.tracks[ti].notes.len() - 1
                }
            };
            if tie {
                self.open_ties.entry(key).or_default().push((idx, onset + dur));
            }
        }
    }

    /// Place one melodic bar (`voice & voice`) at `bar_start` cells.
    fn apply_melodic(
        &mut self,
        ti: usize,
        bar_start: u32,
        cpb: u32,
        content: &str,
        base: u8,
    ) -> Result<(), Raw> {
        let bar_start = MusicalTime::from_sixteenths(bar_start);
        let bar_end = bar_start + MusicalTime::from_sixteenths(cpb);
        for voice in content.split('&') {
            let toks =
                notation::parse_tokens(voice).map_err(|m| raw("bad-token", m).hint(TOKEN_SHAPE))?;
            let mut cursor = bar_start;
            for tok in toks {
                let dur = tok.dur();
                if cursor.ticks().saturating_add(dur.ticks()) > bar_end.ticks() {
                    let spelled = notation::emit_token(&tok);
                    return Err(raw("bar-length", format!("bar overflows at token {spelled:?}"))
                        .hint(format!(
                            "durations in this bar sum past its {cpb} cells — shorten one, or move \
                         the overflow into the next bar with a tie (`-`)"
                        ))
                        .at(spelled));
                }
                match tok {
                    Tok::Rest { .. } => {}
                    Tok::Note { pitch, tie, mark, .. } => {
                        self.place(
                            ti,
                            cursor,
                            dur,
                            &[pitch],
                            tie,
                            notation::apply_mark(base, mark),
                        );
                    }
                    Tok::Chord { pitches, tie, mark, .. } => {
                        self.place(
                            ti,
                            cursor,
                            dur,
                            &pitches,
                            tie,
                            notation::apply_mark(base, mark),
                        );
                    }
                    Tok::Tuplet { n, members, span, tie } => {
                        // Member i spans [i*S/n, (i+1)*S/n) — exact by the
                        // divisibility check in parse_tokens.
                        let step = MusicalTime(span.ticks() / n as i64);
                        for (i, m) in members.iter().enumerate() {
                            let at = cursor + step * i as i64;
                            let last = i + 1 == members.len();
                            match m {
                                Tok::Rest { .. } => {}
                                Tok::Note { pitch, mark, .. } => self.place(
                                    ti,
                                    at,
                                    step,
                                    &[*pitch],
                                    tie && last,
                                    notation::apply_mark(base, *mark),
                                ),
                                Tok::Chord { pitches, mark, .. } => self.place(
                                    ti,
                                    at,
                                    step,
                                    pitches,
                                    tie && last,
                                    notation::apply_mark(base, *mark),
                                ),
                                Tok::Tuplet { .. } => unreachable!("tuplets don't nest"),
                            }
                        }
                    }
                }
                cursor += dur;
            }
            if cursor != bar_end && cursor != bar_start {
                return Err(raw(
                    "bar-length",
                    format!("voice covers {} of {cpb} cells", cells_display(cursor - bar_start)),
                )
                .hint(
                    "every `&` voice must fill the bar exactly — pad with rests (z<n>) or \
                       adjust durations",
                ));
            }
        }
        Ok(())
    }

    fn apply_chordal(
        &mut self,
        ti: usize,
        bar_start: u32,
        cpb: u32,
        content: &str,
        base: u8,
    ) -> Result<(), Raw> {
        let cols = parse_chord_cols(content, cpb)?;
        let mut current: Option<(Vec<u8>, u32, u32)> = None; // (pitches, start, dur)
        let flush = |cur: &mut Option<(Vec<u8>, u32, u32)>, tracks: &mut Vec<QTrack>| {
            if let Some((pitches, start, dur)) = cur.take() {
                for pitch in pitches {
                    tracks[ti].notes.push(QNote::from_cells(pitch, start, dur, base));
                }
            }
        };
        for (beat, col) in cols.into_iter().enumerate() {
            match col {
                ChordCol::Hold => {
                    if let Some(c) = current.as_mut() {
                        c.2 += 4;
                    }
                }
                ChordCol::Rest => flush(&mut current, &mut self.tracks),
                ChordCol::Sym(pitches) => {
                    flush(&mut current, &mut self.tracks);
                    current = Some((pitches, bar_start + beat as u32 * 4, 4));
                }
            }
        }
        flush(&mut current, &mut self.tracks);
        Ok(())
    }

    fn apply_drums(&mut self, ti: usize, bar_start: u32, lanes: &[(u8, Vec<u8>)], base: u8) {
        for (pitch, cells) in lanes {
            for (i, code) in cells.iter().enumerate() {
                let (vel, strokes) = match *code {
                    LANE_ACCENT => (notation::apply_mark(base, Mark::Accent), 1),
                    LANE_GHOST => (notation::apply_mark(base, Mark::Ghost), 1),
                    LANE_HIT => (base, 1),
                    LANE_D2 => (base, 2),
                    LANE_D3 => (base, 3),
                    LANE_D4 => (base, 4),
                    _ => continue,
                };
                // One-shot on the 16th grid; the digit is a stroke count.
                let mut hit = QNote::from_cells(*pitch, bar_start + i as u32, 1, vel);
                hit.strokes = strokes;
                self.tracks[ti].notes.push(hit);
            }
        }
    }

    fn apply(
        &mut self,
        ti: usize,
        bar_start: u32,
        cpb: u32,
        body: &RecBody,
        base: u8,
    ) -> Result<(), Raw> {
        match body {
            RecBody::Melodic(s) => self.apply_melodic(ti, bar_start, cpb, s, base),
            RecBody::Chordal(s) => self.apply_chordal(ti, bar_start, cpb, s, base),
            RecBody::Drums(lanes) => {
                self.apply_drums(ti, bar_start, lanes, base);
                Ok(())
            }
        }
    }
}

/// Drum lane cell codes (shared vocabulary with the emitter).
const LANE_EMPTY: u8 = 0;
const LANE_GHOST: u8 = 1;
const LANE_HIT: u8 = 2;
const LANE_ACCENT: u8 = 3;
const LANE_D2: u8 = 4;
const LANE_D3: u8 = 5;
const LANE_D4: u8 = 6;

/// Check melodic token syntax and bar-sum without placing notes.
fn validate_melodic(content: &str, cpb: u32) -> Result<(), Raw> {
    let bar = MusicalTime::from_sixteenths(cpb).ticks();
    for voice in content.split('&') {
        let toks =
            notation::parse_tokens(voice).map_err(|m| raw("bad-token", m).hint(TOKEN_SHAPE))?;
        // Saturating: hostile durations must not wrap the sum.
        let mut sum = 0i64;
        for tok in &toks {
            let dur = tok.dur().ticks();
            if sum.saturating_add(dur) > bar {
                let spelled = notation::emit_token(tok);
                return Err(raw(
                    "bar-length",
                    format!(
                        "bar overflows at token {spelled:?} ({} of {cpb} cells already used)",
                        cells_display(MusicalTime(sum))
                    ),
                )
                .hint(
                    "durations sum past the bar — shorten one, or carry the note into the next \
                     bar with a tie (`-`)",
                )
                .at(spelled));
            }
            sum = sum.saturating_add(dur);
        }
        if sum != bar && sum != 0 {
            return Err(raw(
                "bar-length",
                format!("voice covers {} of {cpb} cells", cells_display(MusicalTime(sum))),
            )
            .hint(
                "every `&` voice must fill the bar exactly — pad with rests (z<n>) or adjust \
                 durations",
            ));
        }
    }
    Ok(())
}

fn parse_kin(tok: &str) -> Result<usize, Raw> {
    tok.strip_prefix("~P").and_then(|n| n.parse().ok()).ok_or_else(|| {
        raw("bad-variant", format!("expected ~P<n>, got {tok:?}"))
            .hint("a variant marker names its base pattern: `P8 drums ~P3`")
            .at(tok)
    })
}

/// (prefix-token, instrument, chordal?, base vel, kin base, content).
type MusicLine<'a> = (&'a str, &'a str, bool, u8, Option<usize>, &'a str);

/// Split a `P1 bass | ... |` / `b3 piano*@mp | ... |` / `P9 piano ~P7 | ... |`
/// line into its [`MusicLine`] parts.
fn split_music_line(line: &str) -> Result<MusicLine<'_>, Raw> {
    let (prefix, rest) = line.split_once('|').ok_or_else(|| {
        raw("bad-line", format!("expected `| ... |` in {line:?}")).hint(MUSIC_LINE_SHAPE)
    })?;
    let content = match rest.rfind('|') {
        Some(i) => &rest[..i],
        None => return Err(raw("bad-line", "missing closing `|`").hint(MUSIC_LINE_SHAPE)),
    };
    let mut parts = prefix.split_whitespace();
    let head = parts
        .next()
        .ok_or_else(|| raw("bad-line", "missing P<n>/b<n> label").hint(MUSIC_LINE_SHAPE))?;
    let inst = parts
        .next()
        .ok_or_else(|| raw("bad-line", "missing instrument name").hint(MUSIC_LINE_SHAPE))?;
    let kin = match parts.next() {
        None => None,
        Some(tok) => Some(parse_kin(tok)?),
    };
    if let Some(junk) = parts.next() {
        return Err(raw("bad-line", format!("unexpected {junk:?} before `|`"))
            .hint(MUSIC_LINE_SHAPE)
            .at(junk));
    }
    let (inst, chordal, base) = parse_inst_token(inst)?;
    Ok((head, inst, chordal, base, kin, content))
}

/// A drum lane line: `K |x... x.x.|` (exactly one token before `|`).
fn try_lane_line(line: &str) -> Option<(u8, &str)> {
    let (label, content) = lane_shape(line)?;
    Some((drums::lane_pitch(label)?, content))
}

/// The shape of a lane line — one bare token, then `|cells|` — regardless
/// of whether the label is a known lane.
fn lane_shape(line: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = line.split_once('|')?;
    let mut toks = prefix.split_whitespace();
    let label = toks.next()?;
    if toks.next().is_some() {
        return None;
    }
    let content = &rest[..rest.rfind('|')?];
    Some((label, content))
}

fn parse_lane_cells(content: &str, cpb: u32) -> Result<Vec<u8>, Raw> {
    let mut cells = Vec::with_capacity(cpb as usize);
    for c in content.chars() {
        match c {
            'x' => cells.push(LANE_HIT),
            'X' => cells.push(LANE_ACCENT),
            'o' => cells.push(LANE_GHOST),
            '2' => cells.push(LANE_D2),
            '3' => cells.push(LANE_D3),
            '4' => cells.push(LANE_D4),
            '.' | '-' => cells.push(LANE_EMPTY),
            c if c.is_whitespace() => {}
            c => {
                return Err(raw("bad-lane-char", format!("bad lane char {c:?}")).hint(
                    "lane cells are `.` empty, `x` hit, `X` accent, `o` ghost, or `2`/`3`/`4` \
                     sub-strokes; spaces are cosmetic",
                ));
            }
        }
    }
    if cells.len() != cpb as usize {
        return Err(raw("bar-length", format!("lane has {} cells, expected {cpb}", cells.len()))
            .hint("one cell per 16th between the `|`s; spaces don't count"));
    }
    Ok(cells)
}

/// An arrangement row: `label: [P1+P2+z] x4` → (pattern ids, reps).
fn parse_row(line: &str) -> Result<(Vec<usize>, u32), Raw> {
    const ROW_SHAPE: &str = "arrangement rows look like `label: [P1+P2] x4` or `[z]`";
    let open = line
        .find('[')
        .ok_or_else(|| raw("bad-row", format!("expected `[` in row {line:?}")).hint(ROW_SHAPE))?;
    let label = line[..open].trim();
    if !label.is_empty() && !label.ends_with(':') {
        return Err(raw("bad-row", format!("bad row prefix {label:?}"))
            .hint("row labels end with `:`, e.g. `chorus: [P1] x4`")
            .at(label));
    }
    let rest = &line[open + 1..];
    let (inner, after) =
        rest.split_once(']').ok_or_else(|| raw("bad-row", "unclosed `[`").hint(ROW_SHAPE))?;
    let mut ids = Vec::new();
    for part in inner.split('+') {
        let part = part.trim();
        if part == "z" || part.is_empty() {
            continue;
        }
        let id: usize = part.strip_prefix('P').and_then(|n| n.parse().ok()).ok_or_else(|| {
            raw("bad-row", format!("bad pattern ref {part:?}"))
                .hint("stack entries are P<n> (or z for silence)")
                .at(part)
        })?;
        ids.push(id);
    }
    let reps = match after.split_whitespace().next() {
        None => 1,
        Some(x) => {
            x.strip_prefix('x').and_then(|n| n.parse::<u32>().ok()).filter(|n| *n >= 1).ok_or_else(
                || {
                    raw("bad-row", format!("bad repeat {x:?}"))
                        .hint("repeats are written x<n> with n >= 1, e.g. x4")
                        .at(x)
                },
            )?
        }
    };
    Ok((ids, reps))
}

/// `P<n>` / `b<n>` head token → block target, with bar-number limits.
fn parse_head(head: &str, max_bar: &mut u32) -> Result<BlockTarget, Raw> {
    if let Some(id) = head.strip_prefix('P').and_then(|n| n.parse().ok()) {
        return Ok(BlockTarget::Pattern(id));
    }
    if let Some(bar) = head.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()) {
        if bar == 0 {
            return Err(raw("bad-line", "bar numbers start at 1").at(head));
        }
        if bar > MAX_BARS {
            return Err(
                raw("too-large", format!("bar {bar} is beyond the {MAX_BARS}-bar limit")).at(head)
            );
        }
        *max_bar = (*max_bar).max(bar);
        return Ok(BlockTarget::Direct(bar));
    }
    Err(raw("bad-line", format!("expected P<n> or b<n>, got {head:?}"))
        .hint("patterns start with P<n>, direct bars with b<n> (n >= 1)")
        .at(head))
}

pub fn parse(text: &str) -> Result<QSong, Error> {
    let mut header: Option<Header> = None;
    let mut b = Builder::default();
    let mut patterns: HashMap<usize, PatternRec> = HashMap::new();
    let mut pending: Option<DrumBlock> = None;
    let mut next_bar = 0u32; // arrangement cursor
    let mut max_bar = 0u32; // from direct `b<n>` lines

    let known_patterns = |patterns: &HashMap<usize, PatternRec>| -> String {
        let mut ids: Vec<usize> = patterns.keys().copied().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            "no patterns are defined yet".into()
        } else {
            format!(
                "defined patterns: {}",
                ids.iter().map(|i| format!("P{i}")).collect::<Vec<_>>().join(" ")
            )
        }
    };

    // Flushing a completed drum block, shared by the loop and EOF.
    fn flush_block(
        block: DrumBlock,
        patterns: &mut HashMap<usize, PatternRec>,
        b: &mut Builder,
        cpb: u32,
    ) -> Result<(), Raw> {
        // Resolve a variant against its base: unlisted lanes are inherited,
        // listed ones replace (an all-dots lane clears a base lane).
        let lanes = match block.base {
            None => block.lanes,
            Some(base_id) => {
                let base = patterns.get(&base_id).ok_or_else(|| {
                    raw("unknown-pattern", format!("unknown variant base P{base_id}"))
                        .hint("the ~P base must be a drum pattern defined earlier in the file")
                })?;
                if base.track != block.track {
                    return Err(raw(
                        "bad-variant",
                        format!("variant base P{base_id} is a different instrument"),
                    )
                    .hint("a variant inherits lanes, so base and variant must share a kit"));
                }
                let [RecBody::Drums(base_lanes)] = base.bars.as_slice() else {
                    return Err(raw(
                        "bad-variant",
                        format!("variant base P{base_id} is not a drum pattern"),
                    )
                    .hint("drum lane inheritance needs a drum-pattern base"));
                };
                let mut merged = base_lanes.clone();
                for (pitch, cells) in block.lanes {
                    merged.retain(|(p, _)| *p != pitch);
                    merged.push((pitch, cells));
                }
                merged
            }
        };
        match block.target {
            BlockTarget::Pattern(id) => {
                let rec = PatternRec {
                    track: block.track,
                    base: block.base_vel,
                    bars: vec![RecBody::Drums(lanes)],
                };
                if patterns.insert(id, rec).is_some() {
                    return Err(raw("duplicate-pattern", format!("duplicate pattern P{id}"))
                        .hint("every P<n> must be unique — renumber this one"));
                }
            }
            BlockTarget::Direct(bar) => {
                b.apply(block.track, (bar - 1) * cpb, cpb, &RecBody::Drums(lanes), block.base_vel)?;
            }
        }
        Ok(())
    }

    for (lineno, raw_line) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let err = |r: Raw| diag(lineno, raw_line, r);
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let cpb = header.as_ref().map(|h| h.meter.0 * 16 / h.meter.1).unwrap_or(16);

        // Lane lines extend a pending drum block; anything else closes it.
        if let Some(block) = &mut pending {
            if let Some((pitch, content)) = try_lane_line(line) {
                let cells = parse_lane_cells(content, cpb).map_err(err)?;
                block.lanes.push((pitch, cells));
                continue;
            }
            // A lane-shaped line with an unknown label is almost certainly a
            // typo'd lane, not the next construct.
            if let Some((label, _)) = lane_shape(line)
                && !label.starts_with('P')
                && !label.starts_with('b')
            {
                return Err(err(raw("unknown-lane", format!("unknown drum lane {label:?}"))
                    .hint(
                        "lanes: K K2 S S2 st cp h hp O C C2 Cs Cn R R2 rb T1..T6 tm cb vs B1 B2 \
                     cg1 cg2 cg3, or d<key> for any GM percussion key",
                    )
                    .at(label)));
            }
            let block = pending.take().unwrap();
            let opened = block.line;
            flush_block(block, &mut patterns, &mut b, cpb).map_err(|r| diag(opened, "", r))?;
        }

        if let Some(rest) = line.strip_prefix('#') {
            parse_header_line(rest, &mut header, &mut b).map_err(err)?;
            continue;
        }
        if line == "arrangement:" {
            continue;
        }
        if header.is_none() {
            return Err(err(raw("missing-header", "content before `# song:` header").hint(
                "start the file with `# song: NAME  tempo: 120.00  meter: 4/4  grid: 1/16` and \
                 `# instruments: name:PROGRAM ...`",
            )));
        }

        // Arrangement row?
        let before_pipe = line.split('|').next().unwrap_or(line);
        if before_pipe.contains('[') {
            let (ids, reps) = parse_row(line).map_err(err)?;
            // Row unit = the longest pattern; 1-bar patterns repeat per bar,
            // longer ones must all agree on the unit length.
            let mut unit = 1u32;
            for id in &ids {
                let p = patterns.get(id).ok_or_else(|| {
                    err(raw("unknown-pattern", format!("unknown pattern P{id}"))
                        .hint(known_patterns(&patterns))
                        .at(format!("P{id}")))
                })?;
                unit = unit.max(p.bars.len() as u32);
            }
            for id in &ids {
                let len = patterns[id].bars.len() as u32;
                if len != 1 && len != unit {
                    return Err(err(raw(
                        "pattern-length",
                        format!("P{id} is {len} bars but the row unit is {unit}"),
                    )
                    .hint(
                        "in one stack, multi-bar patterns must all be the same length; 1-bar \
                         patterns repeat each bar of the unit",
                    )
                    .at(format!("P{id}"))));
                }
            }
            if next_bar as u64 + reps as u64 * unit as u64 > MAX_BARS as u64 {
                return Err(err(raw("too-large", format!("arrangement exceeds {MAX_BARS} bars"))));
            }
            for _ in 0..reps {
                for offset in 0..unit {
                    for id in &ids {
                        let p = &patterns[id];
                        let track = p.track;
                        let body =
                            if p.bars.len() == 1 { &p.bars[0] } else { &p.bars[offset as usize] };
                        b.apply(track, (next_bar + offset) * cpb, cpb, body, p.base)
                            .map_err(err)?;
                    }
                }
                next_bar += unit;
            }
            continue;
        }

        if !line.contains('|') {
            // Drum block opener: `P2 drums`, `b3 drums@p`, or `P8 drums ~P3`.
            let mut parts = line.split_whitespace();
            let (head, inst) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let base = match parts.next() {
                None => None,
                Some(tok) => Some(parse_kin(tok).map_err(err)?),
            };
            if parts.next().is_some() || inst.is_empty() {
                return Err(err(raw("bad-line", format!("cannot parse {line:?}")).hint(
                    "a drum block opens with `P<n> <kit-instrument>` or `b<n> <kit-instrument>`, \
                     with its lanes on the following lines",
                )));
            }
            let (inst, chordal, base_vel) = parse_inst_token(inst).map_err(err)?;
            if chordal {
                return Err(err(raw("bad-line", "drum patterns cannot be chord-mode (`*`)")));
            }
            let ti = *b.track_index.get(inst).ok_or_else(|| {
                err(raw("unknown-instrument", format!("unknown instrument {inst:?}"))
                    .hint(known_instruments(&b))
                    .at(inst))
            })?;
            if !b.tracks[ti].is_drums {
                return Err(err(raw("not-a-kit", format!("{inst:?} is not a drum kit"))
                    .hint(format!(
                        "{inst:?} is a melodic program; drum lanes need an instrument declared \
                         as `{inst}:kit`"
                    ))
                    .at(inst)));
            }
            let target = parse_head(head, &mut max_bar).map_err(err)?;
            pending = Some(DrumBlock {
                track: ti,
                target,
                line: lineno,
                base_vel,
                base,
                lanes: Vec::new(),
            });
            continue;
        }

        // Pattern definition or direct bar line. `content` may span several
        // bars separated by `|`.
        let (head, inst, chordal, base_vel, kin, content) = split_music_line(line).map_err(err)?;
        let ti = *b.track_index.get(inst).ok_or_else(|| {
            err(raw("unknown-instrument", format!("unknown instrument {inst:?}"))
                .hint(known_instruments(&b))
                .at(inst))
        })?;
        // Melodic/chordal kinship is informational; just sanity-check it.
        if let Some(base_id) = kin {
            let base = patterns.get(&base_id).ok_or_else(|| {
                err(raw("unknown-pattern", format!("unknown variant base P{base_id}"))
                    .hint(known_patterns(&patterns)))
            })?;
            if base.track != ti {
                return Err(err(raw(
                    "bad-variant",
                    format!("variant base P{base_id} is a different instrument"),
                )));
            }
        }
        let bars: Vec<RecBody> = content
            .split('|')
            .map(|seg| {
                if chordal {
                    parse_chord_cols(seg, cpb)?;
                    Ok(RecBody::Chordal(seg.trim().to_string()))
                } else {
                    validate_melodic(seg, cpb)?;
                    Ok(RecBody::Melodic(seg.trim().to_string()))
                }
            })
            .collect::<Result<_, Raw>>()
            .map_err(err)?;
        match parse_head(head, &mut max_bar).map_err(err)? {
            BlockTarget::Pattern(id) => {
                if patterns.insert(id, PatternRec { track: ti, base: base_vel, bars }).is_some() {
                    return Err(err(raw("duplicate-pattern", format!("duplicate pattern P{id}"))
                        .hint("every P<n> must be unique — renumber this one")
                        .at(head)));
                }
            }
            BlockTarget::Direct(bar) => {
                if bar as u64 + bars.len() as u64 - 1 > MAX_BARS as u64 {
                    return Err(err(raw(
                        "too-large",
                        format!("bars beyond the {MAX_BARS}-bar limit"),
                    )
                    .at(head)));
                }
                for (i, body) in bars.iter().enumerate() {
                    b.apply(ti, (bar - 1 + i as u32) * cpb, cpb, body, base_vel).map_err(err)?;
                }
                max_bar = max_bar.max(bar + bars.len() as u32 - 1);
            }
        }
    }

    let header = header.ok_or_else(|| {
        diag(
            0,
            "",
            raw("missing-header", "missing `# song:` header").hint(
                "start the file with `# song: NAME  tempo: 120.00  meter: 4/4  grid: 1/16` and \
                 `# instruments: name:PROGRAM ...`",
            ),
        )
    })?;
    let cpb = header.meter.0 * 16 / header.meter.1;
    if let Some(block) = pending.take() {
        let opened = block.line;
        flush_block(block, &mut patterns, &mut b, cpb).map_err(|r| diag(opened, "", r))?;
    }
    let mut max_end = MusicalTime::ZERO;
    for t in &mut b.tracks {
        t.notes.sort_by(|a, b| a.onset.cmp(&b.onset).then(a.pitch.cmp(&b.pitch)));
        for n in &t.notes {
            max_end = max_end.max(n.onset + n.dur);
        }
    }
    Ok(QSong {
        name: header.name,
        bpm: header.bpm,
        meter: header.meter,
        key: header.key,
        swing: header.swing,
        n_bars: max_end.spans_ceil(MusicalTime::from_sixteenths(cpb)).max(next_bar).max(max_bar),
        tracks: b.tracks,
    })
}

fn known_instruments(b: &Builder) -> String {
    if b.tracks.is_empty() {
        "no instruments are declared — add them to the `# instruments:` header line".into()
    } else {
        format!(
            "declared instruments: {}",
            b.tracks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(" ")
        )
    }
}

fn parse_header_line(rest: &str, header: &mut Option<Header>, b: &mut Builder) -> Result<(), Raw> {
    const HEADER_SHAPE: &str =
        "the song line is `# song: NAME  tempo: BPM  meter: N/D  key: K  grid: 1/16`";
    let rest = rest.trim();
    if let Some(fields) = rest.strip_prefix("song:") {
        // `song: NAME  tempo: T  meter: N/D  key: K  grid: 1/16` — the name
        // runs until the `tempo:` key (names may contain single spaces).
        let (name_part, after) = fields
            .split_once("tempo:")
            .ok_or_else(|| raw("bad-header", "header missing `tempo:`").hint(HEADER_SHAPE))?;
        let name = name_part.trim().to_string();
        let mut fields_map = HashMap::new();
        let mut it = after.split_whitespace();
        let bpm_tok = it.next().ok_or_else(|| raw("bad-tempo", "missing tempo value"))?;
        let bpm: f64 = bpm_tok
            .parse()
            .map_err(|_| raw("bad-tempo", format!("bad tempo {bpm_tok:?}")).at(bpm_tok))?;
        let mut pending: Option<&str> = None;
        while let Some(k) = pending.take().or_else(|| it.next()) {
            if k.ends_with('%') {
                // Second word of a two-word swing value.
                fields_map.insert("swing2", k);
                continue;
            }
            let v = it
                .next()
                .ok_or_else(|| raw("bad-header", format!("missing value for {k}")).at(k))?;
            if v.ends_with('%') && !k.trim_end_matches(':').eq("swing") {
                return Err(raw("bad-swing", format!("unexpected {v:?} after {k}"))
                    .hint("swing is `swing: 66%` or `swing: 16th 58%`")
                    .at(v));
            }
            fields_map.insert(k.trim_end_matches(':'), v);
            if v == "8th" || v == "16th" {
                // The percent follows as a bare token.
                if let Some(nxt) = it.next() {
                    pending = Some(nxt);
                }
            }
        }
        const METER_HINT: &str = "meter is N/D with D = 4 or 8, e.g. 4/4, 3/4, 6/8";
        let meter = match fields_map.get("meter") {
            None => (4, 4),
            Some(m) => {
                let (n, d) = m.split_once('/').ok_or_else(|| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                let n: u32 = n.parse().map_err(|_| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                let d: u32 = d.parse().map_err(|_| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                if d != 4 && d != 8 {
                    return Err(raw("bad-meter", format!("unsupported meter {m:?}"))
                        .hint(METER_HINT)
                        .at(*m));
                }
                if n == 0 || n > MAX_METER_NUMERATOR {
                    return Err(raw(
                        "bad-meter",
                        format!(
                            "meter numerator out of range in {m:?} (1..={MAX_METER_NUMERATOR})"
                        ),
                    )
                    .at(*m));
                }
                (n, d)
            }
        };
        let key = match fields_map.get("key") {
            None => None,
            Some(k) => Some(Key::parse(k).ok_or_else(|| {
                raw("bad-key", format!("bad key {k:?}"))
                    .hint("keys look like C, F#, Bb, Am, Ebm")
                    .at(*k)
            })?),
        };
        // `swing: 66%` (offbeat 8ths) or `swing: 16th 60%`. Because header
        // fields split on whitespace, the two-word form arrives as
        // swing->"16th" plus a stray percent token; recover it from the map.
        const SWING_HINT: &str =
            "swing is `swing: 66%` (offbeat 8ths) or `swing: 16th 58%`; range 50%..75%";
        let swing = match fields_map.get("swing") {
            None => None,
            Some(v) => {
                let (level, pct_str) = match *v {
                    "8th" | "16th" => {
                        let lvl = if *v == "16th" { 16 } else { 8 };
                        (
                            lvl,
                            *fields_map.get("swing2").ok_or_else(|| {
                                raw("bad-swing", "swing level without percent").hint(SWING_HINT)
                            })?,
                        )
                    }
                    other => (8u8, other),
                };
                let percent: u8 = pct_str
                    .strip_suffix('%')
                    .and_then(|n| n.parse().ok())
                    .filter(|p| (50..=75).contains(p))
                    .ok_or_else(|| {
                        raw("bad-swing", format!("bad swing {pct_str:?} (want 50%..75%)"))
                            .hint(SWING_HINT)
                            .at(pct_str)
                    })?;
                Some(crate::grid::Swing { level, percent })
            }
        };
        if let Some(g) = fields_map.get("grid")
            && *g != "1/16"
        {
            return Err(raw("bad-grid", format!("unsupported grid {g:?} (only 1/16)")).at(*g));
        }
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(raw("bad-tempo", format!("bad tempo {bpm}")).at(bpm_tok));
        }
        *header = Some(Header { name, bpm, meter, key, swing });
        return Ok(());
    }
    if let Some(fields) = rest.strip_prefix("instruments:") {
        const INST_HINT: &str = "instruments are `name:PROGRAM` (GM 0-127) or `name:kit`";
        for field in fields.split_whitespace() {
            let (name, prog) = field.split_once(':').ok_or_else(|| {
                raw("bad-instrument", format!("bad instrument {field:?}")).hint(INST_HINT).at(field)
            })?;
            let (program, is_drums) = if prog == "kit" {
                (0u8, true)
            } else {
                (
                    prog.parse::<u8>().ok().filter(|p| *p <= 127).ok_or_else(|| {
                        raw("bad-instrument", format!("bad program in {field:?}"))
                            .hint(INST_HINT)
                            .at(field)
                    })?,
                    false,
                )
            };
            if b.track_index.contains_key(name) {
                return Err(
                    raw("duplicate-instrument", format!("duplicate instrument {name:?}")).at(name)
                );
            }
            b.track_index.insert(name.to_string(), b.tracks.len());
            b.tracks.push(QTrack { name: name.to_string(), program, is_drums, notes: Vec::new() });
        }
        return Ok(());
    }
    // Any other `#` line is a comment.
    Ok(())
}
