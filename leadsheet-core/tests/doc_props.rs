//! Triage-2 A4 — the Document-layer canonicality theorem, machine-
//! enforced (the Phase-2 sibling of `emission_is_canonical` in props.rs):
//! for any Document that `validate()` accepts — hostile-but-legal names
//! and labels, interleaved rows and direct bars, drum variants, kinship,
//! fractional durations, tuplet groups (inexact divisions included),
//! empty instrument lists —
//!
//! 1. `emit_document(d)` parses back (`parse_document` succeeds);
//! 2. emission is a fixpoint across the hop:
//!    `emit(parse(emit(d))) == emit(d)` byte for byte;
//! 3. resolution is preserved: `d.resolve()` and the reparse's
//!    `resolve()` compile to identical tracks and notes.
//!
//! Document *equality* through the hop is deliberately not asserted:
//! emission canonicalizes spelling (three equal tuplet-shaped durations
//! group into `(3 …)S`, and so on), so only already-canonical Documents
//! reparse to themselves. What the theorem forbids is silent loss or
//! drift of music or structure through the text — every triage-2 B-item
//! was an instance of a validate() hole this property now guards.

use leadsheet_core::doc::{
    ChordCol, DirectItem, Document, DrumsBody, Header, Instrument, MelodicBar, PatternBody,
    PatternDef, Row, TimelineItem,
};
use leadsheet_core::drums::LANE_D4;
use leadsheet_core::grid::{MusicalTime, Swing};
use leadsheet_core::key::Key;
use leadsheet_core::notation::{Mark, Tok, tuplet_boundary};
use leadsheet_core::{chord, emit, parse};
use proptest::prelude::*;
use proptest::test_runner::TestCaseError;

const BUCKETS: [u8; 6] = [32, 48, 64, 80, 96, 112];

/// Durations that exercise whole cells, fractions (32nds, dotted values,
/// 64ths) and tuplet-shaped values that `detect_tuplets` may regroup.
const DUR_POOL: [i64; 8] = [240, 480, 960, 120, 360, 320, 80, 1440];

/// Symbols whose spelling round-trips through `symbol_to_string` (the
/// same guarantee props.rs leans on).
const CHORD_SYMBOLS: &[&str] = &["C", "Am", "G7", "Fmaj7", "Dm7", "Bdim", "F/A", "C6"];

fn mark_of(m: u8) -> Mark {
    [Mark::None, Mark::Accent, Mark::Ghost][(m % 3) as usize]
}

// ---------------------------------------------------------------------------
// Seeds: plain data from proptest; `build_document` turns them into a
// *valid* Document deterministically (fills bars exactly, only references
// earlier same-track patterns, keeps ids unique).

#[derive(Debug, Clone)]
enum TokSeed {
    Note { pitch: u8, dur: u8, tie: bool, mark: u8 },
    Chord { pitches: Vec<u8>, dur: u8, tie: bool, mark: u8 },
    Rest { dur: u8 },
    Tuplet { n: u8, span: u8, members: Vec<(u8, u8, u8)>, tie: bool },
}

fn tok_seed() -> impl Strategy<Value = TokSeed> {
    prop_oneof![
        4 => (24u8..=104, any::<u8>(), any::<bool>(), 0u8..3)
            .prop_map(|(pitch, dur, tie, mark)| TokSeed::Note { pitch, dur, tie, mark }),
        2 => (prop::collection::vec(24u8..=104, 1..=3), any::<u8>(), any::<bool>(), 0u8..3)
            .prop_map(|(pitches, dur, tie, mark)| TokSeed::Chord { pitches, dur, tie, mark }),
        2 => any::<u8>().prop_map(|dur| TokSeed::Rest { dur }),
        2 => (
            any::<u8>(),
            any::<u8>(),
            prop::collection::vec((any::<u8>(), 24u8..=104, 0u8..3), 1..=7),
            any::<bool>(),
        )
            .prop_map(|(n, span, members, tie)| TokSeed::Tuplet { n, span, members, tie }),
    ]
}

