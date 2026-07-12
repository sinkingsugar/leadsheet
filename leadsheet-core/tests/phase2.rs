//! Phase 2 acceptance: the Document layer. Author structure survives the
//! canonical loop, mixed direct/arrangement semantics are pinned (E6),
//! validate() guards host-built values, and the semantic diff reports at
//! the right granularity.

use leadsheet_core::doc::{Document, PatternBody, TimelineItem};
use leadsheet_core::grid::MusicalTime;
use leadsheet_core::{diff, emit, parse};

const AUTHOR: &str = "\
# song: author  tempo: 100.00  meter: 4/4  key: Am  grid: 1/16
# instruments: piano:0 lead:81 drums:kit

P7 piano* | Am . . . | F . G . |
P9 lead   | e4 c4 d4 B4 |
P10 drums
  K |x... .... x... ....|
  S |.... x... .... x...|
P11 drums ~P10
  h |x.x. x.x. x.x. x.x.|
b1 lead | z8 (3 a g e)4 e4 |

arrangement:
  verse: [P7+P9+P10] x2
  chorus: [P7+P9+P11] x2
";

fn doc(text: &str) -> Document {
    parse::parse_document(text).unwrap()
}

#[test]
fn author_structure_survives_the_canonical_loop() {
    let d1 = doc(AUTHOR);
    let text1 = emit::emit_document(&d1);
    let d2 = doc(&text1);
    assert_eq!(d1, d2, "Document survives emission byte-exactly:\n{text1}");
    assert_eq!(emit::emit_document(&d2), text1, "emission is a fixpoint");
    // The structure really is the author's, not the compressor's.
    assert_eq!(d1.patterns.iter().map(|p| p.id).collect::<Vec<_>>(), [7, 9, 10, 11]);
    assert_eq!(d1.pattern(7).unwrap().body.n_bars(), 2, "multi-bar pattern survives");
    assert!(matches!(
        &d1.pattern(11).unwrap().body,
        PatternBody::Drums(db) if db.variant_base == Some(10) && db.lanes.len() == 1
    ));
    let labels: Vec<_> = d1.rows().map(|r| r.label.clone().unwrap()).collect();
    assert_eq!(labels, ["verse", "chorus"]);
    // And both routes to QSong agree.
    let q1 = parse::parse(AUTHOR).unwrap();
    let q2 = d1.resolve().unwrap();
    assert_eq!(q1.n_bars, q2.n_bars);
    for (a, b) in q1.tracks.iter().zip(&q2.tracks) {
        assert_eq!(a.notes, b.notes);
    }
}

/// E6: direct bars overlay the arrangement timeline, and *source order*
/// carries tie semantics across items.
#[test]
fn mixed_direct_and_arrangement_semantics_are_pinned() {
    // Row leaves a tie open at the end of bar 1; the direct bar written
    // AFTER it continues the note: one 32-cell note.
    let joined = "\
# song: e6  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16- |
arrangement:
  [P1]
b2 p | C16 |
";
    let q = parse::parse(joined).unwrap();
    assert_eq!(q.tracks[0].notes.len(), 1, "tie joins across row -> direct");
    assert_eq!(q.tracks[0].notes[0].dur, MusicalTime::from_sixteenths(32));
    assert_eq!(q.n_bars, 2);

    // The same content with the direct bar written FIRST does not join —
    // the continuation had not been placed yet when b2 resolved.
    let split = "\
# song: e6  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16- |
b2 p | C16 |
arrangement:
  [P1]
";
    let q = parse::parse(split).unwrap();
    assert_eq!(q.tracks[0].notes.len(), 2, "source order is semantic");

    // Overlay: a direct bar stacks with (not replaces) row content.
    let overlay = "\
# song: e6  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16 |
arrangement:
  [P1] x2
b2 p | e4 z12 |
";
    let q = parse::parse(overlay).unwrap();
    assert_eq!(q.tracks[0].notes.len(), 3, "direct content overlays bar 2");
    assert_eq!(q.n_bars, 2);
}

