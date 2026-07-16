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
use crate::drums::{LANE_ACCENT, LANE_D2, LANE_D3, LANE_D4, LANE_EMPTY, LANE_GHOST, LANE_HIT};
use crate::error::{Diagnostic, Error};
use crate::grid::{Ease, MusicalTime, QAuto, QNote, QSong, QTrack, Swing, Target};
use crate::key::Key;
use crate::notation::{self, Mark, Tok};
use std::collections::HashMap;

/// The `song:` line.
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

/// One `instruments:` entry, in track order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instrument {
    pub name: String,
    pub program: u8,
    pub is_drums: bool,
}

/// The reach of a [`Bind`]: which `@name` lanes it can resolve.
/// `Instrument` binds shadow `Song` binds of the same name (innermost
/// wins), so one name can map to different targets on different tracks.
/// A section scope would need a stable anchor the derived arrangement
/// labels don't provide, so it is deliberately not modeled yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindScope {
    /// Applies to `@name` lanes on any track.
    Song,
    /// Applies only to lanes on this instrument (index into
    /// [`Document::instruments`]); overrides a same-named `Song` bind.
    Instrument(usize),
}

/// A `bind name = target` declaration: gives an automation lane's `@name`
/// a concrete destination. Song-level (`bind cutoff = cc74`) or
/// instrument-scoped (`bind lead.cutoff = cc74`); see [`BindScope`]. An
/// optional `[min..max]` `domain` maps the authored value range onto the
/// target's wire range at render (`bind cutoff = cc74 [0..1]`); without
/// one, values are already in wire units.
#[derive(Debug, Clone, PartialEq)]
pub struct Bind {
    pub scope: BindScope,
    pub name: String,
    pub target: Target,
    pub domain: Option<(f64, f64)>,
    /// `//` comment lines written above this bind (see [`Document`] on
    /// comment attachment).
    pub comments: Vec<String>,
}

impl Bind {
    /// The bind that resolves `@name` on `track`: an `Instrument(track)`
    /// bind wins over a `Song` bind of the same name. Returns the target
    /// and whether the match was instrument-scoped (diagnostics only).
    pub fn resolve<'a>(binds: &'a [Bind], name: &str, track: usize) -> Option<&'a Bind> {
        binds
            .iter()
            .find(|b| b.name == name && b.scope == BindScope::Instrument(track))
            .or_else(|| binds.iter().find(|b| b.name == name && b.scope == BindScope::Song))
    }
}

/// One keyframe of an automation lane: a decimal `value` (in the bound
/// target's units) at a pattern-local position `at`, with the easing that
/// carries it to the next keyframe.
#[derive(Debug, Clone, PartialEq)]
pub struct Keyframe {
    pub at: MusicalTime,
    pub value: f64,
    pub ease: Ease,
}

/// A `@name { pos:value ease ... }` automation lane on a pattern or direct
/// bar: the named parameter's keyframes, in pattern-local time (16th cells
/// from the pattern start).
#[derive(Debug, Clone, PartialEq)]
pub struct AutoLane {
    pub name: String,
    pub keys: Vec<Keyframe>,
    /// `//` comment lines written above this lane.
    pub comments: Vec<String>,
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

/// One step of a drum lane: a plain 16th cell, or a tuplet group
/// spanning several cells. Cell/member codes are the shared lane
/// vocabulary in [`crate::drums`]; group members are stroke codes only
/// (`.` `o` `x` `X` — no nested subdivision digits).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaneItem {
    Cell(u8),
    /// `(n members)span`: n strokes evenly dividing `span` whole cells,
    /// placed by the same boundary rule as melodic tuplets.
    Group {
        n: u8,
        members: Vec<u8>,
        span: u8,
    },
}

impl LaneItem {
    /// Width in 16th cells.
    pub fn width(&self) -> u32 {
        match self {
            LaneItem::Cell(_) => 1,
            LaneItem::Group { span, .. } => *span as u32,
        }
    }
}

