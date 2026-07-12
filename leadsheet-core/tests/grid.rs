//! M1 acceptance (plan Layer 1): on known-BPM material with human-ish jitter,
//! recovered tempo within ±1 BPM and >95% of onsets snapped to the correct
//! grid cell, with the downbeat on the bar line.

use leadsheet_core::grid::{QuantizeOptions, TempoSource, quantize};
use leadsheet_core::model::{RawNote, RawSong, RawTrack};

/// Deterministic jitter source (no rand dep).
struct Lcg(u64);

impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [-ms, +ms], as seconds.
    fn jitter(&mut self, ms: f64) -> f64 {
        (self.next_f64() * 2.0 - 1.0) * ms * 1e-3
    }
}

/// One track's ground truth: (global cell, dur cells, pitch).
type Truth = Vec<(u32, u32, u8)>;

struct SynthSong {
    song: RawSong,
    /// Per track, sorted like the ingested notes: (true global cell, pitch).
    truth: Vec<Vec<(u32, u8)>>,
}

/// A plausible 4-piece: bass anchored on bar lines (long low notes resolve
/// the half-bar downbeat ambiguity, like a real chord change), rock kit with
/// a syncopated kick, 8th hats, and a 16th-note melody.
fn synth_song(bpm: f64, offset_sec: f64, bars: u32, jitter_ms: f64, seed: u64) -> SynthSong {
    let cell = 60.0 / (bpm * 4.0);
    let mut rng = Lcg(seed);

    let bass: Truth = (0..bars)
        .flat_map(|b| {
            let root: u8 = [33, 40, 36, 31][(b % 4) as usize];
            vec![(b * 16, 12, root), (b * 16 + 12, 4, root + 7)]
        })
        .collect();
    let drums: Truth = (0..bars)
        .flat_map(|b| {
            let mut v = vec![
                (b * 16, 2, 36),
                (b * 16 + 8, 2, 36),
                (b * 16 + 14, 2, 36), // syncopated kick
                (b * 16 + 4, 2, 38),
                (b * 16 + 12, 2, 38),
            ];
            v.extend((0..8).map(|i| (b * 16 + i * 2, 1, 42u8)));
            v
        })
        .collect();
    let melody: Truth = (0..bars)
        .flat_map(|b| {
            const CELLS: [u32; 8] = [0, 2, 3, 6, 8, 11, 12, 14];
            const PITCH: [u8; 8] = [69, 72, 74, 76, 72, 74, 69, 67];
            CELLS.iter().zip(PITCH).map(move |(&c, p)| (b * 16 + c, 2, p)).collect::<Vec<_>>()
        })
        .collect();

    let mut truth = Vec::new();
    let mut mk = |name: &str, program: u8, is_drums: bool, t: Truth| {
        let mut tagged: Vec<(f64, u32, u8, f64)> = t
            .into_iter()
            .map(|(c, d, p)| {
                (offset_sec + c as f64 * cell + rng.jitter(jitter_ms), c, p, d as f64 * cell)
            })
            .collect();
        tagged.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.2.cmp(&b.2)));
        truth.push(tagged.iter().map(|&(_, c, p, _)| (c, p)).collect());
        RawTrack {
            name: name.into(),
            program,
            is_drums,
            notes: tagged
                .into_iter()
                .map(|(t, _, p, d)| RawNote { pitch: p, onset: t.max(0.0), dur: d, vel: 96 })
                .collect(),
        }
    };

    let tracks = vec![
        mk("bass", 33, false, bass),
        mk("drums", 0, true, drums),
        mk("melody", 81, false, melody),
    ];
    SynthSong {
        song: RawSong { name: "synth".into(), tracks, source_bpm: None, source_meter: None },
        truth,
    }
}

