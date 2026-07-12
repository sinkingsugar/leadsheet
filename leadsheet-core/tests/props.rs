//! Phase 1 property tests — the invariants, machine-enforced.
//!
//! 1. Canonical emission (invariant 2): `emit(parse(emit(q))) == emit(q)`
//!    byte-identical, for arbitrary valid QSongs within current semantics
//!    (melodic / chordal / drums, dynamics, swing, ties, `&`-voices,
//!    variants, silent bars).
//! 2. Structural inverse: `parse(emit(q)) == q` exactly — only on a
//!    generator constrained to bucketed velocities (compression is lossy
//!    by bucket, so the constraint lives in the generator, not the
//!    assertion).
//! 3. Compiled roundtrip: `emit → parse → render → ingest → quantize`
//!    note F1 == 1.0 on already-quantized input (no swing — quantization
//!    never produces it; single-stroke drums — subdivisions live below
//!    the grid and re-quantize onto neighbouring cells by design).
//!
//! These properties originally exposed three canonicality bugs (found
//! 2026-07-12), fixed on this branch:
//!
//! 1. `parse` held a single open tie per (track, pitch), so a doubled
//!    pitch in a tied chord (`[EE]16-`) lost one continuation — the tie
//!    map now holds a list, matched by end cell.
//! 2. `parse` *consumed* a tie registration even when its end cell didn't
//!    match the cursor, so a later same-pitch token in an earlier voice
//!    stole the continuation and split the note — non-matching entries
//!    now stay registered.
//! 3. `emit` derived a bar's `@dyn` base from the median of *note*
//!    velocities while marks are per token *group* (a chord = one mark),
//!    and let tie-in segments vote with raw velocities parse can't
//!    reconstruct — the base is now the median over group votes, with
//!    tie-in segments voting at their reconstructed velocity.
//!
//! The original failing seeds stay pinned in `props.proptest-regressions`;
//! the named reproducers at the bottom are the minimal cases for 1 and 2.

use leadsheet_core::grid::{MusicalTime, QNote, QSong, QTrack, QuantizeOptions, Swing};
use leadsheet_core::key::Key;
use leadsheet_core::{chord, emit, ingest, metrics, notation, parse, render};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;
use std::collections::HashMap;

const BUCKETS: [u8; 6] = [32, 48, 64, 80, 96, 112];

/// One template event: a note/chord (melodic, times in ticks) or a hit
/// (drums, where `onset_ticks` is cell-aligned and `dur_ticks` doubles as
/// the stroke count 1..=4).
#[derive(Debug, Clone)]
struct Ev {
    onset_ticks: i64,
    dur_ticks: i64,
    pitches: Vec<u8>,
    vel: u8,
}

#[derive(Debug, Clone, Copy)]
struct GenCfg {
    /// Constrain every note of a track to one dynamic bucket, making the
    /// text a lossless structural inverse.
    bucketed_vels: bool,
    /// Max drum stroke count per cell (1 disables subdivisions).
    max_strokes: u32,
    /// Allow a swing header.
    swing: bool,
    /// Allow sub-16th durations/onsets (32nds, dotted values, tuplet
    /// members). Off for the compiled roundtrip: quantized input is
    /// on-grid by definition.
    fractional: bool,
}

const CANONICAL: GenCfg =
    GenCfg { bucketed_vels: false, max_strokes: 4, swing: true, fractional: true };
const STRUCTURAL: GenCfg =
    GenCfg { bucketed_vels: true, max_strokes: 4, swing: true, fractional: true };
const COMPILED: GenCfg =
    GenCfg { bucketed_vels: false, max_strokes: 1, swing: false, fractional: false };

/// Melodic durations in ticks: whole cells, or (when `fractional`) the
/// interesting sub-16th values — 32nds/64ths, dotted, triplet and
/// quintuplet members — which exercise fraction spelling and canonical
/// tuplet grouping.
fn melodic_dur(cpb: u32, fractional: bool) -> BoxedStrategy<i64> {
    let cells = (1..=cpb + 8).prop_map(|c| c as i64 * 240);
    if fractional {
        prop_oneof![
            4 => cells,
            2 => prop::sample::select(vec![120i64, 60, 360, 600, 180]),
            2 => prop::sample::select(vec![320i64, 160, 80, 40, 192, 96, 48]),
        ]
        .boxed()
    } else {
        cells.boxed()
    }
}

