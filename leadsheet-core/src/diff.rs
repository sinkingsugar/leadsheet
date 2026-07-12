//! Semantic diff over [`Document`]s, not lines: the right granularity for
//! a human or an LLM reviewing an edit — header fields, instruments,
//! patterns (per bar for melodic/chordal, per lane for drums), and the
//! timeline. Plain text out; empty string = identical.
//!
//! The timeline is compared as the ordered sequence it is: source order
//! is tie-semantic (a row↔direct reorder compiles differently — E6), so
//! interleaving changes are real differences. Matching is positional —
//! inserting an item at the top reports the whole tail as changed. That
//! is a known, accepted limit of the lean diff; don't build
//! edit-distance intuitions on it.

use crate::doc::{Document, DrumsBody, PatternBody, PatternDef, Row, TimelineItem};
use crate::emit::{lane_char, spell_chordal_bar, spell_melodic_bar};
use crate::{drums, notation};
use std::fmt::Write;

/// Human/LLM-readable difference report; empty when the documents are
/// semantically identical.
pub fn diff(a: &Document, b: &Document) -> String {
    let mut out = String::new();
    let fa = a.header.key.map(|k| k.use_flats()).unwrap_or(false);
    let fb = b.header.key.map(|k| k.use_flats()).unwrap_or(false);

    // Header.
    if a.header.name != b.header.name {
        let _ = writeln!(out, "song: {:?} -> {:?}", a.header.name, b.header.name);
    }
    if a.header.bpm != b.header.bpm {
        let _ = writeln!(out, "tempo: {:.2} -> {:.2}", a.header.bpm, b.header.bpm);
    }
    if a.header.meter != b.header.meter {
        let _ = writeln!(
            out,
            "meter: {}/{} -> {}/{}",
            a.header.meter.0, a.header.meter.1, b.header.meter.0, b.header.meter.1
        );
    }
    if a.header.key != b.header.key {
        let name = |k: Option<crate::key::Key>| k.map(|k| k.name()).unwrap_or_else(|| "-".into());
        let _ = writeln!(out, "key: {} -> {}", name(a.header.key), name(b.header.key));
    }
    if a.header.swing != b.header.swing {
        let name = |s: Option<crate::grid::Swing>| match s {
            None => "-".to_string(),
            Some(s) if s.level == 16 => format!("16th {}%", s.percent),
            Some(s) => format!("{}%", s.percent),
        };
        let _ = writeln!(out, "swing: {} -> {}", name(a.header.swing), name(b.header.swing));
    }

    // Instruments, by name.
    for ia in &a.instruments {
        match b.instruments.iter().find(|ib| ib.name == ia.name) {
            None => {
                let _ = writeln!(out, "instrument {} removed", ia.name);
            }
            Some(ib) if ia != ib => {
                let _ = writeln!(out, "instrument {}: {} -> {}", ia.name, field(ia), field(ib));
            }
            _ => {}
        }
    }
    for ib in &b.instruments {
        if !a.instruments.iter().any(|ia| ia.name == ib.name) {
            let _ = writeln!(out, "instrument {} added ({})", ib.name, field(ib));
        }
    }

    // Patterns, by id.
    for pa in &a.patterns {
        match b.pattern(pa.id) {
            None => {
                let _ = writeln!(out, "P{} removed", pa.id);
            }
            Some(pb) => diff_pattern(&mut out, pa, pb, fa, fb),
        }
    }
    for pb in &b.patterns {
        if a.pattern(pb.id).is_none() {
            let _ = writeln!(out, "P{} added ({})", pb.id, kind(&pb.body));
        }
    }

    // Timeline: one ordered, positional walk. Same-kind items get the
    // detailed report; a kind flip at a position is itself the finding
    // (the interleaving carries tie semantics).
    let mut row_no = 0usize;
    for i in 0..a.timeline.len().max(b.timeline.len()) {
        match (a.timeline.get(i), b.timeline.get(i)) {
            (Some(TimelineItem::Row(x)), Some(TimelineItem::Row(y))) => {
                row_no += 1;
                if x != y {
                    let _ = writeln!(out, "row {row_no}: {} -> {}", row_text(x), row_text(y));
                }
            }
            (Some(TimelineItem::Direct(x)), Some(TimelineItem::Direct(y))) => {
                if x != y {
                    if x.bar == y.bar {
                        let _ = writeln!(out, "direct b{} changed", x.bar);
                    } else {
                        let _ = writeln!(out, "direct b{} -> b{} changed", x.bar, y.bar);
                    }
                }
            }
            (Some(x), Some(y)) => {
                if matches!(x, TimelineItem::Row(_)) {
                    row_no += 1;
                }
                let _ =
                    writeln!(out, "timeline item {}: {} -> {}", i + 1, item_text(x), item_text(y));
            }
            (Some(x), None) => {
                let _ = writeln!(out, "timeline item {} removed: {}", i + 1, item_text(x));
            }
            (None, Some(y)) => {
                let _ = writeln!(out, "timeline item {} added: {}", i + 1, item_text(y));
            }
            (None, None) => unreachable!(),
        }
    }
    out
}