/// Fill exactly `bar` ticks from the seeds; pad the tail with a rest.
fn build_voice(seeds: &[TokSeed], bar: i64) -> Vec<Tok> {
    let mut toks = Vec::new();
    let mut remaining = bar;
    for s in seeds {
        if remaining == 0 {
            break;
        }
        match s {
            TokSeed::Note { pitch, dur, tie, mark } => {
                let d = DUR_POOL[*dur as usize % DUR_POOL.len()].min(remaining);
                toks.push(Tok::Note {
                    pitch: *pitch,
                    dur: MusicalTime(d),
                    tie: *tie,
                    mark: mark_of(*mark),
                });
                remaining -= d;
            }
            TokSeed::Chord { pitches, dur, tie, mark } => {
                let d = DUR_POOL[*dur as usize % DUR_POOL.len()].min(remaining);
                toks.push(Tok::Chord {
                    pitches: pitches.clone(),
                    dur: MusicalTime(d),
                    tie: *tie,
                    mark: mark_of(*mark),
                });
                remaining -= d;
            }
            TokSeed::Rest { dur } => {
                let d = DUR_POOL[*dur as usize % DUR_POOL.len()].min(remaining);
                toks.push(Tok::Rest { dur: MusicalTime(d) });
                remaining -= d;
            }
            TokSeed::Tuplet { n, span, members, tie } => {
                let n = [2u32, 3, 5, 7][*n as usize % 4];
                let span_ticks = (240 * (1 + *span as i64 % 8)).min(remaining);
                if span_ticks < n as i64 {
                    continue;
                }
                let span = MusicalTime(span_ticks);
                let ms: Vec<Tok> = (0..n)
                    .map(|i| {
                        let (kind, pitch, mark) = members[i as usize % members.len()];
                        let dur = tuplet_boundary(span, n, i + 1) - tuplet_boundary(span, n, i);
                        match kind % 3 {
                            0 => Tok::Note { pitch, dur, tie: false, mark: mark_of(mark) },
                            1 => Tok::Chord {
                                pitches: vec![pitch, 24 + (pitch + 7) % 80],
                                dur,
                                tie: false,
                                mark: mark_of(mark),
                            },
                            _ => Tok::Rest { dur },
                        }
                    })
                    .collect();
                let tie = *tie && !matches!(ms.last(), Some(Tok::Rest { .. }));
                toks.push(Tok::Tuplet { n, members: ms, span, tie });
                remaining -= span_ticks;
            }
        }
    }
    if remaining > 0 {
        toks.push(Tok::Rest { dur: MusicalTime(remaining) });
    }
    toks
}