#[test]
fn validate_rejects_host_built_mistakes() {
    let good = doc(AUTHOR);
    good.validate().expect("parsed documents validate");
    good.resolve().unwrap().validate().expect("resolved songs validate");

    // Duplicate id.
    let mut d = good.clone();
    d.patterns[1].id = 7;
    assert!(d.validate().is_err());

    // Wrong lane length.
    let mut d = good.clone();
    if let PatternBody::Drums(db) = &mut d.patterns[2].body {
        db.lanes[0].1.pop();
    }
    assert!(d.validate().is_err());

    // Unrepresentable tempo (u24 µs/quarter).
    let mut d = good.clone();
    d.header.bpm = 0.0001;
    assert!(d.validate().is_err());

    // A song name that would break reparse.
    let mut d = good.clone();
    d.header.name = "x tempo: 4".into();
    assert!(d.validate().is_err());

    // Row referencing a missing pattern (first row sits after the
    // direct bar in the timeline).
    let mut d = good.clone();
    let row = d
        .timeline
        .iter_mut()
        .find_map(|i| match i {
            TimelineItem::Row(r) => Some(r),
            _ => None,
        })
        .unwrap();
    row.stack.push(99);
    assert!(d.validate().is_err());

    // Off-grid drum onset on a QSong.
    let mut q = good.resolve().unwrap();
    let drums = q.tracks.iter_mut().find(|t| t.is_drums).unwrap();
    drums.notes[0].onset = MusicalTime(7);
    assert!(q.validate().is_err());
}

/// A1: the diff contract is "empty = semantically identical", and
/// timeline *order* is semantic (the E6 joined/split pair compiles to
/// different QSongs). Reordering rows and directs must never diff empty.
#[test]
fn semantic_diff_sees_timeline_order() {
    let joined = "\
# song: e6  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16- |
arrangement:
  [P1]
b2 p | C16 |
";
    let split = "\
# song: e6  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16- |
b2 p | C16 |
arrangement:
  [P1]
";
    let (a, b) = (doc(joined), doc(split));
    let report = diff::diff(&a, &b);
    assert!(!report.is_empty(), "row/direct interleaving is tie-semantic and must diff nonempty");
    assert!(report.contains("timeline item 1"), "{report}");
    assert!(diff::diff(&a, &a).is_empty());
    assert!(diff::diff(&b, &b).is_empty());
}

/// D2: `kin` is source-semantic Document structure; retargeting it with
/// an unchanged body must be reported.
#[test]
fn semantic_diff_reports_kin_changes() {
    let base = "\
# song: k  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p     | C4 E4 G4 c4 |
P2 p     | D4 F4 A4 d4 |
P3 p ~P1 | C4 E4 G4 z4 |
arrangement:
  [P3]
";
    let retargeted = base.replace("P3 p ~P1", "P3 p ~P2");
    let report = diff::diff(&doc(base), &doc(&retargeted));
    assert!(report.contains("P3: kin ~P1 -> ~P2"), "{report}");
    let dropped = base.replace(" ~P1", "");
    let report = diff::diff(&doc(base), &doc(&dropped));
    assert!(report.contains("P3: kin ~P1 -> -"), "{report}");
}

#[test]
fn semantic_diff_reports_at_the_right_granularity() {
    let edited = AUTHOR
        .replace("| e4 c4 d4 B4 |", "| e4 c4 d4 c4 |")
        .replace("  S |.... x... .... x...|", "  S |.... x... .... x..o|")
        .replace("tempo: 100.00", "tempo: 96.00")
        .replace("chorus: [P7+P9+P11] x2", "chorus: [P7+P9+P11] x4");
    let (a, b) = (doc(AUTHOR), doc(&edited));
    let report = diff::diff(&a, &b);
    assert!(report.contains("tempo: 100.00 -> 96.00"), "{report}");
    assert!(report.contains("P9 bar 1: | e4 c4 d4 B4 | -> | e4 c4 d4 c4 |"), "{report}");
    assert!(report.contains("P10 lane S:"), "{report}");
    assert!(report.contains("x..o"), "{report}");
    assert!(report.contains("row 2:"), "{report}");
    assert!(report.contains("x4"), "{report}");
    assert!(
        !report.contains("P7 bar") && !report.contains("P7:"),
        "untouched patterns stay silent:\n{report}"
    );
    assert!(diff::diff(&a, &a).is_empty(), "identical documents diff empty");
}
