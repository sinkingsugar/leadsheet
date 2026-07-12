//! M2 acceptance: the round trip closes. compress → render → re-ingest,
//! note F1 ≥ 0.95 on quantized-source material (plan test #1), and the
//! text format is an exact inverse pair (emit ∘ parse = id).

use leadsheet_core::grid::{QNote, QSong, QTrack, QuantizeOptions};
use leadsheet_core::metrics::roundtrip;
use leadsheet_core::model::{RawNote, RawSong, RawTrack};
use leadsheet_core::{emit, parse, render};

/// A QSong exercising the nasty parts of the format: chords, overlapping
/// voices, ties across bars (incl. a 3-bar note), drums, gaps, and a
/// duplicate simultaneous note.
fn tricky_qsong() -> QSong {
    let n = |pitch: u8, cell: u32, dur_cells: u32| QNote { pitch, cell, dur_cells, vel: 96 };
    QSong {
        name: "tricky".into(),
        bpm: 132.5,
        meter: (4, 4),
        n_bars: 5,
        tracks: vec![
            QTrack {
                name: "piano".into(),
                program: 0,
                is_drums: false,
                notes: vec![
                    // Chord (same onset+dur) with a doubled pitch.
                    n(60, 0, 4),
                    n(64, 0, 4),
                    n(67, 0, 4),
                    n(64, 0, 4),
                    // Overlapping voice: sustained bass note under moving line.
                    n(48, 4, 12),
                    n(72, 6, 2),
                    n(74, 8, 2),
                    // Tie across bar 1→2, plus a 3-bar pad note (bars 2..5).
                    n(65, 12, 8),
                    n(55, 20, 40),
                    // Off-grid-ish leftovers after a gap.
                    n(61, 67, 1),
                ],
            },
            QTrack {
                name: "drums".into(),
                program: 0,
                is_drums: true,
                notes: vec![n(36, 0, 2), n(42, 0, 1), n(38, 4, 2), n(36, 8, 2), n(46, 14, 2)],
            },
        ],
    }
}

fn structural(q: &QSong) -> Vec<(String, Vec<(u8, u32, u32)>)> {
    q.tracks
        .iter()
        .map(|t| {
            let mut ns: Vec<_> = t.notes.iter().map(|n| (n.pitch, n.cell, n.dur_cells)).collect();
            ns.sort_unstable();
            (t.name.clone(), ns)
        })
        .collect()
}

#[test]
fn emit_parse_is_identity() {
    let q = tricky_qsong();
    let text = emit::emit(&q);
    let q2 = parse::parse(&text).expect(&text);
    assert_eq!(q2.bpm, q.bpm);
    assert_eq!(q2.meter, q.meter);
    assert_eq!(q2.n_bars, q.n_bars);
    assert_eq!(structural(&q2), structural(&q), "text:\n{text}");
    // And a second generation is byte-identical (emitter is canonical).
    assert_eq!(emit::emit(&q2), text);
}

#[test]
fn render_output_is_valid_midi_with_all_notes() {
    // Property (plan test #5): decompressor output is always valid MIDI.
    let q = tricky_qsong();
    let bytes = render::render(&q);
    let back = leadsheet_core::ingest::ingest_midi(&bytes, "back").unwrap();
    assert_eq!(back.note_count(), q.tracks.iter().map(|t| t.notes.len()).sum::<usize>());
    assert_eq!(back.source_bpm.map(|b| b.round()), Some(133.0)); // 132.5 rounds via µs/qn
    let drums = back.tracks.iter().find(|t| t.is_drums).unwrap();
    assert_eq!(drums.notes.len(), 5);
}

/// Deterministic jitter (same generator as the grid tests).
struct Lcg(u64);