fn melodic_ev(cpb: u32, fractional: bool) -> impl Strategy<Value = Ev> {
    let onset_shift = if fractional {
        prop_oneof![4 => Just(0i64), 1 => Just(120i64), 1 => Just(80i64)].boxed()
    } else {
        Just(0i64).boxed()
    };
    (
        0..cpb,
        onset_shift,
        melodic_dur(cpb, fractional),
        prop::collection::btree_set(24u8..=96, 1..=3),
        1u8..=127,
    )
        .prop_map(|(cell, shift, dur, pitches, vel)| Ev {
            onset_ticks: cell as i64 * 240 + shift,
            dur_ticks: dur,
            pitches: pitches.into_iter().collect(),
            vel,
        })
}

/// Symbols whose canonical voicings the emitter can name back (chord mode).
const CHORD_SYMBOLS: &[&str] = &[
    "C", "Am", "G7", "F/A", "Dm7", "Esus4", "Bdim", "Caug", "Am7b5", "C6", "Bb", "F#m", "Am(2)",
    "G7(4)", "Cdim7", "Dsus2", "Em6", "Fmaj7", "G/B(2)",
];

/// A comping bar: canonical voicings on beat boundaries, one beat each.
fn chordal_template(cpb: u32) -> impl Strategy<Value = Vec<Ev>> {
    let beats = (cpb / 4).max(1) as usize;
    prop::collection::vec(prop::option::of((prop::sample::select(CHORD_SYMBOLS), 1u8..=127)), beats)
        .prop_map(|cols| {
            cols.into_iter()
                .enumerate()
                .filter_map(|(i, c)| {
                    c.map(|(sym, vel)| {
                        let sym = chord::parse_symbol(sym).expect("static symbol");
                        let pitches = chord::voicing(&sym).expect("static symbol voices");
                        Ev { onset_ticks: i as i64 * 960, dur_ticks: 960, pitches, vel }
                    })
                })
                .collect()
        })
}

/// Base templates plus near-identical mutations, so pattern dedup, drum
/// lane diffs (`~P` inheritance) and melodic kinship all get exercised.
fn melodic_templates(cpb: u32, fractional: bool) -> impl Strategy<Value = Vec<Vec<Ev>>> {
    prop_oneof![
        3 => (
            prop::collection::vec(melodic_ev(cpb, fractional), 0..=8),
            prop::collection::vec((any::<prop::sample::Index>(), 24u8..=96), 0..=2),
        )
            .prop_map(|(base, muts)| {
                let mut ts = vec![base.clone()];
                for (idx, new_pitch) in muts {
                    let mut t = base.clone();
                    if !t.is_empty() {
                        let i = idx.index(t.len());
                        t[i].pitches = vec![new_pitch];
                    }
                    ts.push(t);
                }
                ts
            }),
        1 => prop::collection::vec(chordal_template(cpb), 1..=2),
    ]
}

fn drum_pitch() -> impl Strategy<Value = u8> {
    // Mostly the named-lane GM range, sometimes a dNN fallback lane.
    prop_oneof![4 => 35u8..=64, 1 => 0u8..=34, 1 => 65u8..=127]
}

fn drum_ev(cpb: u32, max_strokes: u32) -> impl Strategy<Value = Ev> {
    (0..cpb, 1..=max_strokes, drum_pitch(), 1u8..=127).prop_map(|(cell, strokes, p, vel)| Ev {
        onset_ticks: cell as i64 * 240,
        dur_ticks: strokes as i64,
        pitches: vec![p],
        vel,
    })
}

fn drum_templates(cpb: u32, max_strokes: u32) -> impl Strategy<Value = Vec<Vec<Ev>>> {
    (
        prop::collection::vec(drum_ev(cpb, max_strokes), 0..=10),
        prop::collection::vec(
            (drum_pitch(), prop::collection::vec(drum_ev(cpb, max_strokes), 0..=4)),
            0..=2,
        ),
    )
        .prop_map(|(base, muts)| {
            let mut ts = vec![base.clone()];
            for (pitch, evs) in muts {
                let mut t: Vec<Ev> =
                    base.iter().filter(|e| e.pitches[0] != pitch).cloned().collect();
                t.extend(evs.into_iter().map(|mut e| {
                    e.pitches = vec![pitch];
                    e
                }));
                ts.push(t);
            }
            ts
        })
}

