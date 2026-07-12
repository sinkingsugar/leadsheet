//! The correctness oracle: compress → render → re-ingest → note F1,
//! plus compression-ratio bookkeeping.

use crate::error::Error;
use crate::grid::{QuantizeOptions, QuantizeReport, quantize};
use crate::model::RawSong;
use crate::{emit, ingest, parse, render};
use std::fmt::Write as _;

/// Note-level precision/recall over (instrument, pitch, onset±tol).
/// Duration is deliberately excluded (plan: onset/pitch/instrument).
#[derive(Debug, Clone, Copy)]
pub struct NoteF1 {
    pub matched: usize,
    pub ref_count: usize,
    pub hyp_count: usize,
}

impl NoteF1 {
    pub fn precision(&self) -> f64 {
        if self.hyp_count == 0 { 1.0 } else { self.matched as f64 / self.hyp_count as f64 }
    }

    pub fn recall(&self) -> f64 {
        if self.ref_count == 0 { 1.0 } else { self.matched as f64 / self.ref_count as f64 }
    }

    pub fn f1(&self) -> f64 {
        let (p, r) = (self.precision(), self.recall());
        if p + r == 0.0 { 0.0 } else { 2.0 * p * r / (p + r) }
    }
}

/// Match notes between two songs. `ref_shift` is subtracted from reference
/// onsets first (the compressor's grid origin: rendered output starts at
/// bar 0 = `origin` seconds of the source timeline).
///
/// Instruments are pooled by (is_drums, program) so track naming/count
/// differences don't matter; within a pool, notes match per pitch by greedy
/// two-pointer over sorted onsets with tolerance `tol_sec`.
pub fn note_f1(reference: &RawSong, ref_shift: f64, hyp: &RawSong, tol_sec: f64) -> NoteF1 {
    use std::collections::HashMap;
    type Pool = HashMap<(bool, u8, u8), Vec<f64>>;

    let collect = |song: &RawSong, shift: f64| -> Pool {
        let mut pool: Pool = HashMap::new();
        for t in &song.tracks {
            let program = if t.is_drums { 0 } else { t.program };
            for n in &t.notes {
                pool.entry((t.is_drums, program, n.pitch)).or_default().push(n.onset - shift);
            }
        }
        for v in pool.values_mut() {
            v.sort_by(f64::total_cmp);
        }
        pool
    };

    let ref_pool = collect(reference, ref_shift);
    let hyp_pool = collect(hyp, 0.0);

    let ref_count = ref_pool.values().map(Vec::len).sum();
    let hyp_count = hyp_pool.values().map(Vec::len).sum();
    let mut matched = 0usize;
    for (key, refs) in &ref_pool {
        let Some(hyps) = hyp_pool.get(key) else { continue };
        let (mut i, mut j) = (0, 0);
        while i < refs.len() && j < hyps.len() {
            let d = refs[i] - hyps[j];
            if d.abs() <= tol_sec {
                matched += 1;
                i += 1;
                j += 1;
            } else if d > 0.0 {
                j += 1;
            } else {
                i += 1;
            }
        }
    }
    NoteF1 { matched, ref_count, hyp_count }
}

/// The naive baseline the plan's compression target is measured against:
/// one plain-text line per note.
pub fn naive_event_text(song: &RawSong) -> String {
    let mut out = String::new();
    for t in &song.tracks {
        for n in &t.notes {
            let _ = writeln!(
                out,
                "t={:.3} dur={:.3} pitch={} inst={}",
                n.onset, n.dur, n.pitch, t.name
            );
        }
    }
    out
}

#[derive(Debug)]
pub struct RoundtripReport {
    pub quant: QuantizeReport,
    pub f1: NoteF1,
    pub text: String,
    pub midi_out: Vec<u8>,
    pub naive_bytes: usize,
}

impl RoundtripReport {
    pub fn ls_bytes(&self) -> usize {
        self.text.len()
    }

    pub fn ratio_vs_naive(&self) -> f64 {
        self.naive_bytes as f64 / self.ls_bytes().max(1) as f64
    }
}

/// compress → parse → render → re-ingest → F1 (±1 grid cell tolerance).
pub fn roundtrip(song: &RawSong, opts: &QuantizeOptions) -> Result<RoundtripReport, Error> {
    let (qsong, quant) = quantize(song, opts);
    let text = emit::emit(&qsong);
    let reparsed = parse::parse(&text)?;
    let midi_out = render::render(&reparsed);
    let back = ingest::ingest_midi(&midi_out, &song.name)?;
    let cell_sec = 60.0 / (quant.bpm * 4.0);
    let f1 = note_f1(song, quant.origin, &back, cell_sec * 1.001);
    Ok(RoundtripReport { quant, f1, text, midi_out, naive_bytes: naive_event_text(song).len() })
}
