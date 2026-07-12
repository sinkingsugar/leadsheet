//! Text → QSong. The inverse of [`crate::emit`]; the renderer's front door.
//!
//! Two ways to place notes, freely mixable:
//!
//! - Pattern definitions + arrangement (what the emitter writes):
//!   `P1 bass | ... |` then `arrangement:` rows like `[P1+P2] x4`.
//!   Rows may carry a label (`chorus: [P1+P2] x4`); labels are ignored.
//!   `[z]` is a silent bar.
//! - Direct bars (handy when writing by hand): `b3 bass | ... |` puts
//!   content straight into bar 3.
//!
//! Tolerant of whitespace and of anything after the closing `|` (annotation
//! comments). Strict about what matters: unknown instruments or patterns,
//! bad tokens, and voices that don't sum to a full bar are hard errors with
//! line numbers.

use crate::error::Error;
use crate::grid::{QNote, QSong, QTrack};
use crate::notation::{Tok, parse_token};
use std::collections::HashMap;

const DEFAULT_VEL: u8 = 96;

struct Header {
    name: String,
    bpm: f64,
    meter: (u32, u32),
}

struct PatternRec {
    track: usize,
    body: String,
}

#[derive(Default)]
struct Builder {
    tracks: Vec<QTrack>,
    track_index: HashMap<String, usize>,
    /// Open ties per (track, pitch): (index into track notes, end cell so far).
    open_ties: HashMap<(usize, u8), (usize, u32)>,
}

impl Builder {
    /// Place one bar's content (`voice & voice`) at `bar_start` cells.
    fn apply(&mut self, ti: usize, bar_start: u32, cpb: u32, content: &str) -> Result<(), String> {
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
}

/// Check token syntax and bar-sum without placing notes (pattern defs are
/// validated eagerly, even if the arrangement never uses them).
fn validate_content(content: &str, cpb: u32) -> Result<(), String> {
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

/// Split a `P1 bass | ... |` / `b3 bass | ... |` line into
/// (prefix-token, instrument, content).
fn split_music_line(line: &str) -> Result<(&str, &str, &str), String> {
    let (prefix, rest) =
        line.split_once('|').ok_or_else(|| format!("expected `| ... |` in {line:?}"))?;
    let content = match rest.rfind('|') {
        Some(i) => &rest[..i],
        None => return Err("missing closing `|`".into()),
    };
    let mut parts = prefix.split_whitespace();
    let head = parts.next().ok_or("missing P/b label")?;
    let inst = parts.next().ok_or("missing instrument")?;
    if let Some(junk) = parts.next() {
        return Err(format!("unexpected {junk:?} before `|`"));
    }
    Ok((head, inst, content))
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
    let mut next_bar = 0u32; // arrangement cursor
    let mut max_bar = 0u32; // from direct `b<n>` lines

    for (lineno, raw) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            parse_header_line(rest, &mut header, &mut b).map_err(|m| err(lineno, m))?;
            continue;
        }
        if line == "arrangement:" {
            continue;
        }

        let header_ref =
            header.as_ref().ok_or_else(|| err(lineno, "content before `# song:` header".into()))?;
        let cpb = header_ref.meter.0 * 16 / header_ref.meter.1;

        // Arrangement row?
        let before_pipe = line.split('|').next().unwrap_or(line);
        if before_pipe.contains('[') {
            let (ids, reps) = parse_row(line).map_err(|m| err(lineno, m))?;
            for _ in 0..reps {
                for id in &ids {
                    let p = patterns
                        .get(id)
                        .ok_or_else(|| err(lineno, format!("unknown pattern P{id}")))?;
                    // Borrow dance: apply needs &mut b, body is owned per use.
                    let (track, body) = (p.track, p.body.clone());
                    b.apply(track, next_bar * cpb, cpb, &body).map_err(|m| err(lineno, m))?;
                }
                next_bar += 1;
            }
            continue;
        }

        // Pattern definition or direct bar line.
        let (head, inst, content) = split_music_line(line).map_err(|m| err(lineno, m))?;
        let ti = *b
            .track_index
            .get(inst)
            .ok_or_else(|| err(lineno, format!("unknown instrument {inst:?}")))?;
        if let Some(id) = head.strip_prefix('P').and_then(|n| n.parse::<usize>().ok()) {
            validate_content(content, cpb).map_err(|m| err(lineno, m))?;
            if patterns.insert(id, PatternRec { track: ti, body: content.trim().to_string() })
                .is_some()
            {
                return Err(err(lineno, format!("duplicate pattern P{id}")));
            }
        } else if let Some(bar) =
            head.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()).filter(|n| *n >= 1)
        {
            b.apply(ti, (bar - 1) * cpb, cpb, content).map_err(|m| err(lineno, m))?;
            max_bar = max_bar.max(bar);
        } else {
            return Err(err(lineno, format!("expected P<n> or b<n>, got {head:?}")));
        }
    }

    let header = header.ok_or_else(|| Error::Parse("missing `# song:` header".into()))?;
    let cpb = header.meter.0 * 16 / header.meter.1;
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
        // `song: NAME  tempo: T  meter: N/D  grid: 1/16` — the name runs
        // until the `tempo:` key (names may contain single spaces).
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
        if let Some(g) = fields_map.get("grid")
            && *g != "1/16"
        {
            return Err(format!("unsupported grid {g:?} (only 1/16)"));
        }
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(format!("bad tempo {bpm}"));
        }
        *header = Some(Header { name, bpm, meter });
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
