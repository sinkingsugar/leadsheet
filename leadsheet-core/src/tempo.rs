//! Layer 1a — tempo, grid phase, and downbeat estimation from note onsets.
//!
//! No audio, no FFT: everything works off the onset train.
//!
//! 1. Bin weighted onsets into a 10 ms impulse train, smooth lightly.
//! 2. Autocorrelate; score candidate beat lags (60–200 BPM) with subdivision
//!    agreement (half/double/quadruple lags must also correlate) and a mild
//!    log-normal prior around 115 BPM, so the *beat* wins over the 8th or the
//!    half-note.
//! 3. Refine ±3% at the 16th-note level by maximizing the circular alignment
//!    |Σ w·e^(i·2π·t/cell)| — tempo error decoheres over the whole song, so
//!    this is sharp — and read the grid phase off the argument.
//! 4. Downbeat: score all 16 cell offsets against a strong-beat template,
//!    weighting kick, bass, low pitches and long notes (chord changes).

use crate::model::RawSong;
use std::f64::consts::TAU;

/// Grid hypothesis: 16th cells at `origin + k * 15/bpm` seconds, with
/// `origin` the start of bar 0 (earliest onset falls inside bar 0).
#[derive(Debug, Clone, Copy)]
pub struct TempoEstimate {
    pub bpm: f64,
    pub origin: f64,
    pub meter: (u32, u32),
    /// Winning beat-lag score over the runner-up (≥ 1.0; higher = cleaner).
    pub confidence: f64,
}

const DT: f64 = 0.01; // autocorrelation bin, seconds
const MIN_BPM: f64 = 60.0;
const MAX_BPM: f64 = 200.0;
const FALLBACK_BPM: f64 = 120.0;

/// A meter hypothesis: bar length in 16th cells + downbeat templates.
struct MeterSpec {
    meter: (u32, u32),
    cells: usize,
    generic: &'static [f64],
    snare: &'static [f64],
    /// 4/4 is the prior; others must earn their keep.
    bias: f64,
}

/// Strong-beat templates. Generic: beat 1 > secondary beats > 8ths > 16ths.
/// Snare: the backbeat — the one cue a wrong tempo octave or meter can't
/// fake (see `downbeat_origin`).
const T44_GENERIC: [f64; 16] =
    [3.0, 0.1, 0.3, 0.1, 1.2, 0.1, 0.3, 0.1, 2.0, 0.1, 0.3, 0.1, 1.2, 0.1, 0.3, 0.1];
const T44_SNARE: [f64; 16] =
    [0.1, 0.1, 0.3, 0.1, 4.0, 0.1, 0.3, 0.1, 0.1, 0.1, 0.3, 0.1, 4.0, 0.1, 0.3, 0.1];
const T34_GENERIC: [f64; 12] = [3.0, 0.1, 0.3, 0.1, 1.5, 0.1, 0.3, 0.1, 1.5, 0.1, 0.3, 0.1];
const T34_SNARE: [f64; 12] = [0.1, 0.1, 0.3, 0.1, 2.5, 0.1, 0.3, 0.1, 2.5, 0.1, 0.3, 0.1];
const T68_GENERIC: [f64; 12] = [3.0, 0.1, 0.6, 0.1, 0.6, 0.1, 2.0, 0.1, 0.6, 0.1, 0.6, 0.1];
const T68_SNARE: [f64; 12] = [0.1, 0.1, 0.3, 0.1, 0.3, 0.1, 4.0, 0.1, 0.3, 0.1, 0.3, 0.1];

const METERS: &[MeterSpec] = &[
    MeterSpec { meter: (4, 4), cells: 16, generic: &T44_GENERIC, snare: &T44_SNARE, bias: 1.0 },
    MeterSpec { meter: (3, 4), cells: 12, generic: &T34_GENERIC, snare: &T34_SNARE, bias: 0.92 },
    MeterSpec { meter: (6, 8), cells: 12, generic: &T68_GENERIC, snare: &T68_SNARE, bias: 0.92 },
];

/// Metrical role of an onset — kick and snare get their own downbeat
/// templates (kick ≈ downbeat prior, snare ≈ backbeat prior).
#[derive(Clone, Copy, PartialEq)]
enum EvClass {
    Kick,
    Snare,
    Other,
}

/// One onset with the evidence weights the estimators care about.
struct Ev {
    t: f64,
    w: f64,
    dur: f64,
    class: EvClass,
}

fn events(song: &RawSong) -> Vec<Ev> {
    let mut evs = Vec::with_capacity(song.note_count());
    for track in &song.tracks {
        let is_bass = !track.is_drums && (32..=39).contains(&track.program);
        for n in &track.notes {
            if n.onset < 0.0 {
                continue;
            }
            let (w, class) = if track.is_drums {
                match n.pitch {
                    35 | 36 => (3.0, EvClass::Kick),
                    38 | 40 => (2.0, EvClass::Snare),
                    _ => (1.2, EvClass::Other),
                }
            } else if is_bass {
                (2.0, EvClass::Other)
            } else {
                (1.0, EvClass::Other)
            };
            evs.push(Ev { t: n.onset, w, dur: n.dur, class });
        }
    }
    evs.sort_by(|a, b| a.t.total_cmp(&b.t));
    evs
}