fn item_text(i: &TimelineItem) -> String {
    match i {
        TimelineItem::Row(r) => row_text(r),
        TimelineItem::Direct(d) => format!("direct b{}", d.bar),
    }
}

fn field(i: &crate::doc::Instrument) -> String {
    if i.is_drums { "kit".into() } else { format!("program {}", i.program) }
}

fn kind(b: &PatternBody) -> &'static str {
    match b {
        PatternBody::Melodic(_) => "melodic",
        PatternBody::Chordal(_) => "chordal",
        PatternBody::Drums(_) => "drums",
    }
}

fn row_text(r: &Row) -> String {
    let stack = if r.stack.is_empty() {
        "z".to_string()
    } else {
        r.stack.iter().map(|id| format!("P{id}")).collect::<Vec<_>>().join("+")
    };
    let label = r.label.as_ref().map(|l| format!("{l}: ")).unwrap_or_default();
    if r.reps == 1 { format!("{label}[{stack}]") } else { format!("{label}[{stack}] x{}", r.reps) }
}

fn diff_pattern(out: &mut String, pa: &PatternDef, pb: &PatternDef, fa: bool, fb: bool) {
    if pa.track != pb.track {
        let _ = writeln!(out, "P{}: instrument changed", pa.id);
    }
    if pa.kin != pb.kin {
        let show = |k: Option<usize>| k.map(|k| format!("~P{k}")).unwrap_or_else(|| "-".into());
        let _ = writeln!(out, "P{}: kin {} -> {}", pa.id, show(pa.kin), show(pb.kin));
    }
    if pa.base_vel != pb.base_vel {
        let _ = writeln!(
            out,
            "P{}: dynamic {} -> {}",
            pa.id,
            notation::vel_to_dynamic(pa.base_vel).0,
            notation::vel_to_dynamic(pb.base_vel).0
        );
    }
    match (&pa.body, &pb.body) {
        (PatternBody::Melodic(ba), PatternBody::Melodic(bb)) => {
            for i in 0..ba.len().max(bb.len()) {
                match (ba.get(i), bb.get(i)) {
                    (Some(x), Some(y)) => {
                        let (sx, sy) = (spell_melodic_bar(x, fa), spell_melodic_bar(y, fb));
                        if sx != sy {
                            let _ =
                                writeln!(out, "P{} bar {}: | {} | -> | {} |", pa.id, i + 1, sx, sy);
                        }
                    }
                    (Some(x), None) => {
                        let _ = writeln!(
                            out,
                            "P{} bar {} removed: | {} |",
                            pa.id,
                            i + 1,
                            spell_melodic_bar(x, fa)
                        );
                    }
                    (None, Some(y)) => {
                        let _ = writeln!(
                            out,
                            "P{} bar {} added: | {} |",
                            pa.id,
                            i + 1,
                            spell_melodic_bar(y, fb)
                        );
                    }
                    (None, None) => unreachable!(),
                }
            }
        }
        (PatternBody::Chordal(ba), PatternBody::Chordal(bb)) => {
            for i in 0..ba.len().max(bb.len()) {
                let sx = ba.get(i).map(|c| spell_chordal_bar(c, fa));
                let sy = bb.get(i).map(|c| spell_chordal_bar(c, fb));
                if sx != sy {
                    let show = |s: Option<String>| s.unwrap_or_else(|| "(none)".into());
                    let _ = writeln!(
                        out,
                        "P{} bar {}: | {} | -> | {} |",
                        pa.id,
                        i + 1,
                        show(sx),
                        show(sy)
                    );
                }
            }
        }
        (PatternBody::Drums(da), PatternBody::Drums(db)) => diff_drums(out, pa.id, da, db),
        _ => {
            let _ = writeln!(out, "P{}: {} -> {}", pa.id, kind(&pa.body), kind(&pb.body));
        }
    }
}

fn diff_drums(out: &mut String, id: usize, da: &DrumsBody, db: &DrumsBody) {
    if da.variant_base != db.variant_base {
        let show = |v: Option<usize>| v.map(|b| format!("~P{b}")).unwrap_or_else(|| "-".into());
        let _ = writeln!(out, "P{id}: base {} -> {}", show(da.variant_base), show(db.variant_base));
    }
    let lane_str = |cells: &[u8]| cells.iter().map(|c| lane_char(*c)).collect::<String>();
    for (pitch, cells) in &da.lanes {
        match db.lanes.iter().find(|(p, _)| p == pitch) {
            None => {
                let _ = writeln!(out, "P{id} lane {} removed", drums::lane_label(*pitch));
            }
            Some((_, other)) if other != cells => {
                let _ = writeln!(
                    out,
                    "P{id} lane {}: |{}| -> |{}|",
                    drums::lane_label(*pitch),
                    lane_str(cells),
                    lane_str(other)
                );
            }
            _ => {}
        }
    }
    for (pitch, cells) in &db.lanes {
        if !da.lanes.iter().any(|(p, _)| p == pitch) {
            let _ = writeln!(
                out,
                "P{id} lane {} added: |{}|",
                drums::lane_label(*pitch),
                lane_str(cells)
            );
        }
    }
}
