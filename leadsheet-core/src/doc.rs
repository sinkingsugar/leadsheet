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

    /// Structural preflight for host-built Documents (parse_document
    /// output is valid by construction). Checks everything emission and
    /// resolution assume: reference integrity, bar sums, lane shapes,
    /// and header sanity — including that text fields can't break the
    /// emitted syntax (decision B3: names are whitelisted, labels can't
    /// carry structural characters). The contract this enforces is the
    /// `document_canonicality` property: a Document that validates emits
    /// text that reparses and resolves to the same music.
    pub fn validate(&self) -> Result<(), Error> {
        validate_header(&self.header)?;
        // B1 (triage-3): source BPM is canonically hundredth-quantized —
        // emission spells `{:.2}`, so finer precision would be silently
        // rewritten by the first `fmt`. This is a *source* rule: QSong
        // bpm stays a raw f64 (ingest measures arbitrary tempos) and
        // `from_qsong` quantizes at the boundary.
        if !bpm_is_canonical(self.header.bpm) {
            return Err(doc_err(format!(
                "tempo {} carries sub-hundredth precision the text cannot hold (nearest: {:.2})",
                self.header.bpm, self.header.bpm
            )));
        }
        let cpb = self.header.cells_per_bar();
        let bar = self.header.bar_ticks();
        let mut names = std::collections::HashSet::new();
        for i in &self.instruments {
            if !valid_name(&i.name) {
                return Err(doc_err(format!(
                    "instrument name {:?} would break emission (use letters, digits, _ or -)",
                    i.name
                )));
            }
            if !names.insert(&i.name) {
                return Err(doc_err(format!("duplicate instrument {:?}", i.name)));
            }
            validate_program(i.program, i.is_drums, true, &format!("instrument {}", i.name))?;
        }
        let mut ids = std::collections::HashSet::new();
        for (idx, p) in self.patterns.iter().enumerate() {
            if !ids.insert(p.id) {
                return Err(doc_err(format!("duplicate pattern P{}", p.id)));
            }
            if p.track >= self.instruments.len() {
                return Err(doc_err(format!("P{}: track {} out of range", p.id, p.track)));
            }
            let is_drums = self.instruments[p.track].is_drums;
            if matches!(p.body, PatternBody::Drums(_)) != is_drums {
                return Err(doc_err(format!("P{}: body kind does not match the instrument", p.id)));
            }
            validate_base_vel(p.base_vel, &format!("P{}", p.id))?;
            if let Some(kin) = p.kin {
                // Kinship is spelled on the pattern's name field; drums
                // spell variant bases there instead, so a drums `kin`
                // would be silently dropped by emission.
                if is_drums {
                    return Err(doc_err(format!(
                        "P{}: drum patterns carry their base in variant_base, not kin",
                        p.id
                    )));
                }
                if !self.patterns[..idx].iter().any(|q| q.id == kin && q.track == p.track) {
                    return Err(doc_err(format!(
                        "P{}: kin P{kin} is not an earlier pattern on the same instrument",
                        p.id
                    )));
                }
            }
            validate_body(&p.body, cpb, bar, &self.patterns[..idx], p.track, format!("P{}", p.id))?;
        }
        let mut total_bars = 0u64;
        for item in &self.timeline {
            match item {
                TimelineItem::Row(row) => {
                    if row.reps == 0 {
                        return Err(doc_err("row repeats must be >= 1".into()));
                    }
                    // What actually breaks the emitted row line: `[`
                    // ends the label early, `|` reroutes the line as a
                    // music line, a leading `#` turns it into a comment,
                    // and surrounding whitespace doesn't survive the
                    // reparse trim. (`]` before the `[` is harmless.)
                    if let Some(l) = &row.label
                        && (l.contains(['\n', '[', '|'])
                            || l.starts_with('#')
                            || l.trim() != l.as_str())
                    {
                        return Err(doc_err(format!("row label {l:?} would break emission")));
                    }
                    let mut unit = 1u32;
                    for id in &row.stack {
                        let p = self
                            .pattern(*id)
                            .ok_or_else(|| doc_err(format!("unknown pattern P{id} in a row")))?;
                        unit = unit.max(p.body.n_bars());
                    }
                    for id in &row.stack {
                        let len = self.pattern(*id).unwrap().body.n_bars();
                        if len != 1 && len != unit {
                            return Err(doc_err(format!(
                                "P{id} is {len} bars but its row unit is {unit}"
                            )));
                        }
                    }
                    total_bars += row.reps as u64 * unit as u64;
                }
                TimelineItem::Direct(d) => {
                    if d.bar == 0 {
                        return Err(doc_err("direct bars start at 1".into()));
                    }
                    if d.track >= self.instruments.len() {
                        return Err(doc_err(format!("b{}: track out of range", d.bar)));
                    }
                    if matches!(d.body, PatternBody::Drums(_)) != self.instruments[d.track].is_drums
                    {
                        return Err(doc_err(format!(
                            "b{}: body kind does not match the instrument",
                            d.bar
                        )));
                    }
                    validate_base_vel(d.base_vel, &format!("b{}", d.bar))?;
                    validate_body(
                        &d.body,
                        cpb,
                        bar,
                        &self.patterns,
                        d.track,
                        format!("b{}", d.bar),
                    )?;
                    total_bars = total_bars.max(d.bar as u64 + d.body.n_bars() as u64 - 1);
                }
            }
        }
        if total_bars > 100_000 {
            return Err(doc_err(format!("{total_bars} bars is beyond the 100000-bar limit")));
        }
        Ok(())
    }

    /// Compile to a flat, tick-placed [`QSong`].
    ///
    /// Runs [`Document::validate`] first, so hand-built Documents (hosts,
    /// wasm callers) get a structured error instead of a panic on
    /// malformed state. Documents from [`crate::parse::parse_document`]
    /// are already valid with line-accurate diagnostics and pay a
    /// negligible re-check.
    pub fn resolve(&self) -> Result<QSong, Error> {
        self.validate()?;
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

impl QSong {
    /// Structural preflight for host-built songs (quantizer/resolver
    /// output is valid by construction): header sanity, emittable unique
    /// track names, GM-range programs, MIDI-range pitches and velocities,
    /// positive durations, drum hits on the 16th grid with stroke digits
    /// 1..=4, and no notes beyond `n_bars` (emission silently drops
    /// those).
    pub fn validate(&self) -> Result<(), Error> {
        validate_header(&Header {
            name: self.name.clone(),
            bpm: self.bpm,
            meter: self.meter,
            key: self.key,
            swing: self.swing,
        })?;
        let mut names = std::collections::HashSet::new();
        // Bounded given the validated header: bar_ticks <= 64 * 960
        // and n_bars is u32, so the product stays far below i64::MAX.
        let end = self.bar_ticks() * self.n_bars as i64;
        for t in &self.tracks {
            if !valid_name(&t.name) {
                return Err(doc_err(format!(
                    "track name {:?} would break emission (use letters, digits, _ or -)",
                    t.name
                )));
            }
            if !names.insert(&t.name) {
                return Err(doc_err(format!("duplicate track {:?}", t.name)));
            }
            validate_program(t.program, t.is_drums, false, &t.name)?;
            for n in &t.notes {
                if n.pitch > 127 {
                    return Err(doc_err(format!("{}: pitch {} beyond MIDI 127", t.name, n.pitch)));
                }
                // 0 is note-off semantics on the wire; render would
                // silently clamp anything outside MIDI's domain (A3).
                if n.vel == 0 || n.vel > 127 {
                    return Err(doc_err(format!(
                        "{}: velocity {} outside MIDI 1..=127",
                        t.name, n.vel
                    )));
                }
                if n.dur <= MusicalTime::ZERO || n.onset < MusicalTime::ZERO {
                    return Err(doc_err(format!("{}: non-positive time on a note", t.name)));
                }
                if t.is_drums {
                    if n.onset.try_as_sixteenths().is_none() {
                        return Err(doc_err(format!("{}: drum hit off the 16th grid", t.name)));
                    }
                    if !(1..=4).contains(&n.strokes) {
                        return Err(doc_err(format!("{}: stroke digit out of range", t.name)));
                    }
                } else if n.strokes != 1 {
                    return Err(doc_err(format!("{}: melodic notes have strokes = 1", t.name)));
                }
                let extent = if t.is_drums { MusicalTime::from_sixteenths(1) } else { n.dur };
                // Total arithmetic (A2): a hostile onset near i64::MAX
                // passes the sign checks and would overflow a bare add —
                // the validator is the one thing that must never trust
                // its input.
                match n.onset.ticks().checked_add(extent.ticks()) {
                    Some(e) if e <= end.ticks() => {}
                    _ => {
                        return Err(doc_err(format!(
                            "{}: a note ends past n_bars (emission would drop it)",
                            t.name
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Program domains (A1, triage-4): melodic programs are GM 0..=127 on
/// both layers (render feeds them to `u7::new` unguarded). Kit programs
/// are real state on the *compiled* layer — GM2 kit selects ride
/// ProgramChange on channel 10: ingest measures them, render honors
/// them — but the TEXT has no slot for them (`drums:kit`), so the
/// source layer requires 0; `from_qsong` normalizes at the boundary in,
/// exactly like BPM (B1).
fn validate_program(program: u8, is_drums: bool, source: bool, who: &str) -> Result<(), Error> {
    if program > 127 {
        return Err(doc_err(format!("{who}: program {program} beyond GM 127")));
    }
    if source && is_drums && program != 0 {
        return Err(doc_err(format!(
            "{who}: the text has no slot for a kit program (`drums:kit`) — use 0"
        )));
    }
    Ok(())
}

/// Canonical BPM = survives its own `{:.2}` spelling (the emitted form).
/// Defined by the round-trip rather than arithmetic so there is exactly
/// one notion of "fits in the text" and no float-rounding edge between
/// the check and the emitter.
pub(crate) fn bpm_is_canonical(bpm: f64) -> bool {
    format!("{bpm:.2}").parse::<f64>() == Ok(bpm)
}

/// The name whitelist (decision B3): the only instrument/track names that
/// survive the emit → parse loop unambiguously. Shared by both validation
/// boundaries and the parser so the three can't drift.
pub(crate) fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `@dyn` marks only spell the six dynamic buckets, so an off-bucket base
/// would silently shift every velocity through a reparse.
fn validate_base_vel(base: u8, who: &str) -> Result<(), Error> {
    if !notation::DYNAMICS.iter().any(|(_, v)| *v == base) {
        return Err(doc_err(format!(
            "{who}: base velocity {base} is not a dynamic bucket (32/48/64/80/96/112)"
        )));
    }
    Ok(())
}

fn validate_header(h: &Header) -> Result<(), Error> {
    if !h.bpm.is_finite() || h.bpm <= 0.0 {
        return Err(doc_err(format!("bad tempo {}", h.bpm)));
    }
    // MIDI tempo is 24-bit µs/quarter; keep it representable.
    let us = 60e6 / h.bpm;
    if !(1.0..=16_777_215.0).contains(&us) {
        return Err(doc_err(format!("tempo {} is outside the MIDI-representable range", h.bpm)));
    }
    if h.meter.1 != 4 && h.meter.1 != 8 {
        return Err(doc_err(format!("unsupported meter {}/{}", h.meter.0, h.meter.1)));
    }
    // Key spelling indexes a 12-name table; the helpers are total (mod
    // 12), but a noncanonical pc would still normalize through the text.
    if let Some(k) = h.key
        && k.tonic_pc >= 12
    {
        return Err(doc_err(format!("key tonic pitch class {} out of range (0..=11)", k.tonic_pc)));
    }
    if h.meter.0 == 0 || h.meter.0 > 64 {
        return Err(doc_err(format!("meter numerator {} out of range (1..=64)", h.meter.0)));
    }
    if let Some(sw) = h.swing
        && (!(50..=75).contains(&sw.percent) || (sw.level != 8 && sw.level != 16))
    {
        return Err(doc_err("bad swing (level 8/16, percent 50..=75)".into()));
    }
    if h.name.contains("tempo:") || h.name.contains('\n') || h.name.trim() != h.name {
        return Err(doc_err(format!("song name {:?} would break emission", h.name)));
    }
    Ok(())
}

fn validate_body(
    body: &PatternBody,
    cpb: u32,
    bar: MusicalTime,
    earlier: &[PatternDef],
    track: usize,
    who: String,
) -> Result<(), Error> {
    match body {
        PatternBody::Melodic(bars) => {
            if bars.is_empty() {
                return Err(doc_err(format!("{who}: a body needs at least one bar")));
            }
            for mb in bars {
                for voice in &mb.voices {
                    let mut sum = 0i64;
                    for tok in voice {
                        validate_tok(tok, &who)?;
                        sum = sum.saturating_add(tok.dur().ticks());
                    }
                    if sum != bar.ticks() && sum != 0 {
                        return Err(doc_err(format!(
                            "{who}: a voice covers {sum} of {} ticks",
                            bar.ticks()
                        )));
                    }
                }
            }
        }
        PatternBody::Chordal(bars) => {
            if bars.is_empty() {
                return Err(doc_err(format!("{who}: a body needs at least one bar")));
            }
            for cols in bars {
                if cols.len() != (cpb / 4) as usize {
                    return Err(doc_err(format!(
                        "{who}: chord bar has {} columns, expected {}",
                        cols.len(),
                        cpb / 4
                    )));
                }
                let mut have = false;
                for c in cols {
                    match c {
                        ChordCol::Sym(sym) => {
                            // Representation before voicability: a
                            // noncanonical pc (13 ≡ 1) voices fine, then
                            // normalizes through emission and reparses
                            // as a different Document — silent mutation.
                            if sym.root_pc >= 12 || sym.bass_pc >= 12 {
                                return Err(doc_err(format!(
                                    "{who}: chord pitch class out of range (0..=11)"
                                )));
                            }
                            if sym.quality >= chord::QUALITIES.len() {
                                return Err(doc_err(format!(
                                    "{who}: chord quality index out of range"
                                )));
                            }
                            if chord::voicing(sym).is_none() {
                                return Err(doc_err(format!("{who}: unvoicable chord symbol")));
                            }
                            have = true;
                        }
                        ChordCol::Hold if !have => {
                            return Err(doc_err(format!("{who}: hold with no chord before it")));
                        }
                        ChordCol::Hold => {}
                        ChordCol::Rest => have = false,
                    }
                }
            }
        }
        PatternBody::Drums(d) => {
            if let Some(base) = d.variant_base {
                // Same-instrument, like the parser: a variant inherits
                // lanes, so base and variant must share a kit.
                let ok = earlier.iter().any(|p| {
                    p.id == base && p.track == track && matches!(p.body, PatternBody::Drums(_))
                });
                if !ok {
                    return Err(doc_err(format!(
                        "{who}: variant base P{base} is not an earlier drum pattern on the same \
                         instrument"
                    )));
                }
            }
            let mut lane_pitches = std::collections::HashSet::new();
            for (pitch, cells) in &d.lanes {
                if *pitch > 127 {
                    return Err(doc_err(format!("{who}: lane pitch {pitch} beyond MIDI 127")));
                }
                if !lane_pitches.insert(*pitch) {
                    return Err(doc_err(format!(
                        "{who}: duplicate lane {}",
                        crate::drums::lane_label(*pitch)
                    )));
                }
                if cells.len() != cpb as usize {
                    return Err(doc_err(format!(
                        "{who}: lane has {} cells, expected {cpb}",
                        cells.len()
                    )));
                }
                if cells.iter().any(|c| *c > LANE_D4) {
                    return Err(doc_err(format!("{who}: bad lane cell code")));
                }
            }
        }
    }
    Ok(())
}

fn validate_tok(tok: &Tok, who: &str) -> Result<(), Error> {
    if tok.dur() <= MusicalTime::ZERO {
        return Err(doc_err(format!("{who}: non-positive duration")));
    }
    validate_pitches(tok, who)?;
    if let Tok::Tuplet { n, members, span, tie } = tok {
        if !(2..=24).contains(n) || members.len() != *n as usize {
            return Err(doc_err(format!("{who}: malformed tuplet")));
        }
        // Parse-canonical member shape: a shorter span would silently
        // drop members at placement, a member tie is emitter-
        // unrepresentable (ties live on the group), member durations
        // must agree with the placement boundaries, and a tied group
        // cannot end in a rest.
        if span.ticks() < *n as i64 {
            return Err(doc_err(format!("{who}: tuplet span is shorter than its {n} members")));
        }
        if *tie && matches!(members.last(), Some(Tok::Rest { .. })) {
            return Err(doc_err(format!("{who}: a tuplet ending in a rest cannot be tied")));
        }
        for (i, m) in members.iter().enumerate() {
            let want = notation::tuplet_boundary(*span, *n, i as u32 + 1)
                - notation::tuplet_boundary(*span, *n, i as u32);
            let (dur, member_tie) = match m {
                Tok::Note { dur, tie, .. } | Tok::Chord { dur, tie, .. } => (*dur, *tie),
                Tok::Rest { dur } => (*dur, false),
                Tok::Tuplet { .. } => {
                    return Err(doc_err(format!("{who}: tuplets cannot nest")));
                }
            };
            if member_tie {
                return Err(doc_err(format!("{who}: ties go on the tuplet group, not a member")));
            }
            if dur != want {
                return Err(doc_err(format!(
                    "{who}: tuplet member {i} carries {} ticks, the boundary rule says {}",
                    dur.ticks(),
                    want.ticks()
                )));
            }
            validate_pitches(m, who)?;
        }
    }
    if let Tok::Chord { pitches, .. } = tok
        && pitches.is_empty()
    {
        return Err(doc_err(format!("{who}: empty chord")));
    }
    Ok(())
}

/// Pitches beyond MIDI 127 spell to text [`crate::notation::parse_pitch`]
/// rejects: validated Document, unparseable emission.
fn validate_pitches(tok: &Tok, who: &str) -> Result<(), Error> {
    let bad = match tok {
        Tok::Note { pitch, .. } => *pitch > 127,
        Tok::Chord { pitches, .. } => pitches.iter().any(|p| *p > 127),
        _ => false,
    };
    if bad {
        return Err(doc_err(format!("{who}: pitch beyond MIDI 127")));
    }
    Ok(())
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
                    // validate() requires span >= n, which makes every
                    // boundary step positive — a zero-width member here
                    // would be silent data loss, our one forbidden thing.
                    if dur <= MusicalTime::ZERO {
                        return Err(doc_err(format!(
                            "tuplet member collapsed to zero width (span {} / {n})",
                            span.ticks()
                        )));
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
