//! Layer 4 (minimal) — QSong → text. Explicit bars, no pattern dedup yet.
//!
//! ```text
//! # song: demo  tempo: 120.00  meter: 4/4  grid: 1/16
//! # instruments: bass:33 drums:kit lead:81
//!
//! b1 bass  | A,,4 A,,4 G,,4 E,,4 |
//! b1 drums | [C,^F,]2 ^F, ... |
//! b2 bass  | ... |
//! ```
//!
//! Notes crossing a bar line are split and tied (`-`). Overlapping notes
//! that can't stack into one chord are separated into voices with ` & `.

use crate::grid::{QSong, QTrack};
use crate::notation::{Tok, emit_token};
use std::collections::BTreeMap;
use std::fmt::Write;

/// A note fragment clipped to one bar.
struct Seg {
    cell: u32, // bar-relative
    dur: u32,
    pitch: u8,
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
                    tie_out: seg_end < end,
                });
            }
            cell = seg_end;
        }
    }
    bars
}

/// Render one bar's segments as voice strings (usually one voice).
fn bar_voices(segs: &[Seg], cells_per_bar: u32) -> Vec<String> {
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
            v.toks.iter().map(emit_token).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

fn instrument_field(t: &QTrack) -> String {
    if t.is_drums { format!("{}:kit", t.name) } else { format!("{}:{}", t.name, t.program) }
}

pub fn emit(q: &QSong) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# song: {}  tempo: {:.2}  meter: {}/{}  grid: 1/16",
        q.name, q.bpm, q.meter.0, q.meter.1
    );
    let _ = writeln!(
        out,
        "# instruments: {}",
        q.tracks.iter().map(instrument_field).collect::<Vec<_>>().join(" ")
    );
    out.push('\n');

    let cpb = q.cells_per_bar();
    let per_track: Vec<Vec<Vec<Seg>>> =
        q.tracks.iter().map(|t| split_bars(t, cpb, q.n_bars)).collect();

    let bar_w = q.n_bars.to_string().len();
    let name_w = q.tracks.iter().map(|t| t.name.len()).max().unwrap_or(0);
    for bar in 0..q.n_bars as usize {
        for (track, bars) in q.tracks.iter().zip(&per_track) {
            let voices = bar_voices(&bars[bar], cpb);
            if voices.is_empty() {
                continue;
            }
            let _ = writeln!(
                out,
                "b{:<bar_w$} {:<name_w$} | {} |",
                bar + 1,
                track.name,
                voices.join(" & "),
            );
        }
    }
    out
}