#[derive(Debug, Clone)]
struct TrackGen {
    is_drums: bool,
    program: u8,
    bucket: u8,
    templates: Vec<Vec<Ev>>,
    /// Per bar: which template plays (None = silent bar).
    choices: Vec<Option<usize>>,
}

fn arb_track(cfg: GenCfg, cpb: u32, n_bars: u32) -> impl Strategy<Value = TrackGen> {
    any::<bool>().prop_flat_map(move |is_drums| {
        let templates = if is_drums {
            drum_templates(cpb, cfg.max_strokes).boxed()
        } else {
            melodic_templates(cpb, cfg.fractional).boxed()
        };
        (
            0u8..=127,
            prop::sample::select(&BUCKETS[..]),
            templates,
            prop::collection::vec(prop::option::of(0usize..8), n_bars as usize),
        )
            .prop_map(move |(program, bucket, templates, choices)| TrackGen {
                is_drums,
                program: if is_drums { 0 } else { program },
                bucket,
                templates,
                choices,
            })
    })
}

/// Instantiate a track's bar choices into a valid QTrack: notes clipped to
/// the song, same-pitch overlaps dropped (a lane cell holds one drum hit,
/// and overlapping equal pitches are one voice on one instrument — not a
/// case the format promises to preserve), sorted like the
/// quantizer sorts.
fn assemble(cfg: GenCfg, t: &TrackGen, idx: usize, cpb: u32, n_bars: u32) -> QTrack {
    let total_ticks = n_bars as i64 * cpb as i64 * 240;
    let mut notes: Vec<QNote> = Vec::new();
    for (bar, choice) in t.choices.iter().enumerate() {
        let Some(c) = choice else { continue };
        let tpl = &t.templates[c % t.templates.len()];
        for ev in tpl {
            let onset = bar as i64 * cpb as i64 * 240 + ev.onset_ticks;
            if onset >= total_ticks {
                continue;
            }
            let vel = if cfg.bucketed_vels { t.bucket } else { ev.vel };
            for &p in &ev.pitches {
                if t.is_drums {
                    // dur_ticks is the stroke digit; the hit occupies one cell.
                    notes.push(QNote {
                        pitch: p,
                        onset: MusicalTime(onset),
                        dur: MusicalTime(240),
                        strokes: ev.dur_ticks as u8,
                        vel,
                    });
                } else {
                    let dur = ev.dur_ticks.min(total_ticks - onset).max(1);
                    notes.push(QNote {
                        pitch: p,
                        onset: MusicalTime(onset),
                        dur: MusicalTime(dur),
                        strokes: 1,
                        vel,
                    });
                }
            }
        }
    }
    // Keep-first per pitch: no same-pitch overlaps (drums: one hit per cell).
    notes.sort_by_key(|n| (n.pitch, n.onset, n.dur));
    let mut last_end: HashMap<u8, MusicalTime> = HashMap::new();
    let mut kept: Vec<QNote> = Vec::new();
    for n in notes {
        let end = last_end.entry(n.pitch).or_insert(MusicalTime::ZERO);
        let extent = if t.is_drums { MusicalTime::from_sixteenths(1) } else { n.dur };
        if n.onset >= *end {
            *end = n.onset + extent;
            kept.push(n);
        }
    }
    kept.sort_by(|a, b| a.onset.cmp(&b.onset).then(a.pitch.cmp(&b.pitch)));
    QTrack { name: format!("t{idx}"), program: t.program, is_drums: t.is_drums, notes: kept }
}

