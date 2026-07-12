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
//! bad tokens, and voices that don't sum to a full bar are hard errors with
//! line numbers.

use crate::chord;
use crate::drums;
use crate::error::Error;
use crate::grid::{QNote, QSong, QTrack};
use crate::key::Key;
use crate::notation::{Tok, parse_token};
use std::collections::HashMap;

const DEFAULT_VEL: u8 = 96;

struct Header {
    name: String,
    bpm: f64,
    meter: (u32, u32),
    key: Option<Key>,
}

enum RecBody {
    Melodic(String),
    Chordal(String),
    Drums(Vec<(u8, Vec<bool>)>),
}

struct PatternRec {
    track: usize,
    /// One entry per bar: patterns may span several bars
    /// (`P3 piano* | Am . . . | F . C . |`). Drum patterns are one bar.
    bars: Vec<RecBody>,
}

/// One parsed chord-line column.
enum ChordCol {
    Sym(Vec<u8>),
    Hold,
    Rest,
}

fn parse_chord_cols(content: &str, cpb: u32) -> Result<Vec<ChordCol>, String> {
    let beats = (cpb / 4) as usize;
    let cols: Vec<&str> = content.split_whitespace().collect();
    if cols.len() != beats {
        return Err(format!("chord line has {} columns, expected {beats} (1 per beat)", cols.len()));
    }
    let mut out = Vec::with_capacity(beats);
    let mut have_chord = false;
    for col in cols {
        out.push(match col {
            "." => {
                if !have_chord {
                    return Err("`.` hold with no chord before it".into());
                }
                ChordCol::Hold
            }
            "z" => {
                have_chord = false;
                ChordCol::Rest
            }
            sym => {
                let sym = chord::parse_symbol(sym)?;
                have_chord = true;
                ChordCol::Sym(chord::voicing(&sym).expect("parse_symbol validated the bass"))
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
    /// Variant base: lanes not listed here are inherited from it.
    base: Option<usize>,
    lanes: Vec<(u8, Vec<bool>)>,
}

#[derive(Default)]
struct Builder {
    tracks: Vec<QTrack>,
    track_index: HashMap<String, usize>,
    /// Open ties per (track, pitch): (index into track notes, end cell so far).
    open_ties: HashMap<(usize, u8), (usize, u32)>,
}

impl Builder {
    /// Place one melodic bar (`voice & voice`) at `bar_start` cells.
    fn apply_melodic(
        &mut self,
        ti: usize,
        bar_start: u32,
        cpb: u32,
        content: &str,
    ) -> Result<(), String> {
        for voice in content.split('&') {
            let mut cursor = bar_start;
            for tok_str in voice.split_whitespace() {
                let tok = parse_token(tok_str)?;
                let dur = tok.dur();
                if cursor + dur > bar_start + cpb {
                    return Err(format!("bar overflows at token {tok_str:?}"));
                }
                let (pitches, tie): (Vec<u8>, bool) = match tok {
                    Tok::Rest { .. } => (vec![], false),
                    Tok::Note { pitch, tie, .. } => (vec![pitch], tie),
                    Tok::Chord { pitches, tie, .. } => (pitches, tie),
                };
                for pitch in pitches {
                    let key = (ti, pitch);
                    // Continuation of a tied note joins it; else a new note.
                    let idx = match self.open_ties.remove(&key) {
                        Some((idx, end)) if end == cursor => {
                            self.tracks[ti].notes[idx].dur_cells += dur;
                            idx
                        }
                        _ => {
                            self.tracks[ti].notes.push(QNote {
                                pitch,
                                cell: cursor,
                                dur_cells: dur,
                                vel: DEFAULT_VEL,
                            });
                            self.tracks[ti].notes.len() - 1
                        }
                    };
                    if tie {
                        self.open_ties.insert(key, (idx, cursor + dur));
                    }
                }
                cursor += dur;
            }
            if cursor != bar_start + cpb && cursor != bar_start {
                return Err(format!("voice covers {} of {cpb} cells", cursor - bar_start));
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
    ) -> Result<(), String> {
        let cols = parse_chord_cols(content, cpb)?;
        let mut current: Option<(Vec<u8>, u32, u32)> = None; // (pitches, start, dur)
        let flush = |cur: &mut Option<(Vec<u8>, u32, u32)>, tracks: &mut Vec<QTrack>| {
            if let Some((pitches, start, dur)) = cur.take() {
                for pitch in pitches {
                    tracks[ti].notes.push(QNote {
                        pitch,
                        cell: start,
                        dur_cells: dur,
                        vel: DEFAULT_VEL,
                    });
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

    fn apply_drums(&mut self, ti: usize, bar_start: u32, lanes: &[(u8, Vec<bool>)]) {
        for (pitch, cells) in lanes {
            for (i, hit) in cells.iter().enumerate() {
                if *hit {
                    self.tracks[ti].notes.push(QNote {
                        pitch: *pitch,
                        cell: bar_start + i as u32,
                        dur_cells: 1,
                        vel: DEFAULT_VEL,
                    });
                }
            }
        }
    }

    fn apply(
        &mut self,
        ti: usize,
        bar_start: u32,
        cpb: u32,
        body: &RecBody,
    ) -> Result<(), String> {
        match body {
            RecBody::Melodic(s) => self.apply_melodic(ti, bar_start, cpb, s),
            RecBody::Chordal(s) => self.apply_chordal(ti, bar_start, cpb, s),
            RecBody::Drums(lanes) => {
                self.apply_drums(ti, bar_start, lanes);
                Ok(())
            }
        }
    }
}

/// Check melodic token syntax and bar-sum without placing notes.
fn validate_melodic(content: &str, cpb: u32) -> Result<(), String> {
    for voice in content.split('&') {
        let mut sum = 0u32;
        for tok_str in voice.split_whitespace() {
            sum += parse_token(tok_str)?.dur();
        }
        if sum != cpb && sum != 0 {
            return Err(format!("voice covers {sum} of {cpb} cells"));
        }
    }
    Ok(())
}

fn parse_kin(tok: &str) -> Result<usize, String> {
    tok.strip_prefix("~P")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("expected ~P<n>, got {tok:?}"))
}

/// Split a `P1 bass | ... |` / `b3 piano* | ... |` / `P9 piano ~P7 | ... |`
/// line into (prefix-token, instrument, chordal?, kin base, content).
fn split_music_line(line: &str) -> Result<(&str, &str, bool, Option<usize>, &str), String> {
    let (prefix, rest) =
        line.split_once('|').ok_or_else(|| format!("expected `| ... |` in {line:?}"))?;
    let content = match rest.rfind('|') {
        Some(i) => &rest[..i],
        None => return Err("missing closing `|`".into()),
    };
    let mut parts = prefix.split_whitespace();
    let head = parts.next().ok_or("missing P/b label")?;
    let inst = parts.next().ok_or("missing instrument")?;
    let kin = match parts.next() {
        None => None,
        Some(tok) => Some(parse_kin(tok)?),
    };
    if let Some(junk) = parts.next() {
        return Err(format!("unexpected {junk:?} before `|`"));
    }
    let (inst, chordal) = match inst.strip_suffix('*') {
        Some(base) => (base, true),
        None => (inst, false),
    };
    Ok((head, inst, chordal, kin, content))
}

/// A drum lane line: `K |x... x.x.|` (exactly one token before `|`).
fn try_lane_line(line: &str) -> Option<(u8, &str)> {
    let (prefix, rest) = line.split_once('|')?;
    let mut toks = prefix.split_whitespace();
    let label = toks.next()?;
    if toks.next().is_some() {
        return None;
    }
    let pitch = drums::lane_pitch(label)?;
    let content = &rest[..rest.rfind('|')?];
    Some((pitch, content))
}

fn parse_lane_cells(content: &str, cpb: u32) -> Result<Vec<bool>, String> {
    let mut cells = Vec::with_capacity(cpb as usize);
    for c in content.chars() {
        match c {
            'x' | 'X' => cells.push(true),
            '.' | '-' => cells.push(false),
            c if c.is_whitespace() => {}
            c => return Err(format!("bad lane char {c:?}")),
        }
    }
    if cells.len() != cpb as usize {
        return Err(format!("lane has {} cells, expected {cpb}", cells.len()));
    }
    Ok(cells)
}

/// An arrangement row: `label: [P1+P2+z] x4` → (pattern ids, reps).
fn parse_row(line: &str) -> Result<(Vec<usize>, u32), String> {
    let open = line.find('[').ok_or_else(|| format!("expected `[` in row {line:?}"))?;
    let label = line[..open].trim();
    if !label.is_empty() && !label.ends_with(':') {
        return Err(format!("bad row prefix {label:?} (labels end with `:`)"));
    }
    let rest = &line[open + 1..];
    let (inner, after) = rest.split_once(']').ok_or("unclosed `[`")?;
    let mut ids = Vec::new();
    for part in inner.split('+') {
        let part = part.trim();
        if part == "z" || part.is_empty() {
            continue;
        }
        let id: usize = part
            .strip_prefix('P')
            .and_then(|n| n.parse().ok())
            .ok_or_else(|| format!("bad pattern ref {part:?}"))?;
        ids.push(id);
    }
    let reps = match after.split_whitespace().next() {
        None => 1,
        Some(x) => x
            .strip_prefix('x')
            .and_then(|n| n.parse::<u32>().ok())
            .filter(|n| *n >= 1)
            .ok_or_else(|| format!("bad repeat {x:?} (expected xN)"))?,
    };
    Ok((ids, reps))
}

pub fn parse(text: &str) -> Result<QSong, Error> {
    let err = |line: usize, msg: String| Error::Parse(format!("line {line}: {msg}"));

    let mut header: Option<Header> = None;
    let mut b = Builder::default();
    let mut patterns: HashMap<usize, PatternRec> = HashMap::new();
    let mut pending: Option<DrumBlock> = None;
    let mut next_bar = 0u32; // arrangement cursor
    let mut max_bar = 0u32; // from direct `b<n>` lines

    // Flushing a completed drum block, shared by the loop and EOF.
    fn flush_block(
        block: DrumBlock,
        patterns: &mut HashMap<usize, PatternRec>,
        b: &mut Builder,
        cpb: u32,
    ) -> Result<(), String> {
        // Resolve a variant against its base: unlisted lanes are inherited,
        // listed ones replace (an all-dots lane clears a base lane).
        let lanes = match block.base {
            None => block.lanes,
            Some(base_id) => {
                let base = patterns
                    .get(&base_id)
                    .ok_or_else(|| format!("unknown variant base P{base_id}"))?;
                if base.track != block.track {
                    return Err(format!("variant base P{base_id} is a different instrument"));
                }
                let [RecBody::Drums(base_lanes)] = base.bars.as_slice() else {
                    return Err(format!("variant base P{base_id} is not a drum pattern"));
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
                let rec = PatternRec { track: block.track, bars: vec![RecBody::Drums(lanes)] };
                if patterns.insert(id, rec).is_some() {
                    return Err(format!("duplicate pattern P{id}"));
                }
            }
            BlockTarget::Direct(bar) => {
                b.apply(block.track, (bar - 1) * cpb, cpb, &RecBody::Drums(lanes))?;
            }
        }
        Ok(())
    }

    for (lineno, raw) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        let cpb = header
            .as_ref()
            .map(|h| h.meter.0 * 16 / h.meter.1)
            .unwrap_or(16);

        // Lane lines extend a pending drum block; anything else closes it.
        if pending.is_some() {
            if let Some((pitch, content)) = try_lane_line(line) {
                let cells = parse_lane_cells(content, cpb).map_err(|m| err(lineno, m))?;
                pending.as_mut().unwrap().lanes.push((pitch, cells));
                continue;
            }
            let block = pending.take().unwrap();
            flush_block(block, &mut patterns, &mut b, cpb).map_err(|m| err(lineno, m))?;
        }

        if let Some(rest) = line.strip_prefix('#') {
            parse_header_line(rest, &mut header, &mut b).map_err(|m| err(lineno, m))?;
            continue;
        }
        if line == "arrangement:" {
            continue;
        }
        if header.is_none() {
            return Err(err(lineno, "content before `# song:` header".into()));
        }

        // Arrangement row?
        let before_pipe = line.split('|').next().unwrap_or(line);
        if before_pipe.contains('[') {
            let (ids, reps) = parse_row(line).map_err(|m| err(lineno, m))?;
            // Row unit = the longest pattern; 1-bar patterns repeat per bar,
            // longer ones must all agree on the unit length.
            let mut unit = 1u32;
            for id in &ids {
                let p = patterns
                    .get(id)
                    .ok_or_else(|| err(lineno, format!("unknown pattern P{id}")))?;
                unit = unit.max(p.bars.len() as u32);
            }
            for id in &ids {
                let len = patterns[id].bars.len() as u32;
                if len != 1 && len != unit {
                    return Err(err(
                        lineno,
                        format!("P{id} is {len} bars but the row unit is {unit}"),
                    ));
                }
            }
            for _ in 0..reps {
                for offset in 0..unit {
                    for id in &ids {
                        let p = &patterns[id];
                        let track = p.track;
                        let body = if p.bars.len() == 1 { &p.bars[0] } else { &p.bars[offset as usize] };
                        b.apply(track, (next_bar + offset) * cpb, cpb, body)
                            .map_err(|m| err(lineno, m))?;
                    }
                }
                next_bar += unit;
            }
            continue;
        }

        if !line.contains('|') {
            // Drum block opener: `P2 drums`, `b3 drums`, or `P8 drums ~P3`.
            let mut parts = line.split_whitespace();
            let (head, inst) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let base = match parts.next() {
                None => None,
                Some(tok) => Some(parse_kin(tok).map_err(|m| err(lineno, m))?),
            };
            if parts.next().is_some() || inst.is_empty() {
                return Err(err(lineno, format!("cannot parse {line:?}")));
            }
            let ti = *b
                .track_index
                .get(inst)
                .ok_or_else(|| err(lineno, format!("unknown instrument {inst:?}")))?;
            if !b.tracks[ti].is_drums {
                return Err(err(lineno, format!("{inst:?} is not a drum kit")));
            }
            let target = if let Some(id) = head.strip_prefix('P').and_then(|n| n.parse().ok()) {
                BlockTarget::Pattern(id)
            } else if let Some(bar) =
                head.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()).filter(|n| *n >= 1)
            {
                max_bar = max_bar.max(bar);
                BlockTarget::Direct(bar)
            } else {
                return Err(err(lineno, format!("expected P<n> or b<n>, got {head:?}")));
            };
            pending = Some(DrumBlock { track: ti, target, base, lanes: Vec::new() });
            continue;
        }

        // Pattern definition or direct bar line. `content` may span several
        // bars separated by `|`.
        let (head, inst, chordal, kin, content) =
            split_music_line(line).map_err(|m| err(lineno, m))?;
        let ti = *b
            .track_index
            .get(inst)
            .ok_or_else(|| err(lineno, format!("unknown instrument {inst:?}")))?;
        // Melodic/chordal kinship is informational; just sanity-check it.
        if let Some(base_id) = kin {
            let base = patterns
                .get(&base_id)
                .ok_or_else(|| err(lineno, format!("unknown variant base P{base_id}")))?;
            if base.track != ti {
                return Err(err(lineno, format!("variant base P{base_id} is a different instrument")));
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
            .collect::<Result<_, String>>()
            .map_err(|m| err(lineno, m))?;
        if let Some(id) = head.strip_prefix('P').and_then(|n| n.parse::<usize>().ok()) {
            if patterns.insert(id, PatternRec { track: ti, bars }).is_some() {
                return Err(err(lineno, format!("duplicate pattern P{id}")));
            }
        } else if let Some(bar) =
            head.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()).filter(|n| *n >= 1)
        {
            for (i, body) in bars.iter().enumerate() {
                b.apply(ti, (bar - 1 + i as u32) * cpb, cpb, body).map_err(|m| err(lineno, m))?;
            }
            max_bar = max_bar.max(bar + bars.len() as u32 - 1);
        } else {
            return Err(err(lineno, format!("expected P<n> or b<n>, got {head:?}")));
        }
    }

    let header = header.ok_or_else(|| Error::Parse("missing `# song:` header".into()))?;
    let cpb = header.meter.0 * 16 / header.meter.1;
    if let Some(block) = pending.take() {
        flush_block(block, &mut patterns, &mut b, cpb)
            .map_err(|m| Error::Parse(format!("end of file: {m}")))?;
    }
    let mut max_end = 0u32;
    for t in &mut b.tracks {
        t.notes.sort_by(|a, b| a.cell.cmp(&b.cell).then(a.pitch.cmp(&b.pitch)));
        for n in &t.notes {
            max_end = max_end.max(n.cell + n.dur_cells);
        }
    }
    Ok(QSong {
        name: header.name,
        bpm: header.bpm,
        meter: header.meter,
        key: header.key,
        n_bars: max_end.div_ceil(cpb).max(next_bar).max(max_bar),
        tracks: b.tracks,
    })
}

fn parse_header_line(
    rest: &str,
    header: &mut Option<Header>,
    b: &mut Builder,
) -> Result<(), String> {
    let rest = rest.trim();
    if let Some(fields) = rest.strip_prefix("song:") {
        // `song: NAME  tempo: T  meter: N/D  key: K  grid: 1/16` — the name
        // runs until the `tempo:` key (names may contain single spaces).
        let (name_part, after) = fields.split_once("tempo:").ok_or("header missing `tempo:`")?;
        let name = name_part.trim().to_string();
        let mut fields_map = HashMap::new();
        let mut it = after.split_whitespace();
        let bpm: f64 = it.next().ok_or("missing tempo value")?.parse().map_err(|_| "bad tempo")?;
        while let (Some(k), v) = (it.next(), it.next()) {
            fields_map.insert(k.trim_end_matches(':'), v.ok_or(format!("missing value for {k}"))?);
        }
        let meter = match fields_map.get("meter") {
            None => (4, 4),
            Some(m) => {
                let (n, d) = m.split_once('/').ok_or(format!("bad meter {m:?}"))?;
                let n: u32 = n.parse().map_err(|_| "bad meter")?;
                let d: u32 = d.parse().map_err(|_| "bad meter")?;
                if d != 4 && d != 8 {
                    return Err(format!("unsupported meter {m:?}"));
                }
                (n, d)
            }
        };
        let key = match fields_map.get("key") {
            None => None,
            Some(k) => Some(Key::parse(k).ok_or(format!("bad key {k:?}"))?),
        };
        if let Some(g) = fields_map.get("grid")
            && *g != "1/16"
        {
            return Err(format!("unsupported grid {g:?} (only 1/16)"));
        }
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(format!("bad tempo {bpm}"));
        }
        *header = Some(Header { name, bpm, meter, key });
        return Ok(());
    }
    if let Some(fields) = rest.strip_prefix("instruments:") {
        for field in fields.split_whitespace() {
            let (name, prog) = field.split_once(':').ok_or(format!("bad instrument {field:?}"))?;
            let (program, is_drums) = if prog == "kit" {
                (0u8, true)
            } else {
                (
                    prog.parse::<u8>()
                        .ok()
                        .filter(|p| *p <= 127)
                        .ok_or(format!("bad program in {field:?}"))?,
                    false,
                )
            };
            if b.track_index.contains_key(name) {
                return Err(format!("duplicate instrument {name:?}"));
            }
            b.track_index.insert(name.to_string(), b.tracks.len());
            b.tracks.push(QTrack { name: name.to_string(), program, is_drums, notes: Vec::new() });
        }
        return Ok(());
    }
    // Any other `#` line is a comment.
    Ok(())
}