/// Drum lanes as written: for a variant (`variant_base` set) these are
/// the *diff* lanes only — unlisted lanes inherit from the base at
/// resolve time (an all-dots lane clears an inherited one). Cell codes
/// are the shared lane vocabulary in [`crate::drums`].
#[derive(Debug, Clone, PartialEq)]
pub struct DrumsBody {
    pub variant_base: Option<usize>,
    pub lanes: Vec<(u8, Vec<LaneItem>)>,
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
    /// Meter override (`P5 drums 3/4`); `None` = the header meter. Every
    /// bar of this pattern is in this meter, and all patterns stacked in
    /// one arrangement row must agree on it.
    pub meter: Option<(u32, u32)>,
    /// Informational kinship (`~P<n>`) for melodic/chordal patterns.
    /// (Drum variants carry their base in [`DrumsBody::variant_base`].)
    pub kin: Option<usize>,
    pub body: PatternBody,
    /// Automation lanes attached to this pattern (`@name { ... }`).
    pub autos: Vec<AutoLane>,
    /// `//` comment lines written above this pattern.
    pub comments: Vec<String>,
}

/// One arrangement row: a bar-stack repeated `reps` times.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Section label as written (the compressor derives these; in the
    /// Document they're source content and survive `fmt`).
    pub label: Option<String>,
    pub stack: Vec<usize>,
    pub reps: u32,
    /// `//` comment lines written above this row.
    pub comments: Vec<String>,
}

/// A `b<n>` direct placement (hand-authoring shortcut).
#[derive(Debug, Clone, PartialEq)]
pub struct DirectItem {
    /// 1-based bar number of the first bar.
    pub bar: u32,
    pub track: usize,
    pub base_vel: u8,
    /// Meter override (`b12 lead 3/4 | ... |`); `None` = the header meter.
    pub meter: Option<(u32, u32)>,
    pub body: PatternBody,
    /// Automation lanes attached to this direct bar (`@name { ... }`).
    pub autos: Vec<AutoLane>,
    /// `//` comment lines written above this direct bar.
    pub comments: Vec<String>,
}

/// Rows and direct bars, in source order — the order is semantic: a tie
/// left open by one item is joined by whichever later item continues it.
#[derive(Debug, Clone, PartialEq)]
pub enum TimelineItem {
    Row(Row),
    Direct(DirectItem),
}

