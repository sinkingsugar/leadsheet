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
//!
//! Triage-3 C1 adds the complement, `hostile_mutation_is_rejected`:
//! `arb_document` is valid by construction, so it can never reach the
//! host-boundary holes (its keys are 0..12, its ChordSyms come from
//! `parse_symbol`). The second property takes a valid Document, mutates
//! ONE public field into a hostile value — bad key pc, noncanonical
//! chord discriminant, off-bucket velocity, dangling reference,
//! syntax-breaking text, bad lane shape, zero meter denominator — and
//! asserts `validate()` (and therefore `resolve()`) returns `Err`
//! without panicking. The host boundary as a systematic target instead
//! of a hand-written list.
//!
//! Triage-4 A4 adds the QSong sibling, `hostile_qsong_mutation_is_
//! rejected`: valid QSongs (from `resolve()` of generated Documents)
//! with one public field mutated — program, velocity, timing (including
//! overflow-shaped onsets/durations that the validator's own arithmetic
//! must survive), strokes, names, header — must fail `QSong::validate()`
//! without panicking.

use leadsheet_core::doc::{
    AutoLane, Bind, BindScope, ChordCol, DirectItem, Document, DrumsBody, Header, Instrument,
    Keyframe, LaneItem, MelodicBar, PatternBody, PatternDef, Row, TimelineItem,
};
use leadsheet_core::drums::LANE_D4;
use leadsheet_core::grid::{Ease, ExternKind, MusicalTime, QSong, Swing, Target};
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
                    // Mostly plain cells with occasional tuplet groups —
                    // the seed byte picks the item kind so canonicality
                    // covers `(n:span strokes)` items too.
                    let mut full: Vec<LaneItem> = Vec::new();
                    let mut width = 0u32;
                    let mut i = 0usize;
                    while width < cpb {
                        let b = cells[i % cells.len()];
                        i += 1;
                        if b % 11 == 7 && cpb - width >= 2 {
                            let span = (2 + (b / 16) % 3).min((cpb - width) as u8 % 250);
                            let n = 2 + b % 5;
                            let mut members: Vec<u8> =
                                (0..n).map(|k| cells[(i + k as usize) % cells.len()] % 4).collect();
                            i += n as usize;
                            if members.iter().all(|m| *m == 0) {
                                members[0] = 2; // at least one sounding stroke
                            }
                            width += span as u32;
                            full.push(LaneItem::Group { n, members, span });
                        } else {
                            width += 1;
                            full.push(LaneItem::Cell(b % (LANE_D4 + 1)));
                        }
                    }
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

// ---------------------------------------------------------------------------
// Automation: a deterministic, valid-by-construction bind table plus
// per-body lanes, derived from the body's own coordinates (no extra
// strategy inputs, so shrinking stays stable). This makes
// `document_canonicality` guard the bind/lane/keyframe emit↔parse↔resolve
// paths — easings, fractional positions, scoped resolution, every target.

/// A cheap deterministic byte stream (an LCG) seeded per body.
struct Bytes(u64);
impl Bytes {
    fn new(seed: u64) -> Self {
        Bytes(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u8 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 56) as u8
    }
}

/// 0–2 comment lines from the valid domain (trim-stable, single-line):
/// empty, plain words, interior runs of spaces, and sigil-looking texts
/// (`//`, `[x]`, `P1 |`) that must survive as inert annotation.
fn gen_comments(seed: u64) -> Vec<String> {
    const TEXTS: &[&str] = &[
        "",
        "note",
        "a  b",
        "//",
        "P1 | q c4 |",
        "chorus: [P1] x4",
        "bind a = cc74",
        "@a { 0:1 }",
    ];
    let mut b = Bytes::new(seed ^ 0xC0_FFEE);
    (0..b.next() % 3).map(|_| TEXTS[b.next() as usize % TEXTS.len()].to_string()).collect()
}

