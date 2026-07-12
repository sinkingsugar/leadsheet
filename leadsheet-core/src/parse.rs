//! Text → QSong. The inverse of [`crate::emit`]; the renderer's front door.
//!
//! Tolerant of whitespace and of anything after the closing `|` (annotation
//! comments). Strict about what matters: unknown instruments, bad tokens,
//! and voices that don't sum to a full bar are hard errors with line numbers.

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

pub fn parse(text: &str) -> Result<QSong, Error> {
    let err = |line: usize, msg: String| Error::Parse(format!("line {line}: {msg}"));

    let mut header: Option<Header> = None;
    let mut tracks: Vec<QTrack> = Vec::new();
    let mut track_index: HashMap<String, usize> = HashMap::new();
    // Open ties per (track, pitch): (index into track notes, end cell so far).
    let mut open_ties: HashMap<(usize, u8), (usize, u32)> = HashMap::new();

    for (lineno, raw) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            parse_header_line(rest, &mut header, &mut tracks, &mut track_index)
                .map_err(|m| err(lineno, m))?;
            continue;
        }

        // Bar line: `b<n> <instrument> | voice & voice |`
        let header_ref = header
            .as_ref()
            .ok_or_else(|| err(lineno, "bar line before `# song:` header".into()))?;
        let cells_per_bar = header_ref.meter.0 * 16 / header_ref.meter.1;

        let (prefix, rest) = line
            .split_once('|')
            .ok_or_else(|| err(lineno, format!("expected `| ... |` in {line:?}")))?;
        let content = match rest.rfind('|') {
            Some(i) => &rest[..i],
            None => return Err(err(lineno, "missing closing `|`".into())),
        };
        let mut parts = prefix.split_whitespace();
        let bar_label = parts.next().ok_or_else(|| err(lineno, "missing bar label".into()))?;
        let bar: u32 = bar_label
            .strip_prefix('b')
            .and_then(|n| n.parse().ok())
            .filter(|b| *b >= 1)
            .ok_or_else(|| err(lineno, format!("bad bar label {bar_label:?}")))?;
        let inst = parts.next().ok_or_else(|| err(lineno, "missing instrument".into()))?;
        if let Some(junk) = parts.next() {
            return Err(err(lineno, format!("unexpected {junk:?} before `|`")));
        }
        let ti = *track_index
            .get(inst)
            .ok_or_else(|| err(lineno, format!("unknown instrument {inst:?}")))?;
        let bar_start = (bar - 1) * cells_per_bar;

        for voice in content.split('&') {
            let mut cursor = bar_start;
            for tok_str in voice.split_whitespace() {
                let tok = parse_token(tok_str).map_err(|m| err(lineno, m))?;
                let dur = tok.dur();
                if cursor + dur > bar_start + cells_per_bar {
                    return Err(err(
                        lineno,
                        format!("bar overflows at token {tok_str:?} ({inst}, bar {bar})"),
                    ));
                }
                let (pitches, tie): (Vec<u8>, bool) = match tok {
                    Tok::Rest { .. } => (vec![], false),
                    Tok::Note { pitch, tie, .. } => (vec![pitch], tie),
                    Tok::Chord { pitches, tie, .. } => (pitches, tie),
                };
                for pitch in pitches {
                    let key = (ti, pitch);
                    // Continuation of a tied note joins it; else a new note.
                    let idx = match open_ties.remove(&key) {
                        Some((idx, end)) if end == cursor => {
                            tracks[ti].notes[idx].dur_cells += dur;
                            idx
                        }
                        _ => {
                            tracks[ti].notes.push(QNote {
                                pitch,
                                cell: cursor,
                                dur_cells: dur,
                                vel: DEFAULT_VEL,
                            });
                            tracks[ti].notes.len() - 1
                        }
                    };
                    if tie {
                        open_ties.insert(key, (idx, cursor + dur));
                    }
                }
                cursor += dur;
            }
            if cursor != bar_start + cells_per_bar && cursor != bar_start {
                return Err(err(
                    lineno,
                    format!(
                        "voice covers {} of {} cells ({inst}, bar {bar})",
                        cursor - bar_start,
                        cells_per_bar
                    ),
                ));
            }
        }
    }

    let header = header.ok_or_else(|| Error::Parse("missing `# song:` header".into()))?;
    // Ties left open at EOF are already stored at their accumulated length.
    let cells_per_bar = header.meter.0 * 16 / header.meter.1;
    let mut max_end = 0u32;
    for t in &mut tracks {
        t.notes.sort_by(|a, b| a.cell.cmp(&b.cell).then(a.pitch.cmp(&b.pitch)));
        for n in &t.notes {
            max_end = max_end.max(n.cell + n.dur_cells);
        }
    }
    Ok(QSong {
        name: header.name,
        bpm: header.bpm,
        meter: header.meter,
        n_bars: max_end.div_ceil(cells_per_bar),
        tracks,
    })
}

fn parse_header_line(
    rest: &str,
    header: &mut Option<Header>,
    tracks: &mut Vec<QTrack>,
    track_index: &mut HashMap<String, usize>,
) -> Result<(), String> {
    let rest = rest.trim();
    if let Some(fields) = rest.strip_prefix("song:") {
        // `song: NAME  tempo: T  meter: N/D  grid: 1/16` — the name runs
        // until the `tempo:` key (names may contain single spaces).
        let (name_part, after) =
            fields.split_once("tempo:").ok_or("header missing `tempo:`")?;
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
            let (name, prog) =
                field.split_once(':').ok_or(format!("bad instrument {field:?}"))?;
            let (program, is_drums) = if prog == "kit" {
                (0u8, true)
            } else {
                (prog.parse::<u8>().ok().filter(|p| *p <= 127).ok_or(format!(
                    "bad program in {field:?}"
                ))?, false)
            };
            if track_index.contains_key(name) {
                return Err(format!("duplicate instrument {name:?}"));
            }
            track_index.insert(name.to_string(), tracks.len());
            tracks.push(QTrack {
                name: name.to_string(),
                program,
                is_drums,
                notes: Vec::new(),
            });
        }
        return Ok(());
    }
    // Any other `#` line is a comment.
    Ok(())
}