fn arb_qsong(cfg: GenCfg) -> impl Strategy<Value = QSong> {
    let meter = prop::sample::select(vec![(4u32, 4u32), (3, 4), (6, 8), (2, 4), (5, 4), (12, 8)]);
    (meter, 1u32..=10).prop_flat_map(move |(meter, n_bars)| {
        let cpb = meter.0 * 16 / meter.1;
        let swing = if cfg.swing {
            prop::option::of((prop::sample::select(vec![8u8, 16]), 50u8..=75)).boxed()
        } else {
            Just(None).boxed()
        };
        (
            "[a-z]{1,8}",
            4000u32..=24000,
            prop::option::of((0u8..12, any::<bool>())),
            swing,
            prop::collection::vec(arb_track(cfg, cpb, n_bars), 1..=4),
        )
            .prop_map(move |(name, bpm100, key, swing, tracks)| QSong {
                name,
                bpm: bpm100 as f64 / 100.0,
                meter,
                key: key.map(|(tonic_pc, minor)| Key { tonic_pc, minor }),
                swing: swing.map(|(level, percent)| Swing { level, percent }),
                n_bars,
                tracks: tracks
                    .iter()
                    .enumerate()
                    .map(|(i, t)| assemble(cfg, t, i, cpb, n_bars))
                    .collect(),
            })
    })
}

fn must_parse(text: &str) -> Result<QSong, TestCaseError> {
    parse::parse(text)
        .map_err(|e| TestCaseError::fail(format!("emitted text failed to parse: {e}\n---\n{text}")))
}

proptest! {
    /// THE primary invariant: emission is canonical, byte for byte.
    #[test]
    fn emission_is_canonical(q in arb_qsong(CANONICAL)) {
        let t1 = emit::emit(&q);
        let q2 = must_parse(&t1)?;
        let t2 = emit::emit(&q2);
        prop_assert_eq!(&t1, &t2, "second generation differs from first");
    }

    /// With velocities constrained to buckets, parse is an exact inverse.
    #[test]
    fn parse_is_structural_inverse_on_bucketed_input(q in arb_qsong(STRUCTURAL)) {
        let text = emit::emit(&q);
        let q2 = must_parse(&text)?;
        prop_assert_eq!(&q2.name, &q.name, "{}", text);
        prop_assert_eq!(q2.bpm, q.bpm, "{}", text);
        prop_assert_eq!(q2.meter, q.meter, "{}", text);
        prop_assert_eq!(q2.key, q.key, "{}", text);
        prop_assert_eq!(q2.swing, q.swing, "{}", text);
        prop_assert_eq!(q2.n_bars, q.n_bars, "silent bars must survive:\n{}", text);
        prop_assert_eq!(q2.tracks.len(), q.tracks.len());
        for (a, b) in q.tracks.iter().zip(&q2.tracks) {
            prop_assert_eq!(&a.name, &b.name);
            prop_assert_eq!(a.program, b.program);
            prop_assert_eq!(a.is_drums, b.is_drums);
            prop_assert_eq!(&a.notes, &b.notes, "track {}:\n{}", &a.name, &text);
        }
    }

    /// Already-quantized input survives the full compiled loop exactly.
    #[test]
    fn compiled_roundtrip_is_exact(
        q in arb_qsong(COMPILED)
            .prop_filter("needs notes", |q| q.tracks.iter().any(|t| !t.notes.is_empty()))
    ) {
        let midi = render::render(&q);
        let song = ingest::ingest_midi(&midi, "prop")
            .map_err(|e| TestCaseError::fail(format!("render output must ingest: {e}")))?;
        let report = metrics::roundtrip(&song, &QuantizeOptions::default())
            .map_err(|e| TestCaseError::fail(format!("roundtrip: {e}")))?;
        prop_assert!(
            report.f1.f1() == 1.0,
            "F1 {:.4} != 1.0 ({} ref, {} hyp, {} matched)\n{}",
            report.f1.f1(), report.f1.ref_count, report.f1.hyp_count, report.f1.matched,
            report.text
        );
    }
}

// ---------------------------------------------------------------------------
// Minimal reproducers for the two tie-tracking bugs (module doc, 1 and 2);
// regressions now that `open_ties` holds a list matched by end cell.

fn one_track(notes: Vec<QNote>, n_bars: u32) -> QSong {
    QSong {
        name: "repro".into(),
        bpm: 100.0,
        meter: (4, 4),
        key: None,
        swing: None,
        n_bars,
        tracks: vec![QTrack { name: "p".into(), program: 0, is_drums: false, notes }],
    }
}