/// A varied target for an instrument-scoped bind (exercises every kind).
fn auto_target(i: u8) -> Target {
    match i % 7 {
        0 => Target::ChannelPressure,
        1 => Target::Nrpn(1000 + i as u16),
        2 => Target::Extern { kind: ExternKind::Osc, path: format!("/p{i}") },
        3 => Target::Cc(20 + i),
        4 => Target::Rpn(i as u16),
        5 => Target::Program,
        _ => Target::PolyPressure(i.min(127)),
    }
}

/// The bind table every generated Document carries: song-level `a`/`b`,
/// plus per-instrument `a` (overrides the song `a`) and `c` for the first
/// few instruments. So `a`/`b` resolve on any track; `c` only where scoped.
fn gen_binds(instruments: &[Instrument]) -> Vec<Bind> {
    let mut binds = vec![
        // A domained song bind, so the value-remap path is in the fixpoint.
        Bind {
            scope: BindScope::Song,
            name: "a".into(),
            target: Target::Cc(74),
            domain: Some((0.0, 1.0)),
            comments: Vec::new(),
        },
        Bind {
            scope: BindScope::Song,
            name: "b".into(),
            target: Target::PitchBend,
            domain: None,
            // A commented bind: emission sorts binds, so the comment
            // must travel with its bind through the fixpoint.
            comments: vec!["bend rides the solo".into()],
        },
    ];
    for i in 0..instruments.len().min(3) {
        binds.push(Bind {
            scope: BindScope::Instrument(i),
            name: "a".into(),
            target: Target::Cc(10 + i as u8),
            domain: None,
            comments: Vec::new(),
        });
        binds.push(Bind {
            scope: BindScope::Instrument(i),
            name: "c".into(),
            target: auto_target(i as u8),
            domain: if i % 2 == 0 { Some((-2.0, 2.0)) } else { None },
            comments: gen_comments(i as u64 ^ 0xB1),
        });
    }
    binds
}

fn gen_ease(b: u8) -> Ease {
    match b % 6 {
        0 => Ease::Lin,
        1 => Ease::Hold,
        2 => Ease::Smooth,
        3 => Ease::Exp(2.0),
        4 => Ease::Exp(-3.5),
        _ => Ease::Bez(0.42, 0.0, 0.58, 1.0),
    }
}

/// 0–2 valid lanes over a body of `span_cells` cells: bound names only
/// (`c` iff `allow_c`), strictly-increasing sub-cell positions, canonical
/// quarter-step values, varied easings (last normalized to `Lin`).
fn gen_autos(seed: u64, span_cells: u32, allow_c: bool) -> Vec<AutoLane> {
    let mut b = Bytes::new(seed);
    let names: &[&str] = if allow_c { &["a", "b", "c"] } else { &["a", "b"] };
    let span_ticks = span_cells as i64 * 240;
    let n_lanes = (b.next() % 3) as usize;
    let mut lanes: Vec<AutoLane> = Vec::new();
    for _ in 0..n_lanes {
        let name = names[b.next() as usize % names.len()];
        if lanes.iter().any(|l| l.name == name) {
            continue; // one lane per name per body
        }
        let want = 1 + (b.next() % 4) as usize;
        let mut ticks: Vec<i64> = (0..want)
            .map(|_| {
                // Cell boundary + a sub-cell offset → mostly fractional.
                (b.next() as i64 * 240 + b.next() as i64) % (span_ticks + 1)
            })
            .collect();
        ticks.sort_unstable();
        ticks.dedup();
        let keys: Vec<Keyframe> = ticks
            .iter()
            .enumerate()
            .map(|(i, &t)| Keyframe {
                at: MusicalTime(t),
                value: (b.next() as i32 - 128) as f64 / 4.0, // multiples of 0.25 (canonical)
                ease: if i + 1 == ticks.len() { Ease::Lin } else { gen_ease(b.next()) },
            })
            .collect();
        lanes.push(AutoLane {
            name: name.to_string(),
            keys,
            comments: gen_comments(seed ^ (lanes.len() as u64) << 3),
        });
    }
    lanes
}