/// # Comments
///
/// `//` comment lines are durable annotations — the model's margin
/// notes — and survive the parse → emit loop. Attachment rule: an
/// own-line comment binds to the **next construct** in the file (the
/// `song:` line, the `instruments:` line, a bind, a pattern, an
/// automation lane, a row, or a direct bar); comments after the last
/// construct are [`Document::trailing_comments`]. Emission places each
/// comment on its own `// text` line immediately above its construct,
/// so a comment written *inside* a drum block (between lanes) migrates
/// below the block on the first `fmt` — deterministic, hence canonical.
/// Stored text is trimmed with the `//` stripped; it must stay
/// trim-stable and newline-free ([`Document::validate`] enforces this).
/// Comments never reach [`QSong`] — `resolve`, `render`, and `diff` are
/// blind to them. `Document::strip_comments` (and `fmt
/// --strip-comments`) discards them all.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub header: Header,
    pub instruments: Vec<Instrument>,
    /// Song-level automation bindings (`bind name = target`).
    pub binds: Vec<Bind>,
    /// In source order.
    pub patterns: Vec<PatternDef>,
    /// Arrangement rows and direct bars, interleaved as written.
    pub timeline: Vec<TimelineItem>,
    /// Comments above the `song:` line (the file preamble).
    pub header_comments: Vec<String>,
    /// Comments above the `instruments:` line.
    pub instruments_comments: Vec<String>,
    /// Comments after the last construct in the file.
    pub trailing_comments: Vec<String>,
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

    /// Discard every comment (the `fmt --strip-comments` operation).
    /// Ephemerality is an operation, not a spelling: there is one
    /// comment form and it is durable; a clean file is one strip away.
    pub fn strip_comments(&mut self) {
        self.header_comments.clear();
        self.instruments_comments.clear();
        self.trailing_comments.clear();
        for b in &mut self.binds {
            b.comments.clear();
        }
        for p in &mut self.patterns {
            p.comments.clear();
            for a in &mut p.autos {
                a.comments.clear();
            }
        }
        for item in &mut self.timeline {
            match item {
                TimelineItem::Row(r) => r.comments.clear(),
                TimelineItem::Direct(d) => {
                    d.comments.clear();
                    for a in &mut d.autos {
                        a.comments.clear();
                    }
                }
            }
        }
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
        validate_comments(&self.header_comments, "header")?;
        validate_comments(&self.instruments_comments, "instruments")?;
        validate_comments(&self.trailing_comments, "trailing")?;
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
        let mut bind_keys = std::collections::HashSet::new();
        for b in &self.binds {
            validate_comments(&b.comments, &format!("bind {}", b.name))?;
            if !valid_name(&b.name) {
                return Err(doc_err(format!("bind name {:?} would break emission", b.name)));
            }
            if let BindScope::Instrument(i) = b.scope
                && i >= self.instruments.len()
            {
                return Err(doc_err(format!(
                    "bind {:?}: instrument index {i} out of range",
                    b.name
                )));
            }
            // One target per (scope, name): a song and an instrument bind
            // of the same name legally coexist (innermost wins).
            if !bind_keys.insert((b.scope, &b.name)) {
                return Err(doc_err(format!("duplicate bind {:?} in the same scope", b.name)));
            }
            validate_target(&b.target)?;
            if !crate::grid::domain_is_canonical(b.domain) {
                return Err(doc_err(format!(
                    "bind {:?}: domain must be [min..max] with min < max on the decimal grid",
                    b.name
                )));
            }
        }
        let mut ids = std::collections::HashSet::new();
        for (idx, p) in self.patterns.iter().enumerate() {
            validate_comments(&p.comments, &format!("P{}", p.id))?;
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
            if let Some(m) = p.meter {
                validate_meter(m, &format!("P{}", p.id))?;
            }
            let (pcpb, pbar) = body_geometry(&self.header, p.meter);
            validate_body(
                &p.body,
                pcpb,
                pbar,
                &self.patterns[..idx],
                p.track,
                format!("P{}", p.id),
            )?;
            validate_autos(
                &p.autos,
                &self.binds,
                p.track,
                pcpb * p.body.n_bars(),
                &format!("P{}", p.id),
            )?;
        }
        let mut total_bars = 0u64;
        for item in &self.timeline {
            match item {
                TimelineItem::Row(row) => {
                    validate_comments(&row.comments, "row")?;
                    if row.reps == 0 {
                        return Err(doc_err("row repeats must be >= 1".into()));
                    }
                    // What actually breaks the emitted row line: `[`
                    // ends the label early, `|` reroutes the line as a
                    // music line, a leading `//` turns it into a comment,
                    // a bare `song`/`instruments` label emits `song: [..]`
                    // which reparses as a header directive, and surrounding
                    // whitespace doesn't survive the reparse trim. (`]`
                    // before the `[` is harmless.)
                    if let Some(l) = &row.label
                        && (l.contains(['\n', '[', '|'])
                            || l.starts_with("//")
                            || l == "song"
                            || l == "instruments"
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
                    validate_comments(&d.comments, &format!("b{}", d.bar))?;
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
                    if let Some(m) = d.meter {
                        validate_meter(m, &format!("b{}", d.bar))?;
                    }
                    let (dcpb, dbar) = body_geometry(&self.header, d.meter);
                    validate_body(
                        &d.body,
                        dcpb,
                        dbar,
                        &self.patterns,
                        d.track,
                        format!("b{}", d.bar),
                    )?;
                    validate_autos(
                        &d.autos,
                        &self.binds,
                        d.track,
                        dcpb * d.body.n_bars(),
                        &format!("b{}", d.bar),
                    )?;
                    total_bars = total_bars.max(d.bar as u64 + d.body.n_bars() as u64 - 1);
                }
            }
        }
        if total_bars > 100_000 {
            return Err(doc_err(format!("{total_bars} bars is beyond the 100000-bar limit")));
        }
        // The global bar-meter map: stack agreement, direct/row meter
        // conflicts, and the renderable tick domain (u28 MIDI deltas;
        // midly masks silently past them) all check here.
        build_bar_meters(&self.header, &self.patterns, &self.timeline)?;
        let _ = (cpb, bar);
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
        let bar_meters = build_bar_meters(&self.header, &self.patterns, &self.timeline)?;
        // Bar start ticks: starts[i] opens bar i, starts[len] ends the song.
        let mut starts: Vec<MusicalTime> = Vec::with_capacity(bar_meters.len() + 1);
        let mut t = MusicalTime::ZERO;
        for m in &bar_meters {
            starts.push(t);
            t += crate::grid::meter_ticks(*m);
        }
        starts.push(t);
        let geometry = |abs_bar: u32| -> (MusicalTime, MusicalTime, u32) {
            let m = bar_meters[abs_bar as usize];
            (starts[abs_bar as usize], crate::grid::meter_ticks(m), crate::grid::meter_cells(m))
        };
        let mut b = Builder {
            tracks: self
                .instruments
                .iter()
                .map(|i| QTrack {
                    name: i.name.clone(),
                    program: i.program,
                    is_drums: i.is_drums,
                    notes: Vec::new(),
                    autos: Vec::new(),
                })
                .collect(),
            open_ties: HashMap::new(),
        };

        // Drum-variant lane inheritance resolves once, in source order
        // (bases are defined before their variants).
        let mut pattern_lanes: HashMap<usize, Vec<(u8, Vec<LaneItem>)>> = HashMap::new();
        for p in &self.patterns {
            if let PatternBody::Drums(d) = &p.body {
                let lanes = merge_variant_lanes(d, &pattern_lanes)?;
                pattern_lanes.insert(p.id, lanes);
            }
        }

        let mut next_bar = 0u32;
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
                                let (bar_start, blen, bcpb) = geometry(next_bar + offset);
                                b.apply_body(
                                    &p.body,
                                    lanes.map(Vec::as_slice),
                                    p.track,
                                    bar_in_body,
                                    bar_start,
                                    p.base_vel,
                                    bcpb,
                                    blen,
                                )?;
                                if bar_in_body == 0 {
                                    b.place_autos(p.track, &p.autos, &self.binds, bar_start)?;
                                }
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
                        let (bar_start, blen, bcpb) = geometry(item.bar - 1 + offset);
                        b.apply_body(
                            &item.body,
                            lanes.as_deref(),
                            item.track,
                            offset,
                            bar_start,
                            item.base_vel,
                            bcpb,
                            blen,
                        )?;
                        if offset == 0 {
                            b.place_autos(item.track, &item.autos, &self.binds, bar_start)?;
                        }
                    }
                }
            }
        }

        for t in &mut b.tracks {
            t.notes.sort_by(|a, x| a.onset.cmp(&x.onset).then(a.pitch.cmp(&x.pitch)));
        }
        let uniform = bar_meters.iter().all(|m| *m == self.header.meter);
        Ok(QSong {
            name: self.header.name.clone(),
            bpm: self.header.bpm,
            meter: self.header.meter,
            bar_meters: if uniform { Vec::new() } else { bar_meters.clone() },
            key: self.header.key,
            swing: self.header.swing,
            n_bars: bar_meters.len() as u32,
            tracks: b.tracks,
        })
    }
}

