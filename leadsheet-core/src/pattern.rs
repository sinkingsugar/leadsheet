//! Layer 3 — tracker-style pattern dedup (MOD files solved this in 1987).
//!
//! Bars are canonical strings (the emitter's bar bodies). Identical
//! (instrument, bar-content) pairs share a pattern ID, assigned in order of
//! first appearance. The arrangement is the per-bar stack sequence with
//! run-length encoding. Lossless by construction: instantiating the
//! arrangement reproduces the exact bar sequence that produced it.

use std::collections::HashMap;

pub struct PatternDef {
    /// 1-based, in order of first appearance.
    pub id: usize,
    /// Index into the song's tracks.
    pub track: usize,
    /// Canonical bar body (voices joined with ` & `).
    pub body: String,
}

pub struct Row {
    /// Pattern IDs sounding together in this bar; empty = silent bar.
    pub stack: Vec<usize>,
    pub reps: u32,
}

pub struct PatternSet {
    pub patterns: Vec<PatternDef>,
    pub rows: Vec<Row>,
}

/// `bodies[track][bar]` — `None` for a silent bar.
pub fn build(bodies: &[Vec<Option<String>>]) -> PatternSet {
    let n_bars = bodies.first().map(Vec::len).unwrap_or(0);
    let mut ids: HashMap<(usize, &str), usize> = HashMap::new();
    let mut patterns: Vec<PatternDef> = Vec::new();
    let mut rows: Vec<Row> = Vec::new();
    for bar in 0..n_bars {
        let mut stack = Vec::new();
        for (track, track_bodies) in bodies.iter().enumerate() {
            if let Some(body) = &track_bodies[bar] {
                let id = *ids.entry((track, body.as_str())).or_insert_with(|| {
                    patterns.push(PatternDef { id: patterns.len() + 1, track, body: body.clone() });
                    patterns.len()
                });
                stack.push(id);
            }
        }
        match rows.last_mut() {
            Some(row) if row.stack == stack => row.reps += 1,
            _ => rows.push(Row { stack, reps: 1 }),
        }
    }
    PatternSet { patterns, rows }
}