fn build_document(
    header: Header,
    instruments: Vec<Instrument>,
    alt_meter: Option<(u32, u32)>,
    ids: Vec<usize>,
    pat_seeds: Vec<PatSeed>,
    item_seeds: Vec<ItemSeed>,
) -> Document {
    // One doc-wide override keeps every bar-stack agreed by construction;
    // mixed-meter interactions are unit-tested.
    let eff = alt_meter.unwrap_or(header.meter);
    let cpb = eff.0 * 4 * 4 / eff.1;
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
            let autos = gen_autos(
                (*id as u64).wrapping_mul(2654435761) ^ ((track as u64) << 20),
                cpb * body.n_bars(),
                track < instruments.len().min(3),
            );
            patterns.push(PatternDef {
                id: *id,
                track,
                base_vel: BUCKETS[*vel_pick as usize % BUCKETS.len()],
                meter: alt_meter,
                kin,
                body,
                autos,
                comments: gen_comments(*id as u64 ^ 0x9A),
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
                timeline.push(TimelineItem::Row(Row {
                    label,
                    stack,
                    reps: 1 + (reps % 3) as u32,
                    comments: gen_comments(reps as u64 ^ 0x51),
                }));
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
                let autos = gen_autos(
                    ((bar as u64) << 24) ^ ((track as u64) << 8) ^ vel as u64,
                    cpb * body.n_bars(),
                    track < instruments.len().min(3),
                );
                timeline.push(TimelineItem::Direct(DirectItem {
                    bar: 1 + (bar % 8) as u32,
                    track,
                    base_vel: BUCKETS[vel as usize % BUCKETS.len()],
                    meter: alt_meter,
                    body,
                    autos,
                    comments: gen_comments(bar as u64 ^ 0xD1),
                }));
            }
        }
    }
    let binds = gen_binds(&instruments);
    let n = instruments.len() as u64;
    Document {
        header,
        instruments,
        binds,
        patterns,
        timeline,
        header_comments: gen_comments(n ^ 0x4A),
        instruments_comments: gen_comments(n ^ 0x1B),
        trailing_comments: gen_comments(n ^ 0x7C),
    }
}