/// The (cells, ticks) of one bar of a body under its effective meter.
fn body_geometry(header: &Header, meter: Option<(u32, u32)>) -> (u32, MusicalTime) {
    let m = meter.unwrap_or(header.meter);
    (crate::grid::meter_cells(m), crate::grid::meter_ticks(m))
}

/// Meter override values must spell like header meters.
fn validate_meter(m: (u32, u32), who: &str) -> Result<(), Error> {
    if m.1 != 4 && m.1 != 8 {
        return Err(doc_err(format!("{who}: unsupported meter {}/{}", m.0, m.1)));
    }
    if m.0 == 0 || m.0 > 64 {
        return Err(doc_err(format!("{who}: meter numerator {} out of range (1..=64)", m.0)));
    }
    Ok(())
}

/// Target invariants: a CC's controller and an NRPN parameter are in
/// range, and an opaque `Extern` carries a non-empty, emission-safe path
/// (graphic ASCII — no whitespace to break the `bind` line).
fn validate_target(t: &Target) -> Result<(), Error> {
    match t {
        Target::Cc(n) if *n > 127 => Err(doc_err(format!("controller number {n} beyond CC 127"))),
        Target::PolyPressure(n) if *n > 127 => {
            Err(doc_err(format!("poly-aftertouch note {n} beyond MIDI 127")))
        }
        Target::Nrpn(p) | Target::Rpn(p) if *p > 16383 => {
            Err(doc_err(format!("parameter {p} beyond 14-bit 16383")))
        }
        Target::Extern { path, .. } if !valid_extern_path(path) => Err(doc_err(format!(
            "extern target path {path:?} must be non-empty graphic ASCII (no spaces)"
        ))),
        _ => Ok(()),
    }
}

/// An opaque target path must survive the `bind` line verbatim: non-empty
/// and all graphic ASCII (no whitespace, no control characters).
pub(crate) fn valid_extern_path(path: &str) -> bool {
    !path.is_empty() && path.bytes().all(|b| b.is_ascii_graphic())
}

