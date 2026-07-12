//! Layer 1b — quantization: seconds-domain notes → grid-aligned events.
//!
//! The grid is 16th cells (4 per beat). µtiming residuals are measured and
//! reported but discarded (lossy by design; a sidecar is a later option).

use crate::model::{RawNote, RawSong};
use crate::tempo;

pub const CELLS_PER_BEAT: u32 = 4;

/// A note on the grid. `cell` is a global 16th index: bar = cell / 16 in 4/4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QNote {
    pub pitch: u8,
    pub cell: u32,
    pub dur_cells: u32,
    pub vel: u8,
}

#[derive(Debug, Clone)]
pub struct QTrack {
    pub name: String,
    pub program: u8,
    pub is_drums: bool,
    /// Sorted by (cell, pitch).
    pub notes: Vec<QNote>,
}

/// Swing feel, applied at render time. `percent` is where the swung
/// subdivision lands inside its parent beat: 50 = straight, 66 = triplet
/// shuffle. Authoring-only for now (never produced by quantization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swing {
    /// Which layer swings: 8 = offbeat 8ths, 16 = offbeat 16ths.
    pub level: u8,
    /// 50..=75.
    pub percent: u8,
}

#[derive(Debug, Clone)]
pub struct QSong {
    pub name: String,
    pub bpm: f64,
    /// (numerator, denominator): declared by the source when available,
    /// else detected (4/4, 3/4, 6/8 templates).
    pub meter: (u32, u32),
    /// Estimated key (header + spelling); `None` = unknown, spell sharps.
    pub key: Option<crate::key::Key>,
    /// Swing feel (authoring; see [`Swing`]).
    pub swing: Option<Swing>,
    pub n_bars: u32,
    pub tracks: Vec<QTrack>,
}

impl QSong {
    pub fn cells_per_bar(&self) -> u32 {
        self.meter.0 * CELLS_PER_BEAT * 4 / self.meter.1
    }