/// Full estimation: tempo + phase + downbeat.
///
/// Autocorrelation gives a beat period up to octave ambiguity (a bar always
/// self-correlates at least as well as a beat). So the winner *and* its
/// half/double partners each get refined and judged on: 16th-grid alignment
/// coherence (onsets off the hypothesis grid contribute negatively), the
/// drum-aware downbeat template fit, and a mild tempo prior.
pub fn estimate(song: &RawSong) -> TempoEstimate {
    let evs = events(song);
    if evs.len() < 8 {
        return TempoEstimate { bpm: FALLBACK_BPM, origin: 0.0, meter: (4, 4), confidence: 1.0 };
    }
    let (bpm0, confidence) = beat_period(&evs);
    let mut candidates = vec![bpm0];
    if bpm0 * 2.0 <= MAX_BPM {
        candidates.push(bpm0 * 2.0);
    }
    if bpm0 / 2.0 >= MIN_BPM {
        candidates.push(bpm0 / 2.0);
    }

    let total_w: f64 = evs.iter().map(|e| e.w).sum();
    let mut best: Option<(f64, f64, f64, (u32, u32))> = None; // (score, bpm, origin, meter)
    for c in candidates {
        let (bpm, phase, mag) = refine(&evs, c, 0.03);
        let coherence = mag / total_w; // 1.0 = every onset exactly on this grid
        let prior = (-(bpm / 100.0).ln().powi(2) / (2.0 * 0.45f64.powi(2))).exp();
        for spec in METERS {
            let (origin, fit) = downbeat_origin(&evs, bpm, phase, spec);
            // Normalize by the template's per-cell mass so meters with
            // different bar lengths score comparably.
            let template_mean = spec.generic.iter().sum::<f64>() / spec.cells as f64;
            let score =
                coherence * (fit / (total_w * template_mean)) * (0.6 + 0.4 * prior) * spec.bias;
            if best.is_none_or(|(s, _, _, _)| score > s) {
                best = Some((score, bpm, origin, spec.meter));
            }
        }
    }
    let (_, bpm, origin, meter) = best.unwrap();
    TempoEstimate { bpm, origin, meter, confidence }
}

/// Grid alignment when the BPM is already known (e.g. user override on a
/// MuScriptor stream): only phase + downbeat are estimated.
pub fn align_known_bpm(song: &RawSong, bpm: f64) -> (f64, (u32, u32)) {
    let evs = events(song);
    if evs.is_empty() {
        return (0.0, (4, 4));
    }
    // Tiny search window: absorb rounding, keep the caller's tempo.
    let (_, phase, _) = refine(&evs, bpm, 0.0005);
    let mut best: Option<(f64, f64, (u32, u32))> = None;
    for spec in METERS {
        let (origin, fit) = downbeat_origin(&evs, bpm, phase, spec);
        let template_mean = spec.generic.iter().sum::<f64>() / spec.cells as f64;
        let score = fit / template_mean * spec.bias;
        if best.is_none_or(|(s, _, _)| score > s) {
            best = Some((score, origin, spec.meter));
        }
    }
    let (_, origin, meter) = best.unwrap();
    (origin, meter)
}