/// Automation lanes on a track: a bound name (resolved with instrument
/// scope), canonical decimal values, canonical easings, and strictly
/// increasing keyframes within the pattern's cell span (`total_cells`).
/// Positions may be sub-cell (any tick in the span), unlike note onsets.
fn validate_autos(
    autos: &[AutoLane],
    binds: &[Bind],
    track: usize,
    total_cells: u32,
    who: &str,
) -> Result<(), Error> {
    let span = MusicalTime::from_sixteenths(total_cells);
    for lane in autos {
        validate_comments(&lane.comments, &format!("{who}: @{}", lane.name))?;
        if Bind::resolve(binds, &lane.name, track).is_none() {
            return Err(doc_err(format!(
                "{who}: @{} is not bound (add `bind {} = ...`)",
                lane.name, lane.name
            )));
        }
        if lane.keys.is_empty() {
            return Err(doc_err(format!("{who}: @{} has no keyframes", lane.name)));
        }
        let mut prev: Option<MusicalTime> = None;
        for k in &lane.keys {
            if !crate::grid::value_is_canonical(k.value) {
                return Err(doc_err(format!(
                    "{who}: @{} value {} is finer than the decimal grid",
                    lane.name, k.value
                )));
            }
            if !k.ease.is_canonical() {
                return Err(doc_err(format!("{who}: @{} carries a malformed easing", lane.name)));
            }
            if k.at < MusicalTime::ZERO || k.at > span {
                return Err(doc_err(format!(
                    "{who}: @{} keyframe at {} is past the {total_cells}-cell pattern",
                    crate::grid::pos_text(k.at),
                    lane.name
                )));
            }
            if let Some(p) = prev
                && k.at <= p
            {
                return Err(doc_err(format!(
                    "{who}: @{} keyframes must strictly increase",
                    lane.name
                )));
            }
            prev = Some(k.at);
        }
    }
    Ok(())
}

/// Parser-facing wrapper: run the global meter pass on a built Document.
pub(crate) fn check_bar_meters(d: &Document) -> Result<(), Error> {
    build_bar_meters(&d.header, &d.patterns, &d.timeline).map(|_| ())
}

/// The global bar → meter assignment: rows claim bars in their stack's
/// (agreed) meter, directs in theirs, unclaimed bars default to the
/// header. Conflicting claims and the renderable tick domain error here
/// — shared by validate() and resolve() so they cannot drift.
fn build_bar_meters(
    header: &Header,
    patterns: &[PatternDef],
    timeline: &[TimelineItem],
) -> Result<Vec<(u32, u32)>, Error> {
    let hm = header.meter;
    let mut map: Vec<Option<(u32, u32)>> = Vec::new();
    fn claim(map: &mut Vec<Option<(u32, u32)>>, bar: u64, m: (u32, u32)) -> Result<(), Error> {
        if bar >= 100_000 {
            return Err(doc_err(format!("bar {} is beyond the 100000-bar limit", bar + 1)));
        }
        let bar = bar as usize;
        if bar >= map.len() {
            map.resize(bar + 1, None);
        }
        match map[bar] {
            Some(prev) if prev != m => Err(doc_err(format!(
                "bar {}: meter {}/{} conflicts with {}/{} claimed earlier",
                bar + 1,
                m.0,
                m.1,
                prev.0,
                prev.1
            ))),
            _ => {
                map[bar] = Some(m);
                Ok(())
            }
        }
    }
    let mut next = 0u64;
    for item in timeline {
        match item {
            TimelineItem::Row(row) => {
                let mut unit = 1u32;
                let mut row_meter: Option<((u32, u32), usize)> = None;
                for id in &row.stack {
                    let p = patterns
                        .iter()
                        .find(|p| p.id == *id)
                        .ok_or_else(|| doc_err(format!("unknown pattern P{id} in a row")))?;
                    unit = unit.max(p.body.n_bars());
                    let m = p.meter.unwrap_or(hm);
                    if let Some((rm, rid)) = row_meter
                        && rm != m
                    {
                        return Err(doc_err(format!(
                            "one bar-stack mixes meters: P{rid} is {}/{} but P{id} is {}/{}",
                            rm.0, rm.1, m.0, m.1
                        )));
                    }
                    row_meter = Some((m, *id));
                }
                let bars = row.reps as u64 * unit as u64;
                match row_meter {
                    // A silent row ([z]) claims no meter — it advances
                    // the timeline and defaults to the header unless a
                    // direct at the same bar says otherwise.
                    None => {
                        if next + bars > 100_000 {
                            return Err(doc_err(format!(
                                "bar {} is beyond the 100000-bar limit",
                                next + bars
                            )));
                        }
                        if (next + bars) as usize > map.len() {
                            map.resize((next + bars) as usize, None);
                        }
                    }
                    Some((m, _)) => {
                        for k in 0..bars {
                            claim(&mut map, next + k, m)?;
                        }
                    }
                }
                next += bars;
            }
            TimelineItem::Direct(d) => {
                let m = d.meter.unwrap_or(hm);
                for k in 0..d.body.n_bars() as u64 {
                    claim(&mut map, d.bar as u64 - 1 + k, m)?;
                }
            }
        }
    }
    let map: Vec<(u32, u32)> = map.into_iter().map(|m| m.unwrap_or(hm)).collect();
    let total: i64 = map.iter().map(|m| crate::grid::meter_ticks(*m).ticks()).sum();
    if total > crate::grid::MAX_SONG_TICKS {
        return Err(doc_err(format!(
            "{} bars exceed the renderable tick domain (2^28 MIDI deltas)",
            map.len()
        )));
    }
    Ok(map)
}

