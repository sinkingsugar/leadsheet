//! The Document — the faithful AST of a `.ls` file, and the middle layer
//! of the pipeline:
//!
//! ```text
//!   .ls text  ←→  Document (this)  ←→  QSong (compiled)  ←→  MIDI
//!   [parse_document / emit_document]  [resolve / from_qsong]
//! ```
//!
//! Unlike `QSong` (a flat bag of tick-placed notes), the Document keeps
//! the *source structure*: pattern definitions with their author-chosen
//! ids, drum-variant lane diffs as written, multi-bar patterns,
//! arrangement rows with labels, and direct bars. `leadsheet fmt` is
//! Document-canonical — hand-authored structure survives it; only
//! `leadsheet compress` invents structure (via `emit::from_qsong`).
//!
//! Tuplet groups live here as semantic objects ([`Tok::Tuplet`]); tick
//! placement happens only in [`Document::resolve`], in one function
//! (`Builder::place_tok`), by the DESIGN-960 rule: member *i* of an
//! n-tuplet over span S starts at `round(i·S/n)` and the span always
//! closes exactly — which is what makes inexact divisions (septuplets)
//! representable in source while `QSong` sees only their rounded ticks.

use crate::chord::{self, ChordSym};
use crate::drums::{LANE_ACCENT, LANE_D2, LANE_D3, LANE_D4, LANE_GHOST, LANE_HIT};
use crate::error::{Diagnostic, Error};
use crate::grid::{MusicalTime, QNote, QSong, QTrack, Swing};
use crate::key::Key;
use crate::notation::{self, Mark, Tok};
use std::collections::HashMap;

/// The `# song:` line.
#[derive(Debug, Clone, PartialEq)]
pub struct Header {
    pub name: String,
    pub bpm: f64,
    pub meter: (u32, u32),
    pub key: Option<Key>,
    pub swing: Option<Swing>,
}

impl Header {
    pub fn cells_per_bar(&self) -> u32 {
        self.meter.0 * crate::grid::CELLS_PER_BEAT * 4 / self.meter.1
    }

    pub fn bar_ticks(&self) -> MusicalTime {
        MusicalTime(self.meter.0 as i64 * crate::grid::TICKS_PER_BEAT * 4 / self.meter.1 as i64)
    }
}

/// One `# instruments:` entry, in track order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    pub name: String,
    pub program: u8,
    pub is_drums: bool,
}

/// One melodic bar: `&`-separated voices of tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct MelodicBar {
    pub voices: Vec<Vec<Tok>>,
}

/// One chord-mode column (a beat).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ChordCol {
    Sym(ChordSym),
    Hold,
    Rest,
}

/// Drum lanes as written: for a variant (`variant_base` set) these are
/// the *diff* lanes only — unlisted lanes inherit from the base at
/// resolve time (an all-dots lane clears an inherited one). Cell codes
/// are the shared lane vocabulary in [`crate::drums`].
#[derive(Debug, Clone, PartialEq)]
pub struct DrumsBody {
    pub variant_base: Option<usize>,
    pub lanes: Vec<(u8, Vec<u8>)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PatternBody {
    Melodic(Vec<MelodicBar>),
    Chordal(Vec<Vec<ChordCol>>),
    /// Always exactly one bar.
    Drums(DrumsBody),
}

impl PatternBody {
    pub fn n_bars(&self) -> u32 {
        match self {
            PatternBody::Melodic(bars) => bars.len() as u32,
            PatternBody::Chordal(bars) => bars.len() as u32,
            PatternBody::Drums(_) => 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PatternDef {
    /// The author-chosen `P<n>` id (compressor uses 1..=n).
    pub id: usize,
    /// Index into [`Document::instruments`].
    pub track: usize,
    /// Base velocity from the `@dyn` mark.
    pub base_vel: u8,
    /// Informational kinship (`~P<n>`) for melodic/chordal patterns.
    /// (Drum variants carry their base in [`DrumsBody::variant_base`].)
    pub kin: Option<usize>,
    pub body: PatternBody,
}

/// One arrangement row: a bar-stack repeated `reps` times.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Section label as written (the compressor derives these; in the
    /// Document they're source content and survive `fmt`).
    pub label: Option<String>,
    pub stack: Vec<usize>,
    pub reps: u32,
}

/// A `b<n>` direct placement (hand-authoring shortcut).
#[derive(Debug, Clone, PartialEq)]
pub struct DirectItem {
    /// 1-based bar number of the first bar.
    pub bar: u32,
    pub track: usize,
    pub base_vel: u8,
    pub body: PatternBody,
}

/// Rows and direct bars, in source order — the order is semantic: a tie
/// left open by one item is joined by whichever later item continues it.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineItem {
    Row(Row),
    Direct(DirectItem),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub header: Header,
    pub instruments: Vec<Instrument>,
    /// In source order.
    pub patterns: Vec<PatternDef>,
    /// Arrangement rows and direct bars, interleaved as written.
    pub timeline: Vec<TimelineItem>,
}

impl Document {
    pub fn pattern(&self, id: usize) -> Option<&PatternDef> {
        self.patterns.iter().find(|p| p.id == id)
    }

