//! Layer 4 — QSong → text: patterns + arrangement.
//!
//! ```text
//! # song: demo  tempo: 120.00  meter: 4/4  key: Am  grid: 1/16
//! # instruments: bass:33 drums:kit piano:0 lead:81
//!
//! P1 bass  | A,,4 A,,4 G,,4 E,,4 |
//! P2 drums
//!   K |x... x... x... x...|
//!   S |.... x... .... x...|
//! P3 piano* | Am . F G7 |
//! P4 lead  | e2 c2 d2 B2 c4 A4 |
//!
//! arrangement:
//!   [P1+P2] x4
//!   [P1+P2+P3+P4] x8
//! ```
//!
//! Per (instrument, bar), the body takes the most semantic form that stays
//! lossless: drum tracks become step-grid lanes; harmonic bars whose
//! voicings are canonical become chord symbols (`*` marks chord mode,
//! 1 column = 1 beat, `.` holds); everything else is melodic ABC-style
//! tokens with explicit accidentals, spelled for the detected key.
//! Identical (instrument, bar) contents share one pattern; the arrangement
//! lists bar-stacks with run-length encoding (see [`crate::pattern`]).

use crate::chord;
use crate::drums;
use crate::grid::{QSong, QTrack};
use crate::notation::{Tok, emit_token_spelled};
use crate::pattern;
use std::collections::BTreeMap;
use std::fmt::Write;

/// A note fragment clipped to one bar.
struct Seg {
    cell: u32, // bar-relative
    dur: u32,
    pitch: u8,
    tie_in: bool,
    tie_out: bool,
}

/// Split a track's notes at bar boundaries: per-bar segment lists.
fn split_bars(track: &QTrack, cells_per_bar: u32, n_bars: u32) -> Vec<Vec<Seg>> {
    let mut bars: Vec<Vec<Seg>> = (0..n_bars).map(|_| Vec::new()).collect();
    for n in &track.notes {
        let end = n.cell + n.dur_cells;
        let mut cell = n.cell;
        while cell < end {
            let bar = cell / cells_per_bar;
            let bar_end = (bar + 1) * cells_per_bar;
            let seg_end = end.min(bar_end);
            if let Some(slot) = bars.get_mut(bar as usize) {
                slot.push(Seg {
                    cell: cell - bar * cells_per_bar,
                    dur: seg_end - cell,
                    pitch: n.pitch,
                    tie_in: cell > n.cell,
                    tie_out: seg_end < end,
                });
            }
            cell = seg_end;
        }
    }
    bars
}

