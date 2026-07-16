//! Per-bar meter overrides: `P5 drums 3/4`, `b12 lead 3/4 | ... |`.
//! The header meter is the default; patterns and directs may claim
//! their bars in another meter, and one bar-stack agrees on one meter.

use leadsheet_core::grid::TICKS_PER_BEAT;
use leadsheet_core::{emit, parse, render};

const HEAD: &str = "song: t  tempo: 120.00  meter: 4/4  grid: 1/16\n\
                    instruments: bass:33 drums:kit\n\n";

#[test]
fn a_three_four_bar_inside_a_four_four_song() {
    let text = format!(
        "{HEAD}P1 bass | C4 C4 C4 C4 |\nP2 bass 3/4 | E4 E4 E4 |\n\n\
         arrangement:\n  [P1]\n  [P2]\n  [P1]\n"
    );
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.n_bars, 3);
    assert_eq!(q.meter, (4, 4));
    assert_eq!(q.bar_meters, vec![(4, 4), (3, 4), (4, 4)]);
    // Bar 2 is three beats long: bar 3 opens at 4+3 beats. Bars hold
    // 4, 3, 4 quarter notes respectively.
    let onsets: Vec<i64> = q.tracks[0].notes.iter().map(|n| n.onset.ticks()).collect();
    assert_eq!(onsets[0], 0);
    assert_eq!(onsets[4], 4 * TICKS_PER_BEAT);
    assert_eq!(onsets[7], 7 * TICKS_PER_BEAT);
    assert_eq!(q.total_ticks().ticks(), 11 * TICKS_PER_BEAT);
}

#[test]
fn render_emits_a_time_signature_per_change() {
    let text = format!(
        "{HEAD}P1 bass | C4 C4 C4 C4 |\nP2 bass 3/4 | E4 E4 E4 |\n\n\
         arrangement:\n  [P1]\n  [P2]\n  [P1]\n"
    );
    let bytes = render::render(&parse::parse(&text).unwrap());
    let smf = midly::Smf::parse(&bytes).unwrap();
    let mut sigs = Vec::new();
    let mut tick = 0u32;
    for ev in &smf.tracks[0] {
        tick += ev.delta.as_int();
        if let midly::TrackEventKind::Meta(midly::MetaMessage::TimeSignature(n, d, _, _)) = ev.kind
        {
            sigs.push((n, 1u32 << d, tick));
        }
    }
    assert_eq!(
        sigs,
        vec![(4, 4, 0), (3, 4, 4 * TICKS_PER_BEAT as u32), (4, 4, 7 * TICKS_PER_BEAT as u32)]
    );
}

#[test]
fn drum_lanes_parse_in_their_own_meter() {
    let text = format!(
        "{HEAD}P1 drums 3/4\n  K |x... x... x...|\n  S |.... x... ....|\n\n\
         arrangement:\n  [P1]\n"
    );
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.bar_meters, vec![(3, 4)]);
    assert_eq!(q.total_ticks().ticks(), 3 * TICKS_PER_BEAT);
}

#[test]
fn chordal_columns_follow_the_meter() {
    let text = format!("{HEAD}P1 bass* 3/4 | C G Am |\n\narrangement:\n  [P1]\n");
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.n_bars, 1);
    // Three one-beat chords, three pitches each.
    assert_eq!(q.tracks[0].notes.len(), 9);
    let bad = format!("{HEAD}P1 bass* 3/4 | C G Am F |\n\narrangement:\n  [P1]\n");
    assert!(parse::parse(&bad).is_err(), "four columns in a 3/4 bar");
}

#[test]
fn emission_is_canonical_and_redundant_overrides_normalize() {
    let text = format!(
        "{HEAD}P1 bass 4/4 | C4 C4 C4 C4 |\nP2 bass 3/4 | E4 E4 E4 |\n\n\
         arrangement:\n  [P1]\n  [P2]\n"
    );
    let d = parse::parse_document(&text).unwrap();
    let once = emit::emit_document(&d);
    assert!(!once.contains("4/4 |"), "redundant header-meter override survives:\n{once}");
    assert!(once.contains("3/4 | E4"), "{once}");
    let twice = emit::emit_document(&parse::parse_document(&once).unwrap());
    assert_eq!(once, twice);
}