    pub fn cell_sec(&self) -> f64 {
        60.0 / (self.bpm * CELLS_PER_BEAT as f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempoSource {
    /// Constant tempo declared by the source file, grid anchored at t=0.
    Declared,
    /// Estimated from onsets (MuScriptor streams, or `--infer-tempo`).
    Inferred,
    /// User-supplied BPM; phase/downbeat still estimated from onsets.
    Override,
    /// The file declared a tempo, but onsets fit an inferred grid much
    /// better (live takes against a default click), so it was replaced.
    AutoInferred { declared_bpm: f64, declared_mean_ms: f64 },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QuantizeOptions {
    /// Force this BPM instead of declaring/inferring.
    pub bpm_override: Option<f64>,
    /// Infer tempo from onsets even if the source declares one.
    pub infer_tempo: bool,
    /// Trust a declared tempo unconditionally (disables the auto-switch).
    pub no_infer: bool,
}

/// Declared grids with residuals above this are suspects for auto-switch.
const AUTO_INFER_TRIGGER_MS: f64 = 25.0;
/// The inferred grid must be at least this much tighter to win.
const AUTO_INFER_RATIO: f64 = 0.6;

/// Mean |onset − nearest grid point| in ms for a grid hypothesis.
fn mean_residual_ms(song: &RawSong, bpm: f64, origin: f64) -> f64 {
    let cell = 60.0 / (bpm * CELLS_PER_BEAT as f64);
    let (mut sum, mut n) = (0.0, 0usize);
    for t in &song.tracks {
        for note in &t.notes {
            let k = ((note.onset - origin) / cell).round();
            sum += ((note.onset - origin) - k * cell).abs() * 1e3;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

#[derive(Debug, Clone, Copy)]
pub struct QuantizeReport {
    pub bpm: f64,
    pub tempo_source: TempoSource,
    /// Seconds of bar 0, cell 0 in the source timeline.
    pub origin: f64,
    pub note_count: usize,
    /// Onset µtiming discarded by snapping, milliseconds.
    pub mean_abs_residual_ms: f64,
    pub max_abs_residual_ms: f64,
}

pub fn quantize(song: &RawSong, opts: &QuantizeOptions) -> (QSong, QuantizeReport) {
    let declared_meter = song.source_meter.unwrap_or((4, 4));
    let (bpm, origin, meter, tempo_source) =
        match (opts.bpm_override, opts.infer_tempo, song.source_bpm) {
            (Some(bpm), _, _) => {
                let (origin, meter) = tempo::align_known_bpm(song, bpm);
                let meter = song.source_meter.unwrap_or(meter);
                (bpm, origin, meter, TempoSource::Override)
            }
            (None, false, Some(declared)) => {
                let declared_mean = mean_residual_ms(song, declared, 0.0);
                let mut choice = (declared, 0.0, declared_meter, TempoSource::Declared);
                if !opts.no_infer && declared_mean > AUTO_INFER_TRIGGER_MS {
                    let est = tempo::estimate(song);
                    let inferred_mean = mean_residual_ms(song, est.bpm, est.origin);
                    if inferred_mean < AUTO_INFER_RATIO * declared_mean {
                        // The declared meter stays authoritative if present:
                        // a lying tempo doesn't make the meter a lie too.
                        let meter = song.source_meter.unwrap_or(est.meter);
                        choice = (
                            est.bpm,
                            est.origin,
                            meter,
                            TempoSource::AutoInferred {
                                declared_bpm: declared,
                                declared_mean_ms: declared_mean,
                            },
                        );
                    }
                }
                choice
            }
            _ => {
                let est = tempo::estimate(song);
                let meter = song.source_meter.unwrap_or(est.meter);
                (est.bpm, est.origin, meter, TempoSource::Inferred)
            }
        };

    let cell_sec = 60.0 / (bpm * CELLS_PER_BEAT as f64);
    let snap = |t: f64| ((t - origin) / cell_sec).round() as i64;

    let mut residual_sum = 0.0f64;
    let mut residual_max = 0.0f64;
    let mut note_count = 0usize;
    let mut max_end_cell = 0u32;

    let tracks = song
        .tracks
        .iter()
        .map(|t| {
            let mut notes: Vec<QNote> = t
                .notes
                .iter()
                .map(|n: &RawNote| {
                    let cell_i = snap(n.onset);
                    let residual = ((n.onset - origin) - cell_i as f64 * cell_sec).abs() * 1e3;
                    residual_sum += residual;
                    residual_max = residual_max.max(residual);
                    note_count += 1;
                    let cell = cell_i.max(0) as u32;
                    let end = snap(n.end()).max(0) as u32;
                    // Drum hits are one-shots; their length carries no
                    // information (MuScriptor emits a fixed minimum anyway).
                    let dur_cells = if t.is_drums { 1 } else { end.saturating_sub(cell).max(1) };
                    max_end_cell = max_end_cell.max(cell + dur_cells);
                    QNote { pitch: n.pitch, cell, dur_cells, vel: n.vel }
                })
                .collect();
            notes.sort_by(|a: &QNote, b: &QNote| a.cell.cmp(&b.cell).then(a.pitch.cmp(&b.pitch)));
            QTrack { name: t.name.clone(), program: t.program, is_drums: t.is_drums, notes }
        })
        .collect();

    let mut qsong =
        QSong { name: song.name.clone(), bpm, meter, key: None, swing: None, n_bars: 0, tracks };
    qsong.n_bars = max_end_cell.div_ceil(qsong.cells_per_bar());
    qsong.key = crate::key::detect(&qsong);

    let report = QuantizeReport {
        bpm,
        tempo_source,
        origin,
        note_count,
        mean_abs_residual_ms: if note_count > 0 { residual_sum / note_count as f64 } else { 0.0 },
        max_abs_residual_ms: residual_max,
    };
    (qsong, report)
}