impl Lcg {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Seconds-domain synthetic band, same shape as the M1 material.
fn synth_raw(bpm: f64, bars: u32, jitter_ms: f64, seed: u64, declared: bool) -> RawSong {
    let cell = 60.0 / (bpm * 4.0);
    let mut rng = Lcg(seed);
    let mut jit = |mult: f64| (rng.next_f64() * 2.0 - 1.0) * jitter_ms * 1e-3 * mult;

    let mut bass = Vec::new();
    let mut drums = Vec::new();
    let mut melody = Vec::new();
    for b in 0..bars {
        let base = (b * 16) as f64 * cell;
        let root: u8 = [33, 40, 36, 31][(b % 4) as usize];
        bass.push(RawNote { pitch: root, onset: base + jit(1.0), dur: 12.0 * cell, vel: 96 });
        bass.push(RawNote {
            pitch: root + 7,
            onset: base + 12.0 * cell + jit(1.0),
            dur: 4.0 * cell,
            vel: 96,
        });
        for (c, p) in [(0u32, 36u8), (8, 36), (14, 36), (4, 38), (12, 38)] {
            drums.push(RawNote {
                pitch: p,
                onset: base + c as f64 * cell + jit(1.0),
                dur: 0.08,
                vel: 100,
            });
        }
        for i in 0..8u32 {
            drums.push(RawNote {
                pitch: 42,
                onset: base + (i * 2) as f64 * cell + jit(1.0),
                dur: 0.05,
                vel: 80,
            });
        }
        const CELLS: [u32; 8] = [0, 2, 3, 6, 8, 11, 12, 14];
        const PITCH: [u8; 8] = [69, 72, 74, 76, 72, 74, 69, 67];
        for (c, p) in CELLS.iter().zip(PITCH) {
            melody.push(RawNote {
                pitch: p,
                onset: base + *c as f64 * cell + jit(1.0),
                dur: 2.0 * cell,
                vel: 90,
            });
        }
    }
    let mk = |name: &str, program: u8, is_drums: bool, mut notes: Vec<RawNote>| {
        notes.sort_by(|a, b| a.onset.total_cmp(&b.onset));
        RawTrack { name: name.into(), program, is_drums, notes }
    };
    RawSong {
        name: "synth".into(),
        tracks: vec![
            mk("bass", 33, false, bass),
            mk("drums", 0, true, drums),
            mk("melody", 81, false, melody),
        ],
        source_bpm: declared.then_some(bpm),
    }
}

#[test]
fn roundtrip_exact_on_declared_tempo() {
    let song = synth_raw(120.0, 8, 0.0, 1, true);
    let report = roundtrip(&song, &QuantizeOptions::default()).unwrap();
    assert_eq!(report.f1.f1(), 1.0, "text:\n{}", report.text);
    assert_eq!(report.f1.ref_count, report.f1.hyp_count);
}

#[test]
fn roundtrip_with_inferred_tempo_and_jitter() {
    // The M2 acceptance number: F1 ≥ 0.95 on quantized-source material.
    let song = synth_raw(128.0, 16, 10.0, 0xBEEF, false);
    let opts = QuantizeOptions { infer_tempo: true, ..Default::default() };
    let report = roundtrip(&song, &opts).unwrap();
    assert!((report.quant.bpm - 128.0).abs() <= 1.0, "bpm {:.2}", report.quant.bpm);
    assert!(report.f1.f1() >= 0.95, "F1 {:.4}", report.f1.f1());
    // Explicit text (no dedup yet) should already beat the naive event list.
    assert!(report.ratio_vs_naive() > 1.5, "ratio {:.2}", report.ratio_vs_naive());
}

#[test]
fn parse_rejects_malformed() {
    let ok = "# song: x  tempo: 120.00  meter: 4/4  grid: 1/16\n# instruments: p:0\nb1 p | C16 |\n";
    assert!(parse::parse(ok).is_ok());
    for bad in [
        "b1 p | C16 |\n",                                                       // no header
        "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\nb1 q | C16 |",  // unknown inst
        "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\nb1 p | C15 |",  // short bar
        "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\nb1 p | C17 |",  // overflow
        "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\nb1 p | Q16 |",  // bad pitch
        "# song: x  tempo: nope\n# instruments: p:0\nb1 p | C16 |",             // bad tempo
    ] {
        assert!(parse::parse(bad).is_err(), "should reject: {bad}");
    }
}