fn arb_document() -> impl Strategy<Value = Document> {
    (arb_header(), arb_instruments()).prop_flat_map(|(header, instruments)| {
        (
            Just(header),
            Just(instruments),
            prop::option::of(prop::sample::select(vec![(3u32, 4u32), (6, 8), (5, 4)])),
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
            .prop_map(|(header, instruments, alt, ids, pat_seeds, item_seeds)| {
                build_document(header, instruments, alt, ids, pat_seeds, item_seeds)
            })
    })
}

// ---------------------------------------------------------------------------
// C1 — hostile single-field mutations. Each MUST make the Document
// invalid; the property asserts validate() says so without panicking.

fn bodies_mut(d: &mut Document) -> impl Iterator<Item = &mut PatternBody> {
    d.patterns.iter_mut().map(|p| &mut p.body).chain(d.timeline.iter_mut().filter_map(
        |i| match i {
            TimelineItem::Direct(x) => Some(&mut x.body),
            _ => None,
        },
    ))
}

fn first_chord_sym(d: &mut Document) -> Option<&mut chord::ChordSym> {
    bodies_mut(d).find_map(|b| match b {
        PatternBody::Chordal(bars) => bars.iter_mut().flatten().find_map(|c| match c {
            ChordCol::Sym(s) => Some(s),
            _ => None,
        }),
        _ => None,
    })
}

fn first_row(d: &mut Document) -> Option<&mut Row> {
    d.timeline.iter_mut().find_map(|i| match i {
        TimelineItem::Row(r) => Some(r),
        _ => None,
    })
}

fn first_lane(d: &mut Document) -> Option<&mut Vec<LaneItem>> {
    bodies_mut(d).find_map(|b| match b {
        PatternBody::Drums(db) => db.lanes.first_mut().map(|(_, cells)| cells),
        _ => None,
    })
}

fn first_lane_group(d: &mut Document) -> Option<&mut LaneItem> {
    bodies_mut(d).find_map(|b| match b {
        PatternBody::Drums(db) => db
            .lanes
            .iter_mut()
            .flat_map(|(_, items)| items.iter_mut())
            .find(|i| matches!(i, LaneItem::Group { .. })),
        _ => None,
    })
}

fn auto_lanes_mut(d: &mut Document) -> impl Iterator<Item = &mut AutoLane> {
    let pat = d.patterns.iter_mut().flat_map(|p| p.autos.iter_mut());
    let dir = d
        .timeline
        .iter_mut()
        .filter_map(|i| match i {
            TimelineItem::Direct(x) => Some(x.autos.iter_mut()),
            _ => None,
        })
        .flatten();
    pat.chain(dir)
}

fn first_note_pitch(d: &mut Document) -> Option<&mut u8> {
    bodies_mut(d).find_map(|b| match b {
        PatternBody::Melodic(bars) => {
            bars.iter_mut().flat_map(|mb| mb.voices.iter_mut().flatten()).find_map(|t| match t {
                Tok::Note { pitch, .. } => Some(pitch),
                _ => None,
            })
        }
        _ => None,
    })
}

fn first_tuplet_span(d: &mut Document) -> Option<&mut MusicalTime> {
    bodies_mut(d).find_map(|b| match b {
        PatternBody::Melodic(bars) => {
            bars.iter_mut().flat_map(|mb| mb.voices.iter_mut().flatten()).find_map(|t| match t {
                Tok::Tuplet { span, .. } => Some(span),
                _ => None,
            })
        }
        _ => None,
    })
}

/// Apply hostile mutation `which`, walking forward past inapplicable
/// ones (mutation 0 always applies). Returns the mutation's name.
fn mutate_hostile(d: &mut Document, which: u8, spice: u8) -> &'static str {
    const N: u32 = 36;
    for k in 0..N {
        let (name, applied) = match (which as u32 + k) % N {
            0 => ("key pc out of range", {
                d.header.key =
                    Some(Key { tonic_pc: 12 + spice % 244, minor: spice.is_multiple_of(2) });
                true
            }),
            1 => ("zero meter denominator", {
                d.header.meter = (4, 0);
                true
            }),
            2 => ("untrimmed song name", {
                d.header.name = format!(" {}", d.header.name);
                true
            }),
            3 => ("non-finite tempo", {
                d.header.bpm = f64::NAN;
                true
            }),
            4 => ("swing percent out of range", {
                d.header.swing = Some(Swing { level: 16, percent: 99 });
                true
            }),
            5 => ("noncanonical chord root", {
                first_chord_sym(d).map(|s| s.root_pc = 13 + spice % 243).is_some()
            }),
            6 => ("noncanonical chord bass", {
                first_chord_sym(d).map(|s| s.bass_pc = 12 + spice % 244).is_some()
            }),
            7 => ("chord quality overshoot", {
                first_chord_sym(d)
                    .map(|s| s.quality = chord::QUALITIES.len() + spice as usize)
                    .is_some()
            }),
            8 => ("off-bucket base velocity", {
                d.patterns.first_mut().map(|p| p.base_vel = 70).is_some()
            }),
            9 => ("pattern track out of range", {
                let n = d.instruments.len();
                d.patterns.first_mut().map(|p| p.track = n + 1).is_some()
            }),
            10 => ("syntax-breaking instrument name", {
                d.instruments.first_mut().map(|i| i.name = "a[b".into()).is_some()
            }),
            11 => ("empty instrument name", {
                d.instruments.first_mut().map(|i| i.name = String::new()).is_some()
            }),
            12 => ("comment-shaped row label", {
                first_row(d).map(|r| r.label = Some("//x".into())).is_some()
            }),
            13 => ("zero row repeats", first_row(d).map(|r| r.reps = 0).is_some()),
            14 => ("direct bar zero", {
                d.timeline
                    .iter_mut()
                    .find_map(|i| match i {
                        TimelineItem::Direct(x) => Some(x),
                        _ => None,
                    })
                    .map(|x| x.bar = 0)
                    .is_some()
            }),
            15 => ("bad lane cell code", {
                first_lane(d)
                    .map(|cells| cells[0] = LaneItem::Cell(LANE_D4 + 1 + spice % 100))
                    .is_some()
            }),
            16 => ("lane length off by one", {
                first_lane(d)
                    .map(|cells| {
                        cells.pop();
                    })
                    .is_some()
            }),
            17 => ("dangling kin reference", {
                d.patterns.first_mut().map(|p| p.kin = Some(1000)).is_some()
            }),
            18 => ("pitch beyond MIDI", first_note_pitch(d).map(|p| *p = 200).is_some()),
            19 => ("duplicate pattern id", {
                if d.patterns.len() >= 2 {
                    d.patterns[1].id = d.patterns[0].id;
                    true
                } else {
                    false
                }
            }),
            20 => ("tuplet span below arity", {
                first_tuplet_span(d).map(|s| *s = MusicalTime(1)).is_some()
            }),
            21 => ("sub-hundredth tempo", {
                d.header.bpm += 0.001;
                true
            }),
            22 => ("melodic program beyond GM", {
                d.instruments
                    .iter_mut()
                    .find(|i| !i.is_drums)
                    .map(|i| i.program = 128 + spice % 128)
                    .is_some()
            }),
            23 => ("source kit program", {
                // The text has no slot for one (`drums:kit`).
                d.instruments
                    .iter_mut()
                    .find(|i| i.is_drums)
                    .map(|i| i.program = 1 + spice % 127)
                    .is_some()
            }),
            24 => ("timeline beyond the renderable tick domain", {
                // A rest-bar row repeated past every cap (bars or ticks,
                // whichever fires first — both must Err).
                d.timeline.push(TimelineItem::Row(Row {
                    label: None,
                    stack: Vec::new(),
                    reps: u32::MAX,
                    comments: Vec::new(),
                }));
                true
            }),
            25 => ("all-silent lane group", {
                first_lane_group(d)
                    .map(|item| {
                        if let LaneItem::Group { members, .. } = item {
                            members.iter_mut().for_each(|m| *m = 0);
                        }
                    })
                    .is_some()
            }),
            26 => ("lane group arity mismatch", {
                first_lane_group(d)
                    .map(|item| {
                        if let LaneItem::Group { members, .. } = item {
                            members.push(2);
                        }
                    })
                    .is_some()
            }),
            27 => ("unbound automation lane", {
                auto_lanes_mut(d).next().map(|l| l.name = "zzz".into()).is_some()
            }),
            28 => ("off-decimal automation value", {
                auto_lanes_mut(d)
                    .find_map(|l| l.keys.first_mut())
                    .map(|k| k.value = 0.123456)
                    .is_some()
            }),
            29 => ("malformed exp easing", {
                auto_lanes_mut(d)
                    .find_map(|l| l.keys.first_mut())
                    .map(|k| k.ease = Ease::Exp(0.0))
                    .is_some()
            }),
            30 => ("automation keyframe past the pattern", {
                auto_lanes_mut(d)
                    .find_map(|l| l.keys.first_mut())
                    .map(|k| k.at = MusicalTime(i64::MAX))
                    .is_some()
            }),
            31 => ("duplicate bind in one scope", {
                d.binds.first().cloned().map(|b| d.binds.push(b)).is_some()
            }),
            32 => ("inverted bind domain", {
                d.binds.first_mut().map(|b| b.domain = Some((1.0, 0.0))).is_some()
            }),
            33 => ("bezier x-control out of [0,1]", {
                auto_lanes_mut(d)
                    .find_map(|l| l.keys.first_mut())
                    .map(|k| k.ease = Ease::Bez(2.0, 0.0, 0.5, 1.0))
                    .is_some()
            }),
            34 => ("multi-line comment", {
                // A `\n` inside comment text would emit a second,
                // uncommented line.
                d.header_comments.push("a\nb".into());
                true
            }),
            35 => ("untrimmed comment", {
                // Surrounding whitespace does not survive the reparse trim.
                d.trailing_comments.push(" x".into());
                true
            }),
            _ => unreachable!(),
        };
        if applied {
            return name;
        }
    }
    unreachable!("mutation 0 always applies")
}