/// Render one bar's segments as melodic voice strings (usually one voice).
fn bar_voices(segs: &[Seg], cells_per_bar: u32, flats: bool) -> Vec<String> {
    // Segments sharing (onset, duration, tie) stack into one chord token.
    let mut groups: BTreeMap<(u32, u32, bool), Vec<u8>> = BTreeMap::new();
    for s in segs {
        groups.entry((s.cell, s.dur, s.tie_out)).or_default().push(s.pitch);
    }
    // Greedy voice allocation: each group goes to the first voice that has
    // already ended when the group starts.
    struct Voice {
        end: u32,
        toks: Vec<Tok>,
    }
    let mut voices: Vec<Voice> = Vec::new();
    for ((cell, dur, tie), mut pitches) in groups {
        pitches.sort_unstable();
        let tok = if pitches.len() == 1 {
            Tok::Note { pitch: pitches[0], dur, tie }
        } else {
            Tok::Chord { pitches, dur, tie }
        };
        let voice = match voices.iter_mut().find(|v| v.end <= cell) {
            Some(v) => v,
            None => {
                voices.push(Voice { end: 0, toks: Vec::new() });
                voices.last_mut().unwrap()
            }
        };
        if cell > voice.end {
            voice.toks.push(Tok::Rest { dur: cell - voice.end });
        }
        voice.toks.push(tok);
        voice.end = cell + dur;
    }
    voices
        .into_iter()
        .map(|mut v| {
            if v.end < cells_per_bar {
                v.toks.push(Tok::Rest { dur: cells_per_bar - v.end });
            }
            v.toks.iter().map(|t| emit_token_spelled(t, flats)).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// Chord-mode body (`Am . F G7`) if — and only if — every onset group in
/// the bar is a beat-aligned, uniformly-held, canonically-voiced chord.
fn try_chordal(segs: &[Seg], cells_per_bar: u32, flats: bool) -> Option<String> {
    if segs.is_empty() || segs.iter().any(|s| s.tie_in || s.tie_out) {
        return None;
    }
    let mut groups: BTreeMap<u32, Vec<&Seg>> = BTreeMap::new();
    for s in segs {
        groups.entry(s.cell).or_default().push(s);
    }
    let onsets: Vec<u32> = groups.keys().copied().collect();
    let beats = (cells_per_bar / 4) as usize;
    let mut columns: Vec<Option<String>> = vec![None; beats]; // None = rest/hold slot
    let mut covered = vec![false; beats];
    for (i, (&onset, group)) in groups.iter().enumerate() {
        if onset % 4 != 0 {
            return None;
        }
        let dur = group[0].dur;
        if dur % 4 != 0 || group.iter().any(|s| s.dur != dur) {
            return None;
        }
        let limit = onsets.get(i + 1).copied().unwrap_or(cells_per_bar);
        if onset + dur > limit {
            return None; // overlaps the next chord (or the bar line)
        }
        let mut pitches: Vec<u8> = group.iter().map(|s| s.pitch).collect();
        pitches.sort_unstable();
        let sym = chord::detect(&pitches)?;
        let beat = (onset / 4) as usize;
        columns[beat] = Some(chord::symbol_to_string(&sym, flats));
        covered[beat..beat + (dur / 4) as usize].fill(true);
    }
    let cols: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(b, c)| match c {
            Some(sym) => sym.clone(),
            None if covered[b] => ".".to_string(),
            None => "z".to_string(),
        })
        .collect();
    Some(cols.join(" "))
}

/// Drum step-grid lanes, one per GM voice, cells grouped by beat.
fn drum_lanes(segs: &[Seg], cells_per_bar: u32) -> String {
    let mut lanes: BTreeMap<u8, Vec<bool>> = BTreeMap::new();
    for s in segs {
        lanes.entry(s.pitch).or_insert_with(|| vec![false; cells_per_bar as usize])
            [s.cell as usize] = true;
    }
    let mut order: Vec<u8> = lanes.keys().copied().collect();
    order.sort_by_key(|p| drums::lane_order(*p));
    let label_w = order.iter().map(|p| drums::lane_label(*p).len()).max().unwrap_or(1);
    order
        .iter()
        .map(|pitch| {
            let cells = &lanes[pitch];
            let mut grid = String::new();
            for (i, hit) in cells.iter().enumerate() {
                if i > 0 && i % 4 == 0 {
                    grid.push(' ');
                }
                grid.push(if *hit { 'x' } else { '.' });
            }
            format!("  {:<label_w$} |{grid}|", drums::lane_label(*pitch))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One bar's emitted form.
enum Body {
    Melodic(String),
    Chordal(String),
    Drums(String),
}

impl Body {
    /// Dedup key: kind tag + content (a chordal body must never collide
    /// with an identical-looking melodic one).
    fn key(&self) -> String {
        match self {
            Body::Melodic(s) => format!("m:{s}"),
            Body::Chordal(s) => format!("c:{s}"),
            Body::Drums(s) => format!("d:{s}"),
        }
    }
}

fn instrument_field(t: &QTrack) -> String {
    if t.is_drums { format!("{}:kit", t.name) } else { format!("{}:{}", t.name, t.program) }
}

pub fn emit(q: &QSong) -> String {
    let flats = q.key.map(|k| k.use_flats()).unwrap_or(false);
    let mut out = String::new();
    let _ = write!(
        out,
        "# song: {}  tempo: {:.2}  meter: {}/{}",
        q.name, q.bpm, q.meter.0, q.meter.1
    );
    if let Some(key) = q.key {
        let _ = write!(out, "  key: {}", key.name());
    }
    out.push_str("  grid: 1/16\n");
    let _ = writeln!(
        out,
        "# instruments: {}",
        q.tracks.iter().map(instrument_field).collect::<Vec<_>>().join(" ")
    );
    out.push('\n');

    let cpb = q.cells_per_bar();
    let bodies: Vec<Vec<Option<Body>>> = q
        .tracks
        .iter()
        .map(|t| {
            split_bars(t, cpb, q.n_bars)
                .iter()
                .map(|segs| {
                    if segs.is_empty() {
                        return None;
                    }
                    Some(if t.is_drums {
                        Body::Drums(drum_lanes(segs, cpb))
                    } else if let Some(c) = try_chordal(segs, cpb, flats) {
                        Body::Chordal(c)
                    } else {
                        Body::Melodic(bar_voices(segs, cpb, flats).join(" & "))
                    })
                })
                .collect()
        })
        .collect();

    let keys: Vec<Vec<Option<String>>> = bodies
        .iter()
        .map(|track| track.iter().map(|b| b.as_ref().map(Body::key)).collect())
        .collect();
    let set = pattern::build(&keys);

    let id_w = set.patterns.len().to_string().len();
    let name_w = q.tracks.iter().map(|t| t.name.len() + 1).max().unwrap_or(0);
    for p in &set.patterns {
        // Find the Body behind this pattern via its key (first occurrence).
        let bar = keys[p.track]
            .iter()
            .position(|k| k.as_deref() == Some(p.body.as_str()))
            .expect("pattern came from these bodies");
        let name = &q.tracks[p.track].name;
        match bodies[p.track][bar].as_ref().unwrap() {
            Body::Melodic(s) => {
                let _ = writeln!(out, "P{:<id_w$} {:<name_w$} | {s} |", p.id, name);
            }
            Body::Chordal(s) => {
                let starred = format!("{name}*");
                let _ = writeln!(out, "P{:<id_w$} {:<name_w$} | {s} |", p.id, starred);
            }
            Body::Drums(lanes) => {
                let _ = writeln!(out, "P{:<id_w$} {name}", p.id);
                let _ = writeln!(out, "{lanes}");
            }
        }
    }

    if !set.rows.is_empty() {
        out.push('\n');
        out.push_str("arrangement:\n");
        for row in &set.rows {
            let stack = if row.stack.is_empty() {
                "z".to_string()
            } else {
                row.stack.iter().map(|id| format!("P{id}")).collect::<Vec<_>>().join("+")
            };
            match row.reps {
                1 => {
                    let _ = writeln!(out, "  [{stack}]");
                }
                n => {
                    let _ = writeln!(out, "  [{stack}] x{n}");
                }
            }
        }
    }
    out
}
