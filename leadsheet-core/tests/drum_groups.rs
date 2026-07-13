//! Lane tuplet groups — `(n:span strokes)`: n strokes over span cells,
//! placed by the DESIGN-960 boundary rule. The drum sibling of melodic
//! `(3 C D E)4` groups.

use leadsheet_core::grid::{MusicalTime, TICKS_PER_SIXTEENTH};
use leadsheet_core::{emit, parse};

fn song(lanes: &str) -> String {
    format!(
        "# song: t  tempo: 120.00  meter: 4/4  grid: 1/16\n\
         # instruments: drums:kit\n\n\
         P1 drums\n{lanes}\n\n\
         arrangement:\n  [P1]\n"
    )
}

#[test]
fn triplet_group_compiles_to_one_masked_note() {
    let q = parse::parse(&song("  S |(3:4xxx) x... .... ....|")).unwrap();
    let notes = &q.tracks[0].notes;
    assert_eq!(notes.len(), 2);
    let g = &notes[0];
    assert_eq!(
        (g.onset.ticks(), g.dur.ticks(), g.strokes, g.stroke_mask),
        (0, 4 * TICKS_PER_SIXTEENTH, 3, 0b111)
    );
    // The plain hit lands right after the group's span.
    assert_eq!(notes[1].onset, MusicalTime::from_sixteenths(4));
    assert_eq!((notes[1].strokes, notes[1].stroke_mask), (1, 1));
}

#[test]
fn silent_slots_and_marks_split_by_velocity_class() {
    // x X . o → hit bit0, accent bit1, ghost bit3; slot 2 is silent.
    let q = parse::parse(&song("  S |(4:4xX.o) .... .... ....|")).unwrap();
    let notes = &q.tracks[0].notes;
    assert_eq!(notes.len(), 3);
    let by_mask: std::collections::BTreeMap<u32, u8> =
        notes.iter().map(|n| (n.stroke_mask, n.vel)).collect();
    assert_eq!(by_mask.len(), 3);
    assert_eq!(by_mask[&0b0001], 96, "plain hit at base");
    assert_eq!(by_mask[&0b0010], 112, "accented member");
    assert_eq!(by_mask[&0b1000], 72, "ghosted member");
    assert!(notes.iter().all(|n| n.strokes == 4 && n.dur == MusicalTime::from_sixteenths(4)));
}

#[test]
fn groups_survive_fmt_and_emission_is_a_fixpoint() {
    let text = song("  S |(3:4xxx) x.x. (5:4x.X.o) ....|");
    let d = parse::parse_document(&text).unwrap();
    let once = emit::emit_document(&d);
    assert!(once.contains("(3:4xxx)"), "{once}");
    assert!(once.contains("(5:4x.X.o)"), "{once}");
    let twice = emit::emit_document(&parse::parse_document(&once).unwrap());
    assert_eq!(once, twice);
}

#[test]
fn single_cell_full_group_canonicalizes_to_the_digit() {
    // (3:1xxx) is the digit `3` — one spelling per sound.
    let a = parse::parse(&song("  S |(3:1xxx)... .... .... ....|")).unwrap();
    let b = parse::parse(&song("  S |3... .... .... ....|")).unwrap();
    assert_eq!(a.tracks[0].notes, b.tracks[0].notes);
    let spelled = emit::emit(&a);
    assert!(!spelled.contains("(3:1"), "{spelled}");
    assert!(spelled.contains("|3... "), "{spelled}");
}

#[test]
fn mixed_marks_in_one_cell_stay_a_group() {
    // Full mask but not uniform-base: (2:1xX) is NOT the digit 2.
    let q = parse::parse(&song("  S |(2:1xX)... .... .... ....|")).unwrap();
    let spelled = emit::emit(&q);
    assert!(spelled.contains("(2:1xX)"), "{spelled}");
}

#[test]
fn beat_spacing_resumes_after_a_spanning_group() {
    let text = song("  S |(6:8x.x.x.) x... ....|");
    let q = parse::parse(&text).unwrap();
    let spelled = emit::emit(&q);
    assert!(spelled.contains("|(6:8x.x.x.) x... ....|"), "{spelled}");
}

#[test]
fn inexact_division_places_by_the_boundary_rule() {
    // 5 strokes over 4 cells = 960 ticks: boundaries 0 192 384 576 768.
    let q = parse::parse(&song("  S |(5:4xxxxx) .... .... ....|")).unwrap();
    let n = &q.tracks[0].notes[0];
    assert_eq!((n.strokes, n.stroke_mask, n.dur.ticks()), (5, 0b11111, 960));
}

#[test]
fn variant_lanes_inherit_groups() {
    let text = "# song: t  tempo: 120.00  meter: 4/4  grid: 1/16\n\
                # instruments: drums:kit\n\n\
                P1 drums\n  S |(3:4xxx) x... .... ....|\n  K |x... .... x... ....|\n\
                P2 drums ~P1\n  K |x... x... x... x...|\n\n\
                arrangement:\n  [P2]\n";
    let q = parse::parse(text).unwrap();
    let snare: Vec<_> = q.tracks[0].notes.iter().filter(|n| n.pitch == 38).collect();
    assert_eq!(snare[0].strokes, 3, "the group came through the variant");
}

#[test]
fn group_diagnostics_repair() {
    let bad = |lane: &str| {
        let e = parse::parse(&song(lane)).unwrap_err();
        format!("{e}")
    };
    assert!(bad("  S |(1:4x) .... .... ....|").contains("arity"));
    assert!(bad("  S |(3:0xxx) .... .... ....|").contains("span"));
    assert!(bad("  S |(3:4xx) x.x. .... ....|").contains("needs exactly 3"));
    assert!(bad("  S |(3:4...) x.x. .... ....|").contains("sounding"));
    assert!(bad("  S |(3:4xxx .... .... ....|").contains("unclosed"));
    // The old trailing-span spelling gets pointed at the new one.
    assert!(bad("  S |(3xxx)4 .... .... ....|").contains(":span"));
    // Width counts the span.
    assert!(bad("  S |(3:4xxx) .... .... .....|").contains("17 cells"));
}

#[test]
fn subdivision_digits_do_not_nest_in_groups() {
    let e = parse::parse(&song("  S |(3:4x2x) .... .... ....|")).unwrap_err();
    assert!(format!("{e}").contains("strokes are . o x X"), "{e}");
}