// ---------------------------------------------------------------------------
// Triage-4 A4 — the QSong sibling: the compiled layer's boundary got its
// r4 holes (program, velocity, overflowing extents) found by hand; this
// makes it a systematic target too. Valid QSongs come from resolve() of
// generated Documents — the cheapest valid source.

fn qsong_track(q: &mut QSong, drums: Option<bool>) -> Option<&mut leadsheet_core::grid::QTrack> {
    q.tracks.iter_mut().find(|t| drums.is_none_or(|d| t.is_drums == d))
}

/// First note on a track matching `drums` (None = any track).
fn qsong_note(q: &mut QSong, drums: Option<bool>) -> Option<&mut leadsheet_core::grid::QNote> {
    q.tracks
        .iter_mut()
        .filter(|t| drums.is_none_or(|d| t.is_drums == d))
        .find_map(|t| t.notes.first_mut())
}

fn mutate_hostile_qsong(q: &mut QSong, which: u8, spice: u8) -> &'static str {
    // (meter-map cases appended at the tail)
    const N: u32 = 25;
    for k in 0..N {
        let (name, applied) = match (which as u32 + k) % N {
            0 => ("key pc out of range", {
                q.key = Some(Key { tonic_pc: 12 + spice % 244, minor: spice.is_multiple_of(2) });
                true
            }),
            1 => ("zero meter denominator", {
                q.meter = (4, 0);
                true
            }),
            2 => ("zero meter numerator", {
                q.meter = (0, 4);
                true
            }),
            3 => ("meter numerator beyond 64", {
                q.meter = (65, 4);
                true
            }),
            4 => ("swing percent out of range", {
                q.swing = Some(Swing { level: 8, percent: 99 });
                true
            }),
            5 => ("non-finite tempo", {
                q.bpm = f64::NAN;
                true
            }),
            6 => ("untrimmed song name", {
                q.name = format!(" {}", q.name);
                true
            }),
            7 => ("syntax-breaking track name", {
                qsong_track(q, None).map(|t| t.name = "a b".into()).is_some()
            }),
            8 => ("empty track name", {
                qsong_track(q, None).map(|t| t.name = String::new()).is_some()
            }),
            9 => ("duplicate track names", {
                if q.tracks.len() >= 2 {
                    q.tracks[1].name = q.tracks[0].name.clone();
                    true
                } else {
                    false
                }
            }),
            10 => ("program beyond GM", {
                qsong_track(q, None).map(|t| t.program = 128 + spice % 128).is_some()
            }),
            11 => ("pitch beyond MIDI", qsong_note(q, None).map(|n| n.pitch = 200).is_some()),
            12 => ("note-off velocity", qsong_note(q, None).map(|n| n.vel = 0).is_some()),
            13 => ("velocity beyond MIDI", {
                qsong_note(q, None).map(|n| n.vel = 128 + spice % 128).is_some()
            }),
            14 => ("negative onset", {
                qsong_note(q, None).map(|n| n.onset = MusicalTime(-1)).is_some()
            }),
            15 => ("onset near i64::MAX", {
                // Passes the sign checks; the extent add must stay total.
                qsong_note(q, None).map(|n| n.onset = MusicalTime(i64::MAX)).is_some()
            }),
            16 => ("zero duration", {
                qsong_note(q, None).map(|n| n.dur = MusicalTime::ZERO).is_some()
            }),
            17 => ("melodic duration near i64::MAX", {
                // Melodic only: a drum note's extent is one cell, so a
                // hostile dur alone would not trip the end check.
                qsong_note(q, Some(false)).map(|n| n.dur = MusicalTime(i64::MAX)).is_some()
            }),
            18 => ("melodic strokes", qsong_note(q, Some(false)).map(|n| n.strokes = 2).is_some()),
            19 => ("stroke count out of range", {
                // 5 strokes in a cell became legal with lane groups
                // ((5xxxxx)1); the domain ceiling is 24.
                qsong_note(q, Some(true)).map(|n| n.strokes = 25).is_some()
            }),
            20 => ("drum hit off the 16th grid", {
                qsong_note(q, Some(true)).map(|n| n.onset += MusicalTime(1)).is_some()
            }),
            21 => ("note past n_bars", {
                if q.tracks.iter().any(|t| !t.notes.is_empty()) {
                    q.n_bars = 0;
                    true
                } else {
                    false
                }
            }),
            22 => ("beyond the renderable tick domain", {
                // Host-built n_bars has no bar cap; render's u32/u28
                // tick casts would silently wrap.
                q.n_bars = u32::MAX;
                true
            }),
            23 => ("bar_meters length mismatch", {
                if q.bar_meters.is_empty() && q.n_bars > 0 {
                    // A one-entry map for an n>1-bar song, or a stray
                    // entry for a 1-bar song — either way len != n_bars
                    // ... unless n_bars == 1, where one entry is legal.
                    q.bar_meters = vec![(3, 4); q.n_bars as usize + 1];
                    true
                } else if !q.bar_meters.is_empty() {
                    q.bar_meters.pop();
                    // Popping a len-1 map to empty is a *valid* uniform song,
                    // not a mismatch — only claim success if a map remains.
                    !q.bar_meters.is_empty() && q.n_bars == q.bar_meters.len() as u32 + 1
                } else {
                    false
                }
            }),
            24 => ("invalid bar meter entry", {
                if q.n_bars > 0 {
                    q.bar_meters = vec![(0, 4); q.n_bars as usize];
                    true
                } else {
                    false
                }
            }),
            _ => unreachable!(),
        };
        if applied {
            return name;
        }
    }
    unreachable!("mutation 0 always applies")
}