/// Recovered cells must equal truth up to one constant whole-bar shift.
fn assert_grid_recovery(s: &SynthSong, bpm_expected: f64) {
    let (q, report) =
        quantize(&s.song, &QuantizeOptions { infer_tempo: true, ..Default::default() });
    assert_eq!(report.tempo_source, TempoSource::Inferred);
    assert!(
        (report.bpm - bpm_expected).abs() <= 1.0,
        "bpm: expected {bpm_expected}, got {:.3}",
        report.bpm
    );

    // Pair truth and quantized notes per (track, pitch) — both sides sorted
    // by cell — so simultaneous hits and the occasional ±1-cell miss (which
    // the 95% criterion below counts) can't misalign the comparison.
    let mut deltas: std::collections::HashMap<i64, usize> = std::collections::HashMap::new();
    let mut pairs = Vec::new();
    for (track, truth) in q.tracks.iter().zip(&s.truth) {
        assert_eq!(track.notes.len(), truth.len(), "no notes dropped ({})", track.name);
        let mut true_by_pitch: std::collections::HashMap<u8, Vec<u32>> = Default::default();
        for &(cell, pitch) in truth {
            true_by_pitch.entry(pitch).or_default().push(cell);
        }
        let mut got_by_pitch: std::collections::HashMap<u8, Vec<u32>> = Default::default();
        for n in &track.notes {
            got_by_pitch.entry(n.pitch).or_default().push(n.cell());
        }
        for (pitch, mut tv) in true_by_pitch {
            let mut gv = got_by_pitch
                .remove(&pitch)
                .unwrap_or_else(|| panic!("pitch {pitch} missing from quantized {}", track.name));
            assert_eq!(gv.len(), tv.len(), "pitch {pitch} count ({})", track.name);
            tv.sort_unstable();
            gv.sort_unstable();
            for (g, t) in gv.iter().zip(&tv) {
                let d = *g as i64 - *t as i64;
                *deltas.entry(d).or_default() += 1;
                pairs.push(d);
            }
        }
    }
    let (&mode, _) = deltas.iter().max_by_key(|(_, c)| **c).unwrap();
    assert_eq!(mode % 16, 0, "downbeat must land on a bar line (delta {mode})");
    let correct = pairs.iter().filter(|&&d| d == mode).count();
    let rate = correct as f64 / pairs.len() as f64;
    assert!(rate > 0.95, "grid cell accuracy {rate:.3} at {bpm_expected} BPM (need >0.95)");
}

#[test]
fn recovers_tempo_and_grid_across_bpms() {
    for &bpm in &[72.0, 95.0, 128.0, 174.0] {
        let s = synth_song(bpm, 0.0, 16, 10.0, 0xC0FFEE ^ bpm.to_bits());
        assert_grid_recovery(&s, bpm);
    }
}

#[test]
fn recovers_phase_with_leading_silence() {
    let s = synth_song(110.0, 0.37, 16, 10.0, 42);
    assert_grid_recovery(&s, 110.0);
}

#[test]
fn declared_tempo_is_exact() {
    // Grid-perfect material with a declared tempo: no inference, zero residual.
    let mut s = synth_song(120.0, 0.0, 8, 0.0, 7);
    s.song.source_bpm = Some(120.0);
    let (q, report) = quantize(&s.song, &QuantizeOptions::default());
    assert_eq!(report.tempo_source, TempoSource::Declared);
    assert_eq!(report.bpm, 120.0);
    assert!(report.max_abs_residual_ms < 1e-6);
    for (track, truth) in q.tracks.iter().zip(&s.truth) {
        for (n, &(true_cell, _)) in track.notes.iter().zip(truth) {
            assert_eq!(n.cell(), true_cell);
        }
    }
    assert_eq!(q.n_bars, 8);
}

#[test]
fn bpm_override_estimates_phase() {
    let s = synth_song(140.0, 0.25, 12, 8.0, 99);
    let (_, report) =
        quantize(&s.song, &QuantizeOptions { bpm_override: Some(140.0), ..Default::default() });
    assert_eq!(report.tempo_source, TempoSource::Override);
    assert!((report.bpm - 140.0).abs() < 0.2);
    // Origin should sit near the true 0.25 s start (mod whole bars).
    let bar = 16.0 * 60.0 / (140.0 * 4.0);
    let rel = (report.origin - 0.25).rem_euclid(bar);
    let dist = rel.min(bar - rel);
    assert!(dist < 0.05, "origin {:.3}, expected ≈0.25 mod bar (dist {dist:.3})", report.origin);
}