/// Autocorrelation of the smoothed onset train → beat period candidate.
fn beat_period(evs: &[Ev]) -> (f64, f64) {
    let t_end = evs.last().map(|e| e.t).unwrap_or(0.0);
    let n = (t_end / DT).ceil() as usize + 8;

    // Impulse train, then a small triangular smoothing (±20 ms tolerance).
    let mut raw = vec![0.0f64; n];
    for e in evs {
        let i = (e.t / DT).round() as usize;
        if i < n {
            raw[i] += e.w;
        }
    }
    let kernel = [1.0, 2.0, 3.0, 2.0, 1.0];
    let mut s = vec![0.0f64; n];
    for (i, sv) in s.iter_mut().enumerate() {
        for (j, k) in kernel.iter().enumerate() {
            let idx = i as isize + j as isize - 2;
            if (0..n as isize).contains(&idx) {
                *sv += raw[idx as usize] * k;
            }
        }
    }

    // Unbiased autocorrelation up to 4 s (covers 4·beat at 60 BPM).
    let max_lag = ((4.0 / DT) as usize).min(n.saturating_sub(2)).max(2);
    let mut r = vec![0.0f64; max_lag + 1];
    for (lag, rv) in r.iter_mut().enumerate().skip(1) {
        let mut acc = 0.0;
        for i in 0..n - lag {
            acc += s[i] * s[i + lag];
        }
        *rv = acc / (n - lag) as f64;
    }

    // Score beat-range lags with subdivision agreement + mild tempo prior.
    let lag_min = ((60.0 / MAX_BPM / DT).round() as usize).max(2);
    let lag_max = ((60.0 / MIN_BPM / DT).round() as usize).min(max_lag);
    if lag_min >= lag_max {
        return (FALLBACK_BPM, 1.0);
    }
    let score = |lag: usize| -> f64 {
        let mut sc = r[lag];
        let half = lag / 2;
        if half >= 1 {
            sc += 0.5 * r[half];
        }
        if 2 * lag <= max_lag {
            sc += 0.5 * r[2 * lag];
        }
        if 4 * lag <= max_lag {
            sc += 0.25 * r[4 * lag];
        }
        let bpm = 60.0 / (lag as f64 * DT);
        let prior = (-(bpm / 115.0).ln().powi(2) / (2.0 * 0.45f64.powi(2))).exp();
        sc * (0.7 + 0.3 * prior)
    };

    let mut best_lag = lag_min;
    let mut best_sc = f64::MIN;
    for lag in lag_min..=lag_max {
        let sc = score(lag);
        if sc > best_sc {
            best_sc = sc;
            best_lag = lag;
        }
    }
    // Runner-up outside the winner's neighborhood → confidence ratio.
    let mut second = f64::MIN;
    for lag in lag_min..=lag_max {
        if best_lag.abs_diff(lag) > 3 {
            second = second.max(score(lag));
        }
    }
    let confidence = if second > 0.0 { best_sc / second } else { 1.0 };

    // Parabolic interpolation for sub-bin period precision.
    let lag_f = if best_lag > lag_min && best_lag < lag_max {
        let (a, b, c) = (score(best_lag - 1), best_sc, score(best_lag + 1));
        let denom = a - 2.0 * b + c;
        let delta =
            if denom.abs() > 1e-12 { (0.5 * (a - c) / denom).clamp(-0.5, 0.5) } else { 0.0 };
        best_lag as f64 + delta
    } else {
        best_lag as f64
    };

    (60.0 / (lag_f * DT), confidence)
}

/// Fine tempo + phase via circular alignment of onsets to the 16th grid.
/// Returns (bpm, phase, magnitude) with grid points at `phase + k * 15/bpm`.
/// The magnitude is the coherence |Σ w·e^(i·2π·t/cell)|: onsets on the grid
/// add fully, onsets between grid points cancel — so it doubles as evidence
/// *against* a hypothesis whose grid the music subdivides.
fn refine(evs: &[Ev], bpm0: f64, rel_window: f64) -> (f64, f64, f64) {
    let steps = ((rel_window / 0.0005).round() as i64).max(0);
    let mut best = (f64::MIN, bpm0, 0.0f64);
    for k in -steps..=steps {
        let bpm = bpm0 * (1.0 + k as f64 * 0.0005);
        let cell = 15.0 / bpm;
        let (mut re, mut im) = (0.0f64, 0.0f64);
        for e in evs {
            let ph = TAU * e.t / cell;
            re += e.w * ph.cos();
            im += e.w * ph.sin();
        }
        let mag = re.hypot(im);
        if mag > best.0 {
            let phase = im.atan2(re) / TAU * cell;
            best = (mag, bpm, phase);
        }
    }
    (best.1, best.2, best.0)
}

/// Pick which grid cell is the downbeat under a meter hypothesis. Returns
/// (origin of bar 0, fit score of the winning offset — comparable across
/// tempo hypotheses on the same onsets after per-cell normalization).
fn downbeat_origin(evs: &[Ev], bpm: f64, phase: f64, spec: &MeterSpec) -> (f64, f64) {
    let cell = 15.0 / bpm;
    let cells = spec.cells;
    let mut score = vec![0.0f64; cells];
    let mut min_cell = i64::MAX;
    for e in evs {
        let idx = ((e.t - phase) / cell).round() as i64;
        min_cell = min_cell.min(idx);
        // Snare evidence is boosted here (beyond its autocorrelation weight):
        // a wrong tempo octave or meter folds *every* onset class onto
        // nominally stronger template slots — the backbeat is the one cue
        // that can't be faked, so it must dominate the vote.
        let (template, class_boost) = match e.class {
            EvClass::Snare => (spec.snare, 3.0),
            _ => (spec.generic, 1.0),
        };
        // Long notes mark harmonic anchors (chord changes sit on bar lines).
        // Absolute threshold, NOT cells of the hypothesis: a tempo-relative
        // one would tag twice as many notes "long" under a doubled reading
        // and bias the octave vote toward faster tempi.
        let w = e.w * class_boost * if e.dur >= 0.6 { 2.5 } else { 1.0 };
        for (o, sv) in score.iter_mut().enumerate() {
            *sv += w * template[(idx - o as i64).rem_euclid(cells as i64) as usize];
        }
    }
    let (best_o, best_fit) = score
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(o, s)| (o as i64, *s))
        .unwrap_or((0, 0.0));
    // Shift by whole bars so the earliest onset lands inside bar 0.
    let rel = min_cell - best_o;
    let bar_shift = rel.div_euclid(cells as i64);
    (phase + (best_o + bar_shift * cells as i64) as f64 * cell, best_fit)
}