fn assert_canonical(q: &QSong) {
    let t1 = emit::emit(q);
    let q2 = parse::parse(&t1).expect(&t1);
    assert_eq!(emit::emit(&q2), t1, "second generation differs");
}

/// `[EE]16-`: two identical simultaneous notes tied across the barline.
/// Second generation splits them into `E16 & E16-`.
#[test]
fn dup_pitch_tied_chord_is_canonical() {
    let n = |cell: u32, dur: u32| QNote::from_cells(64, cell, dur, 96);
    assert_canonical(&one_track(vec![n(0, 20), n(0, 20)], 2));
}

/// `C16- & z4 C4 z8`: the inner overlapping note consumes the outer note's
/// tie registration; the tie never joins and gen-2 drops the `-`.
#[test]
fn same_pitch_overlap_tie_is_canonical() {
    let n = |cell: u32, dur: u32| QNote::from_cells(60, cell, dur, 96);
    assert_canonical(&one_track(vec![n(0, 20), n(4, 4)], 2));
}

// ---------------------------------------------------------------------------
// Token-level totality: any valid Tok has exactly one spelling that reads
// back as itself — arbitrary tick durations (fractions in lowest terms)
// and tuplet groups with arbitrary spans, fractional included.

fn arb_member() -> impl Strategy<Value = notation::Tok> {
    use notation::{Mark, Tok};
    let mark = prop::sample::select(vec![Mark::None, Mark::Accent, Mark::Ghost]);
    prop_oneof![
        3 => (24u8..=96, mark.clone()).prop_map(|(pitch, mark)| Tok::Note {
            pitch,
            dur: MusicalTime(240),
            tie: false,
            mark,
        }),
        1 => (prop::collection::btree_set(24u8..=96, 2..=3), mark).prop_map(|(ps, mark)| {
            Tok::Chord { pitches: ps.into_iter().collect(), dur: MusicalTime(240), tie: false, mark }
        }),
        1 => Just(Tok::Rest { dur: MusicalTime(240) }),
    ]
}

fn arb_tok() -> impl Strategy<Value = notation::Tok> {
    use notation::{Mark, Tok};
    let mark = prop::sample::select(vec![Mark::None, Mark::Accent, Mark::Ghost]);
    prop_oneof![
        3 => (24u8..=96, 1i64..=4000, any::<bool>(), mark.clone()).prop_map(
            |(pitch, t, tie, mark)| Tok::Note { pitch, dur: MusicalTime(t), tie, mark }
        ),
        2 => (
            prop::collection::btree_set(24u8..=96, 2..=4),
            1i64..=4000,
            any::<bool>(),
            mark,
        )
            .prop_map(|(ps, t, tie, mark)| Tok::Chord {
                pitches: ps.into_iter().collect(),
                dur: MusicalTime(t),
                tie,
                mark,
            }),
        1 => (1i64..=4000).prop_map(|t| Tok::Rest { dur: MusicalTime(t) }),
        2 => (2u32..=24, 1i64..=960, any::<bool>()).prop_flat_map(|(n, step, tie)| {
            prop::collection::vec(arb_member(), n as usize..=n as usize).prop_map(
                move |mut members| {
                    let step = MusicalTime(step);
                    for m in &mut members {
                        match m {
                            Tok::Note { dur, .. }
                            | Tok::Chord { dur, .. }
                            | Tok::Rest { dur } => *dur = step,
                            Tok::Tuplet { .. } => unreachable!(),
                        }
                    }
                    let tie = tie && !matches!(members.last(), Some(Tok::Rest { .. }));
                    Tok::Tuplet { n, members, span: step * n as i64, tie }
                },
            )
        }),
    ]
}

proptest! {
    /// Every token — fractional durations, tuplet groups with fractional
    /// spans — spells to text that parses back to the identical Tok.
    #[test]
    fn token_spelling_is_total_and_invertible(t in arb_tok()) {
        let s = notation::emit_token(&t);
        let back = notation::parse_tokens(&s)
            .map_err(|e| TestCaseError::fail(format!("{s:?} failed to parse: {e}")))?;
        prop_assert_eq!(&back, &std::slice::from_ref(&t), "via {}", &s);
    }
}