#[test]
fn auto_switch_when_declared_tempo_lies() {
    // Played at 125 against a DAW that stamped 120 (the Matrix.mid case).
    let mut s = synth_song(125.0, 0.2, 16, 8.0, 77);
    s.song.source_bpm = Some(120.0);
    let (_, report) = quantize(&s.song, &QuantizeOptions::default());
    match report.tempo_source {
        TempoSource::AutoInferred { declared_bpm, declared_mean_ms } => {
            assert_eq!(declared_bpm, 120.0);
            assert!(declared_mean_ms > 25.0);
        }
        other => panic!("expected AutoInferred, got {other:?} at {:.2} BPM", report.bpm),
    }
    assert!((report.bpm - 125.0).abs() <= 1.0, "bpm {:.2}", report.bpm);

    // Opt-out keeps the declared grid.
    let (_, report) = quantize(&s.song, &QuantizeOptions { no_infer: true, ..Default::default() });
    assert_eq!(report.tempo_source, TempoSource::Declared);
    assert_eq!(report.bpm, 120.0);

    // And an honest declared tempo is left alone.
    let mut s = synth_song(120.0, 0.0, 16, 5.0, 78);
    s.song.source_bpm = Some(120.0);
    let (_, report) = quantize(&s.song, &QuantizeOptions::default());
    assert_eq!(report.tempo_source, TempoSource::Declared);
}

/// A waltz: bass root on beat 1, chord stabs on 2 and 3, kick/snare-snare,
/// quarter-note melody. 12 cells per bar.
fn synth_waltz(bpm: f64, bars: u32, jitter_ms: f64, seed: u64) -> RawSong {
    let cell = 60.0 / (bpm * 4.0);
    let mut rng = Lcg(seed);
    let mut bass = Vec::new();
    let mut drums = Vec::new();
    let mut piano = Vec::new();
    for b in 0..bars {
        let base = (b * 12) as f64 * cell;
        let root: u8 = [38, 43, 45, 43][(b % 4) as usize];
        bass.push(RawNote {
            pitch: root,
            onset: base + rng.jitter(jitter_ms),
            dur: 11.0 * cell,
            vel: 96,
        });
        drums.push(RawNote { pitch: 36, onset: base + rng.jitter(jitter_ms), dur: 0.08, vel: 100 });
        for beat in [4u32, 8] {
            drums.push(RawNote {
                pitch: 38,
                onset: base + beat as f64 * cell + rng.jitter(jitter_ms),
                dur: 0.06,
                vel: 80,
            });
            for p in [62u8, 65, 69] {
                piano.push(RawNote {
                    pitch: p,
                    onset: base + beat as f64 * cell + rng.jitter(jitter_ms),
                    dur: 3.0 * cell,
                    vel: 85,
                });
            }
        }
    }
    let mk = |name: &str, program: u8, is_drums: bool, mut notes: Vec<RawNote>| {
        notes.sort_by(|a, b| a.onset.total_cmp(&b.onset));
        RawTrack { name: name.into(), program, is_drums, notes }
    };
    RawSong {
        name: "waltz".into(),
        tracks: vec![
            mk("bass", 32, false, bass),
            mk("drums", 0, true, drums),
            mk("piano", 0, false, piano),
        ],
        source_bpm: None,
        source_meter: None,
    }
}

#[test]
fn detects_three_four() {
    let song = synth_waltz(140.0, 16, 8.0, 5150);
    let (q, report) = quantize(&song, &QuantizeOptions { infer_tempo: true, ..Default::default() });
    assert!((report.bpm - 140.0).abs() <= 1.5, "bpm {:.2}", report.bpm);
    assert_eq!(q.meter, (3, 4), "waltz must be 3/4");
    assert_eq!(q.cells_per_bar(), 12);
    // Downbeat on the bar line: bass onsets at cell ≡ 0 (mod 12).
    let bass = &q.tracks[0];
    let on_downbeat =
        bass.notes.iter().filter(|n| n.cell() % 12 == 0).count() as f64 / bass.notes.len() as f64;
    assert!(on_downbeat > 0.9, "downbeat rate {on_downbeat:.2}");
}