    pub fn rows(&self) -> impl Iterator<Item = &Row> {
        self.timeline.iter().filter_map(|i| match i {
            TimelineItem::Row(r) => Some(r),
            _ => None,
        })
    }

    pub fn directs(&self) -> impl Iterator<Item = &DirectItem> {
        self.timeline.iter().filter_map(|i| match i {
            TimelineItem::Direct(d) => Some(d),
            _ => None,
        })
    }

    /// Compile to a flat, tick-placed [`QSong`].
    ///
    /// Documents built by [`crate::parse::parse_document`] are already
    /// validated with line-accurate diagnostics; the checks here guard
    /// hand-built Documents (hosts, wasm callers) and carry no line info.
    pub fn resolve(&self) -> Result<QSong, Error> {
        let cpb = self.header.cells_per_bar();
        let bar_len = self.header.bar_ticks();
        let mut b = Builder {
            tracks: self
                .instruments
                .iter()
                .map(|i| QTrack {
                    name: i.name.clone(),
                    program: i.program,
                    is_drums: i.is_drums,
                    notes: Vec::new(),
                })
                .collect(),
            open_ties: HashMap::new(),
        };

        // Drum-variant lane inheritance resolves once, in source order
        // (bases are defined before their variants).
        let mut pattern_lanes: HashMap<usize, Vec<(u8, Vec<u8>)>> = HashMap::new();
        for p in &self.patterns {
            if let PatternBody::Drums(d) = &p.body {
                let lanes = merge_variant_lanes(d, &pattern_lanes)?;
                pattern_lanes.insert(p.id, lanes);
            }
        }

        let mut next_bar = 0u32;
        let mut max_bar = 0u32;
        for item in &self.timeline {
            match item {
                TimelineItem::Row(row) => {
                    let mut unit = 1u32;
                    for id in &row.stack {
                        let p = self.pattern(*id).ok_or_else(|| {
                            doc_err(format!("unknown pattern P{id} in arrangement"))
                        })?;
                        unit = unit.max(p.body.n_bars());
                    }
                    for id in &row.stack {
                        let len = self.pattern(*id).unwrap().body.n_bars();
                        if len != 1 && len != unit {
                            return Err(doc_err(format!(
                                "P{id} is {len} bars but the row unit is {unit}"
                            )));
                        }
                    }
                    for _ in 0..row.reps {
                        for offset in 0..unit {
                            for id in &row.stack {
                                let p = self.pattern(*id).unwrap();
                                // 1-bar patterns repeat over a longer unit.
                                let bar_in_body = if p.body.n_bars() == 1 { 0 } else { offset };
                                let lanes = pattern_lanes.get(&p.id);
                                b.apply_body(
                                    &p.body,
                                    lanes.map(Vec::as_slice),
                                    p.track,
                                    bar_in_body,
                                    next_bar + offset,
                                    p.base_vel,
                                    cpb,
                                    bar_len,
                                )?;
                            }
                        }
                        next_bar += unit;
                    }
                }
                TimelineItem::Direct(item) => {
                    let lanes = match &item.body {
                        PatternBody::Drums(d) => Some(merge_variant_lanes(d, &pattern_lanes)?),
                        _ => None,
                    };
                    let n = item.body.n_bars();
                    for offset in 0..n {
                        b.apply_body(
                            &item.body,
                            lanes.as_deref(),
                            item.track,
                            offset,
                            item.bar - 1 + offset,
                            item.base_vel,
                            cpb,
                            bar_len,
                        )?;
                    }
                    max_bar = max_bar.max(item.bar + n - 1);
                }
            }
        }

        let mut max_end = MusicalTime::ZERO;
        for t in &mut b.tracks {
            t.notes.sort_by(|a, x| a.onset.cmp(&x.onset).then(a.pitch.cmp(&x.pitch)));
            for n in &t.notes {
                max_end = max_end.max(n.onset + n.dur);
            }
        }
        Ok(QSong {
            name: self.header.name.clone(),
            bpm: self.header.bpm,
            meter: self.header.meter,
            key: self.header.key,
            swing: self.header.swing,
            n_bars: max_end.spans_ceil(bar_len).max(next_bar).max(max_bar),
            tracks: b.tracks,
        })
    }
}

/// Full lanes for a drums body: the variant diff merged over its base's
/// resolved lanes.
fn merge_variant_lanes(
    d: &DrumsBody,
    resolved: &HashMap<usize, Vec<(u8, Vec<u8>)>>,
) -> Result<Vec<(u8, Vec<u8>)>, Error> {
    let Some(base_id) = d.variant_base else {
        return Ok(d.lanes.clone());
    };
    let base = resolved
        .get(&base_id)
        .ok_or_else(|| doc_err(format!("unknown drum variant base P{base_id}")))?;
    let mut merged = base.clone();
    for (pitch, cells) in &d.lanes {
        merged.retain(|(p, _)| p != pitch);
        merged.push((*pitch, cells.clone()));
    }
    Ok(merged)
}

fn doc_err(message: String) -> Error {
    Error::Parse(Diagnostic { code: "bad-document", line: 0, col: 0, message, suggestion: None })
}

struct Builder {
    tracks: Vec<QTrack>,
    /// Open ties per (track, pitch): (note index, end tick). A list, so
    /// doubled pitches keep every continuation; matched by end position.
    open_ties: HashMap<(usize, u8), Vec<(usize, MusicalTime)>>,
}

impl Builder {
    /// Place one bar of a body at absolute bar `abs_bar`. For drums,
    /// `drum_lanes` carries the variant-resolved lanes.
    #[allow(clippy::too_many_arguments)]
    fn apply_body(
        &mut self,
        body: &PatternBody,
        drum_lanes: Option<&[(u8, Vec<u8>)]>,
        track: usize,
        bar_in_body: u32,
        abs_bar: u32,
        base: u8,
        cpb: u32,
        bar_len: MusicalTime,
    ) -> Result<(), Error> {
        if track >= self.tracks.len() {
            return Err(doc_err(format!("track index {track} out of range")));
        }
        let bar_start = bar_len * abs_bar as i64;
        match body {
            PatternBody::Melodic(bars) => {
                let bar = bars
                    .get(bar_in_body as usize)
                    .ok_or_else(|| doc_err(format!("bar {bar_in_body} out of range")))?;
                for voice in &bar.voices {
                    let mut cursor = bar_start;
                    for tok in voice {
                        self.place_tok(track, cursor, tok, base, bar_start + bar_len)?;
                        cursor += tok.dur();
                    }
                }
            }
            PatternBody::Chordal(bars) => {
                let cols = bars
                    .get(bar_in_body as usize)
                    .ok_or_else(|| doc_err(format!("bar {bar_in_body} out of range")))?;
                let beat = MusicalTime(crate::grid::TICKS_PER_BEAT);
                let mut current: Option<(Vec<u8>, MusicalTime, MusicalTime)> = None;
                for (i, col) in cols.iter().enumerate() {
                    match col {
                        ChordCol::Hold => {
                            if let Some(c) = current.as_mut() {
                                c.2 += beat;
                            }
                        }
                        ChordCol::Rest => self.flush_chord(track, base, &mut current),
                        ChordCol::Sym(sym) => {
                            self.flush_chord(track, base, &mut current);
                            let pitches = chord::voicing(sym).ok_or_else(|| {
                                doc_err(format!(
                                    "unvoicable chord {}",
                                    chord::symbol_to_string(sym, false)
                                ))
                            })?;
                            current = Some((pitches, bar_start + beat * i as i64, beat));
                        }
                    }
                }
                self.flush_chord(track, base, &mut current);
            }
            PatternBody::Drums(d) => {
                let lanes = drum_lanes.unwrap_or(&d.lanes);
                for (pitch, cells) in lanes {
                    if cells.len() != cpb as usize {
                        return Err(doc_err(format!(
                            "lane has {} cells, expected {cpb}",
                            cells.len()
                        )));
                    }
                    for (i, code) in cells.iter().enumerate() {
                        let (vel, strokes) = match *code {
                            LANE_ACCENT => (notation::apply_mark(base, Mark::Accent), 1),
                            LANE_GHOST => (notation::apply_mark(base, Mark::Ghost), 1),
                            LANE_HIT => (base, 1),
                            LANE_D2 => (base, 2),
                            LANE_D3 => (base, 3),
                            LANE_D4 => (base, 4),
                            _ => continue,
                        };
                        self.tracks[track].notes.push(QNote {
                            pitch: *pitch,
                            onset: bar_start + MusicalTime::from_sixteenths(i as u32),
                            dur: MusicalTime::from_sixteenths(1),
                            strokes,
                            vel,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn flush_chord(
        &mut self,
        track: usize,
        base: u8,
        current: &mut Option<(Vec<u8>, MusicalTime, MusicalTime)>,
    ) {
        if let Some((pitches, start, dur)) = current.take() {
            for pitch in pitches {
                self.tracks[track].notes.push(QNote {
                    pitch,
                    onset: start,
                    dur,
                    strokes: 1,
                    vel: base,
                });
            }
        }
    }

    /// Place one token at `cursor`. THE tuplet placement function: member
    /// *i* of an n-tuplet over span S starts at `round(i·S/n)`; the span
    /// closes at exactly S. Inexact divisions round here and only here.
    fn place_tok(
        &mut self,
        track: usize,
        cursor: MusicalTime,
        tok: &Tok,
        base: u8,
        bar_end: MusicalTime,
    ) -> Result<(), Error> {
        if cursor + tok.dur() > bar_end {
            return Err(doc_err(format!("bar overflows at token {:?}", notation::emit_token(tok))));
        }
        match tok {
            Tok::Rest { .. } => {}
            Tok::Note { pitch, dur, tie, mark } => {
                self.place(track, cursor, *dur, &[*pitch], *tie, notation::apply_mark(base, *mark));
            }
            Tok::Chord { pitches, dur, tie, mark } => {
                self.place(track, cursor, *dur, pitches, *tie, notation::apply_mark(base, *mark));
            }
            Tok::Tuplet { n, members, span, tie } => {
                let boundary = |i: u32| notation::tuplet_boundary(*span, *n, i);
                for (i, m) in members.iter().enumerate() {
                    let at = cursor + boundary(i as u32);
                    let dur = boundary(i as u32 + 1) - boundary(i as u32);
                    if dur <= MusicalTime::ZERO {
                        continue; // degenerate span smaller than the arity
                    }
                    let last = i + 1 == members.len();
                    match m {
                        Tok::Rest { .. } => {}
                        Tok::Note { pitch, mark, .. } => self.place(
                            track,
                            at,
                            dur,
                            &[*pitch],
                            *tie && last,
                            notation::apply_mark(base, *mark),
                        ),
                        Tok::Chord { pitches, mark, .. } => self.place(
                            track,
                            at,
                            dur,
                            pitches,
                            *tie && last,
                            notation::apply_mark(base, *mark),
                        ),
                        Tok::Tuplet { .. } => {
                            return Err(doc_err("tuplets cannot nest".into()));
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Place one note/chord, joining an open tie that ends exactly at the
    /// onset; register a new tie when asked.
    fn place(
        &mut self,
        track: usize,
        onset: MusicalTime,
        dur: MusicalTime,
        pitches: &[u8],
        tie: bool,
        vel: u8,
    ) {
        for &pitch in pitches {
            let key = (track, pitch);
            let open = self.open_ties.get_mut(&key).and_then(|v| {
                v.iter().position(|&(_, end)| end == onset).map(|i| v.swap_remove(i).0)
            });
            let idx = match open {
                Some(idx) => {
                    self.tracks[track].notes[idx].dur += dur;
                    idx
                }
                None => {
                    self.tracks[track].notes.push(QNote { pitch, onset, dur, strokes: 1, vel });
                    self.tracks[track].notes.len() - 1
                }
            };
            if tie {
                self.open_ties.entry(key).or_default().push((idx, onset + dur));
            }
        }
    }
}