fn build_cols(seeds: &[u8], beats: usize) -> Vec<ChordCol> {
    let mut have = false;
    (0..beats)
        .map(|i| {
            let b = seeds[i % seeds.len()];
            match b % 8 {
                0..=4 => {
                    have = true;
                    let sym = CHORD_SYMBOLS[(b / 8) as usize % CHORD_SYMBOLS.len()];
                    ChordCol::Sym(chord::parse_symbol(sym).expect("static symbol"))
                }
                5 | 6 if have => ChordCol::Hold,
                _ => {
                    have = false;
                    ChordCol::Rest
                }
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
struct BodySeed {
    kind: u8,
    /// Melodic: bars → voices → token seeds.
    bars: Vec<Vec<Vec<TokSeed>>>,
    /// Chordal: bars → per-beat seeds.
    cols: Vec<Vec<u8>>,
    /// Drums: distinct pitches → cell seeds.
    lanes: Vec<(u8, Vec<u8>)>,
}

fn body_seed() -> impl Strategy<Value = BodySeed> {
    (
        any::<u8>(),
        prop::collection::vec(
            prop::collection::vec(prop::collection::vec(tok_seed(), 0..5), 1..=2),
            1..=2,
        ),
        prop::collection::vec(prop::collection::vec(any::<u8>(), 1..=8), 1..=2),
        prop::collection::btree_map(30u8..=70, prop::collection::vec(any::<u8>(), 1..=8), 0..=3),
    )
        .prop_map(|(kind, bars, cols, lanes)| BodySeed {
            kind,
            bars,
            cols,
            lanes: lanes.into_iter().collect(),
        })
}

fn build_body(seed: &BodySeed, is_drums: bool, cpb: u32) -> PatternBody {
    let bar_ticks = cpb as i64 * 240;
    if is_drums {
        PatternBody::Drums(DrumsBody {
            variant_base: None,
            lanes: seed
                .lanes
                .iter()
                .map(|(p, cells)| {
                    let full: Vec<u8> =
                        (0..cpb as usize).map(|i| cells[i % cells.len()] % (LANE_D4 + 1)).collect();
                    (*p, full)
                })
                .collect(),
        })
    } else if seed.kind.is_multiple_of(2) {
        PatternBody::Melodic(
            seed.bars
                .iter()
                .map(|voices| MelodicBar {
                    voices: voices.iter().map(|v| build_voice(v, bar_ticks)).collect(),
                })
                .collect(),
        )
    } else {
        PatternBody::Chordal(seed.cols.iter().map(|s| build_cols(s, (cpb / 4) as usize)).collect())
    }
}

type PatSeed = (u8, u8, Option<u8>, Option<u8>, BodySeed);

#[derive(Debug, Clone)]
enum ItemSeed {
    Row { label: Option<String>, picks: Vec<u8>, reps: u8 },
    Direct { bar: u8, track: u8, vel: u8, variant: Option<u8>, body: BodySeed },
}

fn item_seed() -> impl Strategy<Value = ItemSeed> {
    let label = prop::option::of(prop_oneof![
        2 => "[a-z]{1,6}",
        // Hostile but legal: inner spaces, ':', ']', '&', "'", '-'.
        1 => "[a-z][a-z0-9 :&'\\]\\-]{0,6}[a-z0-9:]",
    ]);
    prop_oneof![
        2 => (label, prop::collection::vec(any::<u8>(), 0..=3), any::<u8>())
            .prop_map(|(label, picks, reps)| ItemSeed::Row { label, picks, reps }),
        1 => (any::<u8>(), any::<u8>(), any::<u8>(), prop::option::of(any::<u8>()), body_seed())
            .prop_map(|(bar, track, vel, variant, body)| ItemSeed::Direct {
                bar, track, vel, variant, body,
            }),
    ]
}

fn arb_header() -> impl Strategy<Value = Header> {
    let name = prop_oneof![
        3 => "[a-z]{1,6}".boxed(),
        // Hostile but legal: inner spaces and odd chars, trimmed ends.
        1 => "[a-z][a-z0-9 &'\\]\\-]{0,8}[a-z0-9]".boxed(),
    ];
    (
        name,
        3000u32..=30000,
        prop::sample::select(vec![(4u32, 4u32), (3, 4), (6, 8), (5, 4), (2, 4), (12, 8), (7, 8)]),
        prop::option::of((0u8..12, any::<bool>())),
        prop::option::of((prop::sample::select(vec![8u8, 16]), 50u8..=75)),
    )
        .prop_map(|(name, bpm100, meter, key, swing)| Header {
            name,
            bpm: bpm100 as f64 / 100.0,
            meter,
            key: key.map(|(tonic_pc, minor)| Key { tonic_pc, minor }),
            swing: swing.map(|(level, percent)| Swing { level, percent }),
        })
}

fn arb_instruments() -> impl Strategy<Value = Vec<Instrument>> {
    (
        prop::collection::btree_set("[a-zA-Z][a-zA-Z0-9_\\-]{0,6}", 0..=4),
        prop::collection::vec((any::<bool>(), 0u8..=127), 4),
    )
        .prop_map(|(names, kinds)| {
            names
                .into_iter()
                .zip(kinds)
                .map(|(name, (is_drums, program))| Instrument {
                    name,
                    program: if is_drums { 0 } else { program },
                    is_drums,
                })
                .collect()
        })
}

fn build_document(
    header: Header,
    instruments: Vec<Instrument>,
    ids: Vec<usize>,
    pat_seeds: Vec<PatSeed>,
    item_seeds: Vec<ItemSeed>,
) -> Document {
    let cpb = header.cells_per_bar();
    let mut patterns: Vec<PatternDef> = Vec::new();
    if !instruments.is_empty() {
        for (id, (track_pick, vel_pick, kin_pick, var_pick, body)) in ids.iter().zip(&pat_seeds) {
            let track = *track_pick as usize % instruments.len();
            let is_drums = instruments[track].is_drums;
            let mut body = build_body(body, is_drums, cpb);
            let mut kin = None;
            let earlier: Vec<usize> =
                patterns.iter().filter(|p| p.track == track).map(|p| p.id).collect();
            if is_drums {
                if let (Some(vp), PatternBody::Drums(db), false) =
                    (var_pick, &mut body, earlier.is_empty())
                {
                    db.variant_base = Some(earlier[*vp as usize % earlier.len()]);
                }
            } else if let (Some(kp), false) = (kin_pick, earlier.is_empty()) {
                kin = Some(earlier[*kp as usize % earlier.len()]);
            }
            patterns.push(PatternDef {
                id: *id,
                track,
                base_vel: BUCKETS[*vel_pick as usize % BUCKETS.len()],
                kin,
                body,
            });
        }
    }
    let n_bars_of = |id: usize| patterns.iter().find(|p| p.id == id).unwrap().body.n_bars();
    let mut timeline = Vec::new();
    for item in item_seeds {
        match item {
            ItemSeed::Row { label, picks, reps } => {
                let mut stack: Vec<usize> = Vec::new();
                for p in picks {
                    if !patterns.is_empty() {
                        let id = patterns[p as usize % patterns.len()].id;
                        if !stack.contains(&id) {
                            stack.push(id);
                        }
                    }
                }
                // Row unit agreement: multi-bar members must all match
                // the longest; 1-bar members always fit.
                let unit = stack.iter().map(|id| n_bars_of(*id)).max().unwrap_or(1);
                stack.retain(|id| {
                    let nb = n_bars_of(*id);
                    nb == 1 || nb == unit
                });
                timeline.push(TimelineItem::Row(Row { label, stack, reps: 1 + (reps % 3) as u32 }));
            }
            ItemSeed::Direct { bar, track, vel, variant, body } => {
                if instruments.is_empty() {
                    continue;
                }
                let track = track as usize % instruments.len();
                let is_drums = instruments[track].is_drums;
                let mut body = build_body(&body, is_drums, cpb);
                if let (Some(vp), PatternBody::Drums(db)) = (variant, &mut body) {
                    let cands: Vec<usize> = patterns
                        .iter()
                        .filter(|p| p.track == track && matches!(p.body, PatternBody::Drums(_)))
                        .map(|p| p.id)
                        .collect();
                    if !cands.is_empty() {
                        db.variant_base = Some(cands[vp as usize % cands.len()]);
                    }
                }
                timeline.push(TimelineItem::Direct(DirectItem {
                    bar: 1 + (bar % 8) as u32,
                    track,
                    base_vel: BUCKETS[vel as usize % BUCKETS.len()],
                    body,
                }));
            }
        }
    }
    Document { header, instruments, patterns, timeline }
}

fn arb_document() -> impl Strategy<Value = Document> {
    (arb_header(), arb_instruments()).prop_flat_map(|(header, instruments)| {
        (
            Just(header),
            Just(instruments),
            prop::collection::btree_set(1usize..=99, 4)
                .prop_map(|s| s.into_iter().collect::<Vec<_>>())
                .prop_shuffle(),
            prop::collection::vec(
                (
                    any::<u8>(),
                    any::<u8>(),
                    prop::option::of(any::<u8>()),
                    prop::option::of(any::<u8>()),
                    body_seed(),
                ),
                0..=4,
            ),
            prop::collection::vec(item_seed(), 0..=5),
        )
            .prop_map(|(header, instruments, ids, pat_seeds, item_seeds)| {
                build_document(header, instruments, ids, pat_seeds, item_seeds)
            })
    })
}

proptest! {
    #[test]
    fn document_canonicality(d in arb_document()) {
        // The generator's claim and validate()'s boundary must agree.
        if let Err(e) = d.validate() {
            return Err(TestCaseError::fail(format!("generated Document failed validate: {e}\n{d:#?}")));
        }
        let text = emit::emit_document(&d);
        // 1. Emitted text always reparses.
        let d2 = parse::parse_document(&text).map_err(|e| {
            TestCaseError::fail(format!("emission failed to reparse: {e}\n---\n{text}"))
        })?;
        prop_assert!(d2.validate().is_ok(), "reparsed Document must validate:\n{}", text);
        // 2. Emission is a fixpoint.
        prop_assert_eq!(&emit::emit_document(&d2), &text, "fixpoint");
        // 3. Resolution is preserved: same music on both routes.
        let q1 = d.resolve().map_err(|e| TestCaseError::fail(format!("resolve d: {e}")))?;
        let q2 = d2.resolve().map_err(|e| TestCaseError::fail(format!("resolve d2: {e}")))?;
        prop_assert_eq!(q1.n_bars, q2.n_bars, "{}", text);
        prop_assert_eq!(q1.tracks.len(), q2.tracks.len());
        for (a, b) in q1.tracks.iter().zip(&q2.tracks) {
            prop_assert_eq!(&a.name, &b.name);
            prop_assert_eq!(a.program, b.program);
            prop_assert_eq!(a.is_drums, b.is_drums);
            prop_assert_eq!(&a.notes, &b.notes, "track {} drifted:\n{}", &a.name, &text);
        }
    }
}