proptest! {
    #[test]
    fn hostile_qsong_mutation_is_rejected(
        (d, which, spice) in (arb_document(), any::<u8>(), any::<u8>())
    ) {
        let mut q = d.resolve().expect("generated Documents resolve");
        let name = mutate_hostile_qsong(&mut q, which, spice);
        prop_assert!(q.validate().is_err(), "'{}' slipped through QSong::validate", name);
    }
}

proptest! {
    #[test]
    fn hostile_mutation_is_rejected(
        (mut d, which, spice) in (arb_document(), any::<u8>(), any::<u8>())
    ) {
        let name = mutate_hostile(&mut d, which, spice);
        // Err, never a panic — resolve() included (it validates first).
        prop_assert!(d.validate().is_err(), "'{}' slipped through validate:\n{:#?}", name, d);
        prop_assert!(d.resolve().is_err(), "'{}' slipped through resolve", name);
    }
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
        // 3. Resolution is preserved: same music AND same compiled
        //    header on both routes (C2 — a header-mangling bug must not
        //    slip the property just because the notes survived).
        let q1 = d.resolve().map_err(|e| TestCaseError::fail(format!("resolve d: {e}")))?;
        let q2 = d2.resolve().map_err(|e| TestCaseError::fail(format!("resolve d2: {e}")))?;
        prop_assert_eq!(&q1.name, &q2.name);
        prop_assert_eq!(q1.bpm, q2.bpm);
        prop_assert_eq!(q1.meter, q2.meter);
        prop_assert_eq!(&q1.bar_meters, &q2.bar_meters);
        prop_assert_eq!(q1.key, q2.key);
        prop_assert_eq!(q1.swing, q2.swing);
        prop_assert_eq!(q1.n_bars, q2.n_bars, "{}", text);
        prop_assert_eq!(q1.tracks.len(), q2.tracks.len());
        for (a, b) in q1.tracks.iter().zip(&q2.tracks) {
            prop_assert_eq!(&a.name, &b.name);
            prop_assert_eq!(a.program, b.program);
            prop_assert_eq!(a.is_drums, b.is_drums);
            prop_assert_eq!(&a.notes, &b.notes, "track {} drifted:\n{}", &a.name, &text);
            // Automation must survive the hop too: same targets, keyframes,
            // positions (fractional included), values and easings.
            prop_assert_eq!(&a.autos, &b.autos, "track {} automation drifted:\n{}", &a.name, &text);
        }
    }
}