#[test]
fn stacks_must_agree_on_meter() {
    let text = format!(
        "{HEAD}P1 bass | C4 C4 C4 C4 |\nP2 drums 3/4\n  K |x... x... x...|\n\n\
         arrangement:\n  [P1+P2]\n"
    );
    let e = parse::parse(&text).unwrap_err();
    assert!(format!("{e}").contains("mixes meters"), "{e}");
}

#[test]
fn conflicting_claims_on_one_bar_error() {
    let text = format!(
        "{HEAD}P1 bass | C4 C4 C4 C4 |\n\narrangement:\n  [P1]\n\n\
         b1 bass 3/4 | E4 E4 E4 |\n"
    );
    let e = parse::parse(&text).unwrap_err();
    assert!(format!("{e}").contains("conflicts"), "{e}");
}

#[test]
fn directs_fill_unclaimed_bars_with_the_header_meter() {
    let text = format!("{HEAD}b3 bass 3/4 | E4 E4 E4 |\n");
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.bar_meters, vec![(4, 4), (4, 4), (3, 4)]);
    // The 3/4 bar opens after two 4/4 bars.
    assert_eq!(q.tracks[0].notes[0].onset.ticks(), 8 * TICKS_PER_BEAT);
}

#[test]
fn a_direct_may_override_a_silent_row_bar() {
    let text = format!(
        "{HEAD}P1 bass | C4 C4 C4 C4 |\n\narrangement:\n  [z]\n  [P1]\n\n\
         b1 bass 3/4 | E4 E4 E4 |\n"
    );
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.bar_meters, vec![(3, 4), (4, 4)]);
    // P1's four C4s start after the silent (now 3/4) first bar.
    let cs: Vec<i64> =
        q.tracks[0].notes.iter().filter(|n| n.pitch == 60).map(|n| n.onset.ticks()).collect();
    assert_eq!(cs.len(), 4);
    assert_eq!(cs[0], 3 * TICKS_PER_BEAT);
}

#[test]
fn multi_bar_patterns_carry_their_meter_across_bars() {
    let text = format!("{HEAD}P1 bass 3/4 | C4 C4 C4 | E4 E4 E4 |\n\narrangement:\n  [P1]\n");
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.bar_meters, vec![(3, 4), (3, 4)]);
    assert_eq!(q.total_ticks().ticks(), 6 * TICKS_PER_BEAT);
}

#[test]
fn six_eight_overrides_work_too() {
    let text = format!("{HEAD}P1 drums 6/8\n  K |x..... x.....|\n\narrangement:\n  [P1]\n");
    let q = parse::parse(&text).unwrap();
    assert_eq!(q.bar_meters, vec![(6, 8)]);
    assert_eq!(q.total_ticks().ticks(), 3 * TICKS_PER_BEAT);
}

#[test]
fn compress_output_stays_uniform_and_suffix_free() {
    // The quantizer never invents meter changes: uniform songs emit no
    // override tokens and keep an empty map.
    let text = format!("{HEAD}P1 bass | C4 C4 C4 C4 |\n\narrangement:\n  [P1] x2\n");
    let q = parse::parse(&text).unwrap();
    assert!(q.bar_meters.is_empty());
    let spelled = emit::emit(&q);
    assert!(!spelled.contains("bass 4/4"), "{spelled}");
    assert!(!spelled.contains("bass 3/4"), "{spelled}");
}

#[test]
fn diff_reports_meter_changes() {
    let a = format!("{HEAD}P1 bass | C4 C4 C4 C4 |\n\narrangement:\n  [P1]\n");
    let b = format!("{HEAD}P1 bass 3/4 | C4 C4 C4 |\n\narrangement:\n  [P1]\n");
    let da = parse::parse_document(&a).unwrap();
    let db = parse::parse_document(&b).unwrap();
    let report = leadsheet_core::diff::diff(&da, &db);
    assert!(report.contains("P1: meter - -> 3/4"), "{report}");
}

#[test]
fn bad_meter_tokens_diagnose() {
    let bad = |line: &str| {
        let text = format!("{HEAD}{line}\n\narrangement:\n  [P1]\n");
        format!("{}", parse::parse(&text).unwrap_err())
    };
    assert!(bad("P1 bass 3/5 | C4 C4 C4 |").contains("denominator"));
    assert!(bad("P1 bass 0/4 | C4 |").contains("numerator"));
    assert!(bad("P1 bass 65/4 | C4 |").contains("numerator"));
}