#[test]
fn detects_six_eight() {
    // A jig: kick on 1, snare on the second dotted-quarter beat (cell 6),
    // 8th-note pulse on cells 0,2,4,6,8,10.
    let bpm = 120.0;
    let cell = 60.0 / (bpm * 4.0);
    let mut rng = Lcg(303);
    let mut drums = Vec::new();
    let mut bass = Vec::new();
    let mut lead = Vec::new();
    for b in 0..16u32 {
        let base = (b * 12) as f64 * cell;
        drums.push(RawNote { pitch: 36, onset: base + rng.jitter(6.0), dur: 0.08, vel: 100 });
        drums.push(RawNote {
            pitch: 38,
            onset: base + 6.0 * cell + rng.jitter(6.0),
            dur: 0.06,
            vel: 90,
        });
        for i in 0..6u32 {
            drums.push(RawNote {
                pitch: 42,
                onset: base + (i * 2) as f64 * cell + rng.jitter(6.0),
                dur: 0.05,
                vel: 70,
            });
        }
        bass.push(RawNote { pitch: 43, onset: base + rng.jitter(6.0), dur: 11.0 * cell, vel: 96 });
        for (i, p) in [67u8, 71, 74, 67, 72, 74].iter().enumerate() {
            lead.push(RawNote {
                pitch: *p,
                onset: base + (i as u32 * 2) as f64 * cell + rng.jitter(6.0),
                dur: 2.0 * cell,
                vel: 90,
            });
        }
    }
    let mk = |name: &str, program: u8, is_drums: bool, mut notes: Vec<RawNote>| {
        notes.sort_by(|a, b| a.onset.total_cmp(&b.onset));
        RawTrack { name: name.into(), program, is_drums, notes }
    };
    let song = RawSong {
        name: "jig".into(),
        tracks: vec![
            mk("bass", 32, false, bass),
            mk("drums", 0, true, drums),
            mk("lead", 73, false, lead),
        ],
        source_bpm: None,
        source_meter: None,
    };
    let (q, report) = quantize(&song, &QuantizeOptions { infer_tempo: true, ..Default::default() });
    assert_eq!(q.meter, (6, 8), "jig must be 6/8, got {:?} at {:.2}", q.meter, report.bpm);
}

#[test]
fn declared_meter_wins_over_detection() {
    let mut song = synth_waltz(140.0, 8, 0.0, 9);
    song.source_bpm = Some(140.0);
    song.source_meter = Some((3, 4));
    let (q, report) = quantize(&song, &QuantizeOptions::default());
    assert_eq!(report.tempo_source, TempoSource::Declared);
    assert_eq!(q.meter, (3, 4));
}

#[test]
fn quantizer_never_drops_notes() {
    // Property (plan test #5): every ingested note survives quantization,
    // even with hostile timing.
    let mut rng = Lcg(1234);
    let notes: Vec<RawNote> = (0..300)
        .map(|_| RawNote {
            pitch: 30 + (rng.next_f64() * 60.0) as u8,
            onset: rng.next_f64() * 60.0,
            dur: 0.01 + rng.next_f64() * 3.0,
            vel: 96,
        })
        .collect();
    let song = RawSong {
        name: "noise".into(),
        tracks: vec![RawTrack { name: "x".into(), program: 0, is_drums: false, notes }],
        source_bpm: None,
        source_meter: None,
    };
    let (q, _) = quantize(&song, &QuantizeOptions { infer_tempo: true, ..Default::default() });
    assert_eq!(q.tracks[0].notes.len(), 300);
    assert!(q.tracks[0].notes.iter().all(|n| n.dur_cells() >= 1));
    let max_end = q.tracks[0].notes.iter().map(|n| n.cell() + n.dur_cells()).max().unwrap();
    assert!(q.n_bars * q.cells_per_bar() >= max_end);
}