/// Full lanes for a drums body: the variant diff merged over its base's
/// resolved lanes.
fn merge_variant_lanes(
    d: &DrumsBody,
    resolved: &HashMap<usize, Vec<(u8, Vec<LaneItem>)>>,
) -> Result<Vec<(u8, Vec<LaneItem>)>, Error> {
    let Some(base_id) = d.variant_base else {
        return Ok(d.lanes.clone());
    };
    let base = resolved
        .get(&base_id)
        .ok_or_else(|| doc_err(format!("unknown drum variant base P{base_id}")))?;
    let mut merged = base.clone();
    for (pitch, items) in &d.lanes {
        merged.retain(|(p, _)| p != pitch);
        merged.push((*pitch, items.clone()));
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
        if !self.bar_meters.is_empty() {
            if self.bar_meters.len() != self.n_bars as usize {
                return Err(doc_err(format!(
                    "bar_meters carries {} entries for {} bars",
                    self.bar_meters.len(),
                    self.n_bars
                )));
            }
            for (i, m) in self.bar_meters.iter().enumerate() {
                validate_meter(*m, &format!("bar {}", i + 1))?;
            }
        }
        // Bounded given the validated header/meters: bar ticks <= 64 *
        // 960 each and n_bars is u32, so the sum stays far below
        // i64::MAX. It must also fit the renderable tick domain (u28
        // MIDI deltas; midly masks silently past them) or render wraps.
        let end = self.total_ticks();
        if end.ticks() > crate::grid::MAX_SONG_TICKS {
            return Err(doc_err(format!(
                "{} bars exceed the renderable tick domain (2^28 MIDI deltas)",
                self.n_bars
            )));
        }
        // Bar boundaries, for the drum-containment rule below (a lane
        // spelling never crosses a barline).
        let starts = self.bar_starts();
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
                    if !(1..=24).contains(&n.strokes) {
                        return Err(doc_err(format!("{}: stroke count out of range", t.name)));
                    }
                    if n.stroke_mask == 0 || n.stroke_mask >= 1u32 << n.strokes {
                        return Err(doc_err(format!(
                            "{}: stroke mask doesn't fit its {} members",
                            t.name, n.strokes
                        )));
                    }
                    // The lane spellings: a plain hit / digit fills one
                    // cell; a tuplet group spans whole cells and its
                    // ticks must cover its members. Digits (strokes
                    // 2..=4, full mask) may only fill one cell — a
                    // multi-cell full uniform subdivision is a group.
                    match n.dur.try_as_sixteenths() {
                        None => {
                            return Err(doc_err(format!(
                                "{}: drum span off the 16th grid",
                                t.name
                            )));
                        }
                        Some(0) => {
                            return Err(doc_err(format!("{}: zero-width drum span", t.name)));
                        }
                        Some(1) => {}
                        Some(_) if n.strokes >= 2 => {}
                        Some(_) => {
                            return Err(doc_err(format!(
                                "{}: a plain drum hit fills one cell",
                                t.name
                            )));
                        }
                    }
                    if n.dur.ticks() < n.strokes as i64 {
                        return Err(doc_err(format!(
                            "{}: drum span shorter than its {} strokes",
                            t.name, n.strokes
                        )));
                    }
                } else if n.strokes != 1 || n.stroke_mask != 1 {
                    return Err(doc_err(format!(
                        "{}: melodic notes have strokes = 1, mask = 1",
                        t.name
                    )));
                }
                // Total arithmetic (A2): a hostile onset near i64::MAX
                // passes the sign checks and would overflow a bare add —
                // the validator is the one thing that must never trust
                // its input.
                let note_end = match n.onset.ticks().checked_add(n.dur.ticks()) {
                    Some(e) if e <= end.ticks() => MusicalTime(e),
                    _ => {
                        return Err(doc_err(format!(
                            "{}: a note ends past n_bars (emission would drop it)",
                            t.name
                        )));
                    }
                };
                if t.is_drums {
                    // A lane spelling lives within one bar; a crossing
                    // span would silently drop at emission.
                    let bar = starts.partition_point(|s| *s <= n.onset).saturating_sub(1);
                    if note_end > starts[(bar + 1).min(starts.len() - 1)] {
                        return Err(doc_err(format!("{}: a drum span crosses a barline", t.name)));
                    }
                }
            }
            for a in &t.autos {
                validate_target(&a.target)?;
                if !crate::grid::domain_is_canonical(a.domain) {
                    return Err(doc_err(format!("{}: malformed automation domain", t.name)));
                }
                if a.keys.is_empty() {
                    return Err(doc_err(format!("{}: empty automation lane", t.name)));
                }
                let mut prev: Option<MusicalTime> = None;
                for (at, val, ease) in &a.keys {
                    if !crate::grid::value_is_canonical(*val) {
                        return Err(doc_err(format!(
                            "{}: automation value off the decimal grid",
                            t.name
                        )));
                    }
                    if !ease.is_canonical() {
                        return Err(doc_err(format!("{}: malformed automation easing", t.name)));
                    }
                    if *at < MusicalTime::ZERO || *at > end {
                        return Err(doc_err(format!(
                            "{}: automation keyframe outside the song",
                            t.name
                        )));
                    }
                    if let Some(p) = prev
                        && *at <= p
                    {
                        return Err(doc_err(format!(
                            "{}: automation keyframes must strictly increase",
                            t.name
                        )));
                    }
                    prev = Some(*at);
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

/// Comment text that survives its own emission: the emitted line is
/// `// {text}`, whose reparse trims both ends — so stored text must be
/// trim-stable and single-line. Shared with the parser (which stores
/// trimmed text, satisfying this by construction).
pub(crate) fn valid_comment(s: &str) -> bool {
    !s.contains('\n') && s.trim() == s
}

fn validate_comments(comments: &[String], who: &str) -> Result<(), Error> {
    for c in comments {
        if !valid_comment(c) {
            return Err(doc_err(format!(
                "{who}: comment {c:?} would break emission (one line, no surrounding whitespace)"
            )));
        }
    }
    Ok(())
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
            for (pitch, items) in &d.lanes {
                if *pitch > 127 {
                    return Err(doc_err(format!("{who}: lane pitch {pitch} beyond MIDI 127")));
                }
                if !lane_pitches.insert(*pitch) {
                    return Err(doc_err(format!(
                        "{who}: duplicate lane {}",
                        crate::drums::lane_label(*pitch)
                    )));
                }
                let mut width = 0u32;
                for item in items {
                    validate_lane_item(item, &who)?;
                    width = width.saturating_add(item.width());
                }
                if width != cpb {
                    return Err(doc_err(format!(
                        "{who}: lane covers {width} cells, expected {cpb}"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Lane-item shape rules, shared with the parser's construction: cell
/// codes stay in the lane vocabulary; groups have matching arity and
/// member count, stroke-only member codes, at least one sounding
/// member, and a span whose ticks cover the members (the same
/// `span >= n` rule melodic tuplets follow).
fn validate_lane_item(item: &LaneItem, who: &str) -> Result<(), Error> {
    match item {
        LaneItem::Cell(code) => {
            if *code > LANE_D4 {
                return Err(doc_err(format!("{who}: bad lane cell code")));
            }
        }
        LaneItem::Group { n, members, span } => {
            if !(2..=24).contains(n) || members.len() != *n as usize {
                return Err(doc_err(format!("{who}: malformed lane tuplet group")));
            }
            if members.iter().any(|m| *m > LANE_ACCENT) {
                return Err(doc_err(format!(
                    "{who}: lane group members are strokes only (. o x X)"
                )));
            }
            if members.iter().all(|m| *m == LANE_EMPTY) {
                return Err(doc_err(format!(
                    "{who}: a lane group needs at least one sounding member (write dots instead)"
                )));
            }
            let span_ticks = MusicalTime::from_sixteenths(*span as u32);
            if *span == 0 || span_ticks.ticks() < *n as i64 {
                return Err(doc_err(format!(
                    "{who}: lane group span is shorter than its {n} members"
                )));
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
    /// Place one bar of a body opening at `bar_start` (`cpb`/`bar_len`
    /// are that bar's geometry — meters can change per bar). For drums,
    /// `drum_lanes` carries the variant-resolved lanes.
    #[allow(clippy::too_many_arguments)]
    fn apply_body(
        &mut self,
        body: &PatternBody,
        drum_lanes: Option<&[(u8, Vec<LaneItem>)]>,
        track: usize,
        bar_in_body: u32,
        bar_start: MusicalTime,
        base: u8,
        cpb: u32,
        bar_len: MusicalTime,
    ) -> Result<(), Error> {
        if track >= self.tracks.len() {
            return Err(doc_err(format!("track index {track} out of range")));
        }
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
                for (pitch, items) in lanes {
                    let width: u32 = items.iter().map(|i| i.width()).sum();
                    if width != cpb {
                        return Err(doc_err(format!("lane covers {width} cells, expected {cpb}")));
                    }
                    let mut cell = 0u32;
                    for item in items {
                        let onset = bar_start + MusicalTime::from_sixteenths(cell);
                        match item {
                            LaneItem::Cell(code) => {
                                let (vel, strokes) = match *code {
                                    LANE_ACCENT => (notation::apply_mark(base, Mark::Accent), 1),
                                    LANE_GHOST => (notation::apply_mark(base, Mark::Ghost), 1),
                                    LANE_HIT => (base, 1),
                                    LANE_D2 => (base, 2),
                                    LANE_D3 => (base, 3),
                                    LANE_D4 => (base, 4),
                                    _ => {
                                        cell += 1;
                                        continue;
                                    }
                                };
                                self.tracks[track].notes.push(QNote {
                                    pitch: *pitch,
                                    onset,
                                    dur: MusicalTime::from_sixteenths(1),
                                    strokes,
                                    stroke_mask: crate::grid::full_stroke_mask(strokes),
                                    vel,
                                });
                            }
                            LaneItem::Group { n, members, span } => {
                                // One QNote per velocity class, masks
                                // partitioning the sounding members —
                                // emission merges them back into one
                                // group with per-member marks.
                                let mut masks: [(u8, u32); 3] = [
                                    (base, 0),
                                    (notation::apply_mark(base, Mark::Accent), 0),
                                    (notation::apply_mark(base, Mark::Ghost), 0),
                                ];
                                for (i, code) in members.iter().enumerate() {
                                    let class = match *code {
                                        LANE_HIT => 0,
                                        LANE_ACCENT => 1,
                                        LANE_GHOST => 2,
                                        _ => continue,
                                    };
                                    masks[class].1 |= 1 << i;
                                }
                                for (vel, mask) in masks {
                                    if mask == 0 {
                                        continue;
                                    }
                                    self.tracks[track].notes.push(QNote {
                                        pitch: *pitch,
                                        onset,
                                        dur: MusicalTime::from_sixteenths(*span as u32),
                                        strokes: *n,
                                        stroke_mask: mask,
                                        vel,
                                    });
                                }
                            }
                        }
                        cell += item.width();
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
                    stroke_mask: 1,
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
                    self.tracks[track].notes.push(QNote {
                        pitch,
                        onset,
                        dur,
                        strokes: 1,
                        stroke_mask: 1,
                        vel,
                    });
                    self.tracks[track].notes.len() - 1
                }
            };
            if tie {
                self.open_ties.entry(key).or_default().push((idx, onset + dur));
            }
        }
    }

    /// Place a pattern's automation lanes into the track at `bar_start`,
    /// offset to absolute time. Each `@name` resolves through the song-level
    /// binds; an unbound name errors (validate catches parsed input first).
    fn place_autos(
        &mut self,
        track: usize,
        autos: &[AutoLane],
        binds: &[Bind],
        bar_start: MusicalTime,
    ) -> Result<(), Error> {
        for lane in autos {
            let bind = Bind::resolve(binds, &lane.name, track)
                .ok_or_else(|| doc_err(format!("automation @{} is not bound", lane.name)))?;
            let keys = lane.keys.iter().map(|k| (bar_start + k.at, k.value, k.ease)).collect();
            self.tracks[track].autos.push(QAuto {
                target: bind.target.clone(),
                domain: bind.domain,
                keys,
            });
        }
        Ok(())
    }
}
