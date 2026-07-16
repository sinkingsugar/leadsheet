//! Layer 4 — QSong → text: patterns + arrangement.
//!
//! ```text
//! song: demo  tempo: 120.00  meter: 4/4  key: Am  grid: 1/16
//! instruments: bass:33 drums:kit piano:0 lead:81
//!
//! P1 bass  | A,,4 A,,4 G,,4 E,,4 |
//! P2 drums
//!   K |x... x... x... x...|
//!   S |.... x... .... x...|
//! P3 piano* | Am . F G7 |
//! P4 lead  | e2 c2 d2 B2 c4 A4 |
//!
//! arrangement:
//!   [P1+P2] x4
//!   [P1+P2+P3+P4] x8
//! ```
//!
//! Per (instrument, bar), the body takes the most semantic form that stays
//! lossless: drum tracks become step-grid lanes; harmonic bars whose
//! voicings are canonical become chord symbols (`*` marks chord mode,
//! 1 column = 1 beat, `.` holds); everything else is melodic ABC-style
//! tokens with explicit accidentals, spelled for the detected key.
//! Identical (instrument, bar) contents share one pattern; the arrangement
//! lists bar-stacks with run-length encoding (see [`crate::pattern`]).

use crate::chord;
use crate::doc::{
    AutoLane, Bind, BindScope, ChordCol, DirectItem, Document, DrumsBody, Header, Instrument,
    LaneItem, MelodicBar, PatternBody, PatternDef, Row, TimelineItem,
};
use crate::drums::{
    self, LANE_ACCENT, LANE_D2, LANE_D3, LANE_D4, LANE_EMPTY, LANE_GHOST, LANE_HIT,
};
use crate::grid::{Ease, MusicalTime, QSong, QTrack, TICKS_PER_BEAT, Target};
use crate::notation::{self, Tok, emit_token_spelled};
use crate::pattern;
use std::collections::BTreeMap;
use std::fmt::Write;

// ---------------------------------------------------------------------------
// Canonical spelling — shared by compressor keys and Document emission.

/// Spell one voice: canonical tuplet grouping (idempotent — already-grouped
/// tokens pass through), then token spelling.
pub(crate) fn spell_voice(toks: &[Tok], flats: bool) -> String {
    notation::detect_tuplets(toks.to_vec())
        .iter()
        .map(|t| emit_token_spelled(t, flats))
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn spell_melodic_bar(bar: &MelodicBar, flats: bool) -> String {
    bar.voices.iter().map(|v| spell_voice(v, flats)).collect::<Vec<_>>().join(" & ")
}

pub(crate) fn spell_chordal_bar(cols: &[ChordCol], flats: bool) -> String {
    cols.iter()
        .map(|c| match c {
            ChordCol::Sym(sym) => chord::symbol_to_string(sym, flats),
            ChordCol::Hold => ".".to_string(),
            ChordCol::Rest => "z".to_string(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A note fragment clipped to one bar.
struct Seg {
    onset: MusicalTime, // bar-relative
    dur: MusicalTime,
    /// Drum stroke shape (1 = plain hit); always 1/mask 1 for melodic
    /// segments. See [`crate::grid::QNote`].
    strokes: u8,
    stroke_mask: u32,
    pitch: u8,
    vel: u8,
    tie_in: bool,
    tie_out: bool,
    /// Index of the source note in its track (ties a note's segments
    /// together across bars for velocity reconstruction).
    note: usize,
}

/// A bar's base dynamic: the bucketed median over mark *groups* — one vote
/// per melodic token (a chord counts once, at its group median) or per drum
/// hit. Voting per group rather than per note makes emission a fixpoint:
/// marks are per token, so re-deriving the base from the reconstructed
/// (base ± delta) velocities lands on the same bucket, byte for byte.
fn base_velocity(segs: &[Seg], is_drums: bool) -> u8 {
    let mut votes: Vec<u8> = if is_drums {
        segs.iter().map(|s| s.vel).collect()
    } else {
        let mut groups: BTreeMap<(MusicalTime, MusicalTime, bool), Vec<u8>> = BTreeMap::new();
        for s in segs {
            groups.entry((s.onset, s.dur, s.tie_out)).or_default().push(s.vel);
        }
        groups
            .into_values()
            .map(|mut v| {
                v.sort_unstable();
                v[(v.len() - 1) / 2]
            })
            .collect()
    };
    votes.sort_unstable();
    notation::vel_to_dynamic(votes[(votes.len() - 1) / 2]).1
}

/// The bar containing `pos`: last start <= pos (`starts` has n_bars + 1
/// entries; see [`QSong::bar_starts`]).
fn bar_of(starts: &[MusicalTime], pos: MusicalTime) -> usize {
    starts.partition_point(|s| *s <= pos).saturating_sub(1).min(starts.len().saturating_sub(2))
}

/// Split a track's notes at bar boundaries: per-bar segment lists.
/// Drum notes never span bars (validated); bars may differ in length
/// when the song changes meter.
fn split_bars(track: &QTrack, starts: &[MusicalTime]) -> Vec<Vec<Seg>> {
    let n_bars = starts.len().saturating_sub(1);
    let mut bars: Vec<Vec<Seg>> = (0..n_bars).map(|_| Vec::new()).collect();
    if n_bars == 0 {
        return bars;
    }
    for (note, n) in track.notes.iter().enumerate() {
        if track.is_drums {
            let bar = bar_of(starts, n.onset);
            if n.onset < starts[n_bars]
                && let Some(slot) = bars.get_mut(bar)
            {
                slot.push(Seg {
                    onset: n.onset - starts[bar],
                    dur: n.dur,
                    strokes: n.strokes,
                    stroke_mask: n.stroke_mask,
                    pitch: n.pitch,
                    vel: n.vel,
                    tie_in: false,
                    tie_out: false,
                    note,
                });
            }
            continue;
        }
        let end = n.onset + n.dur;
        let mut pos = n.onset;
        while pos < end && pos < starts[n_bars] {
            let bar = bar_of(starts, pos);
            let seg_end = end.min(starts[bar + 1]);
            bars[bar].push(Seg {
                onset: pos - starts[bar],
                dur: seg_end - pos,
                strokes: 1,
                stroke_mask: 1,
                pitch: n.pitch,
                vel: n.vel,
                tie_in: pos > n.onset,
                tie_out: seg_end < end,
                note,
            });
            pos = seg_end;
        }
    }
    bars
}

/// After a bar's base is fixed, record the velocity parse will reconstruct
/// for each note that *starts* in it: base ± the group's mark delta. Later
/// bars' tie-in segments vote and mark with this value, so every generation
/// derives identical dynamics (marks are grouped exactly like
/// [`bar_voices`] groups tokens).
fn record_reconstructed_vels(segs: &[Seg], base: u8, recon: &mut [Option<u8>]) {
    let mut groups: BTreeMap<(MusicalTime, MusicalTime, bool), Vec<&Seg>> = BTreeMap::new();
    for s in segs {
        groups.entry((s.onset, s.dur, s.tie_out)).or_default().push(s);
    }
    for group in groups.values() {
        let mut vels: Vec<u8> = group.iter().map(|s| s.vel).collect();
        vels.sort_unstable();
        let mark = notation::mark_for_vel(vels[(vels.len() - 1) / 2], base);
        let v = notation::apply_mark(base, mark);
        for s in group {
            if !s.tie_in {
                recon[s.note] = Some(v);
            }
        }
    }
}

/// Render one bar's segments as melodic voice strings (usually one voice).
/// `base` is the bar's dynamic bucket; deviating notes get `>` / `~` marks.
fn bar_voices(segs: &[Seg], bar_len: MusicalTime, base: u8) -> Vec<Vec<Tok>> {
    // Segments sharing (onset, duration, tie) stack into one chord token.
    let mut groups: BTreeMap<(MusicalTime, MusicalTime, bool), Vec<(u8, u8)>> = BTreeMap::new();
    for s in segs {
        groups.entry((s.onset, s.dur, s.tie_out)).or_default().push((s.pitch, s.vel));
    }
    // Greedy voice allocation: each group goes to the first voice that has
    // already ended when the group starts. Token durations are spelled in
    // the text unit (16th cells).
    struct Voice {
        end: MusicalTime,
        toks: Vec<Tok>,
    }
    let mut voices: Vec<Voice> = Vec::new();
    for ((onset, dur, tie), mut notes) in groups {
        notes.sort_unstable();
        let mut vels: Vec<u8> = notes.iter().map(|(_, v)| *v).collect();
        vels.sort_unstable();
        let mark = notation::mark_for_vel(vels[(vels.len() - 1) / 2], base);
        let pitches: Vec<u8> = notes.iter().map(|(p, _)| *p).collect();
        let tok = if pitches.len() == 1 {
            Tok::Note { pitch: pitches[0], dur, tie, mark }
        } else {
            Tok::Chord { pitches, dur, tie, mark }
        };
        let voice = match voices.iter_mut().find(|v| v.end <= onset) {
            Some(v) => v,
            None => {
                voices.push(Voice { end: MusicalTime::ZERO, toks: Vec::new() });
                voices.last_mut().unwrap()
            }
        };
        if onset > voice.end {
            voice.toks.push(Tok::Rest { dur: onset - voice.end });
        }
        voice.toks.push(tok);
        voice.end = onset + dur;
    }
    voices
        .into_iter()
        .map(|mut v| {
            if v.end < bar_len {
                v.toks.push(Tok::Rest { dur: bar_len - v.end });
            }
            v.toks
        })
        .collect()
}

/// Chord-mode body (`Am . F G7`) if — and only if — every onset group in
/// the bar is a beat-aligned, uniformly-held, canonically-voiced chord.
fn try_chordal(segs: &[Seg], bar_len: MusicalTime) -> Option<Vec<ChordCol>> {
    if segs.is_empty() || segs.iter().any(|s| s.tie_in || s.tie_out) {
        return None;
    }
    let beat_len = MusicalTime(TICKS_PER_BEAT);
    let mut groups: BTreeMap<MusicalTime, Vec<&Seg>> = BTreeMap::new();
    for s in segs {
        groups.entry(s.onset).or_default().push(s);
    }
    let onsets: Vec<MusicalTime> = groups.keys().copied().collect();
    let beats = (bar_len / beat_len) as usize;
    let mut columns: Vec<Option<crate::chord::ChordSym>> = vec![None; beats]; // None = rest/hold
    let mut covered = vec![false; beats];
    for (i, (&onset, group)) in groups.iter().enumerate() {
        if onset % beat_len != MusicalTime::ZERO {
            return None;
        }
        let dur = group[0].dur;
        if dur % beat_len != MusicalTime::ZERO || group.iter().any(|s| s.dur != dur) {
            return None;
        }
        let limit = onsets.get(i + 1).copied().unwrap_or(bar_len);
        if onset + dur > limit {
            return None; // overlaps the next chord (or the bar line)
        }
        let mut pitches: Vec<u8> = group.iter().map(|s| s.pitch).collect();
        pitches.sort_unstable();
        let sym = chord::detect(&pitches)?;
        let beat = (onset / beat_len) as usize;
        columns[beat] = Some(sym);
        covered[beat..beat + (dur / beat_len) as usize].fill(true);
    }
    Some(
        columns
            .iter()
            .enumerate()
            .map(|(b, c)| match c {
                Some(sym) => ChordCol::Sym(*sym),
                None if covered[b] => ChordCol::Hold,
                None => ChordCol::Rest,
            })
            .collect(),
    )
}

type Lanes = BTreeMap<u8, Vec<LaneItem>>;

pub(crate) fn lane_char(code: u8) -> char {
    match code {
        LANE_GHOST => 'o',
        LANE_HIT => 'x',
        LANE_ACCENT => 'X',
        LANE_D2 => '2',
        LANE_D3 => '3',
        LANE_D4 => '4',
        _ => '.',
    }
}

/// One lane item's canonical spelling (no beat spacing).
pub(crate) fn lane_item_text(item: &LaneItem) -> String {
    match item {
        LaneItem::Cell(code) => lane_char(*code).to_string(),
        LaneItem::Group { n, members, span } => {
            let strokes: String = members.iter().map(|m| lane_char(*m)).collect();
            format!("({n}:{span}{strokes})")
        }
    }
}

/// A whole lane's canonical spelling, unspaced — dedup keys and diff.
pub(crate) fn lane_text(items: &[LaneItem]) -> String {
    items.iter().map(lane_item_text).collect()
}

/// The lane code a velocity earns relative to the bar's base bucket.
fn stroke_code(vel: u8, base: u8) -> u8 {
    match notation::mark_for_vel(vel, base) {
        notation::Mark::Accent => LANE_ACCENT,
        notation::Mark::Ghost => LANE_GHOST,
        notation::Mark::None => LANE_HIT,
    }
}

/// Drum step-grid: one lane per GM voice. Same-placement segments (one
/// group's velocity classes) merge back into one item; digits stay
/// digits, single-cell full-mask groups canonicalize to digits, and
/// everything else spells as a `(n strokes)span` group.
fn drum_lane_map(segs: &[Seg], cells_per_bar: u32, base: u8) -> Lanes {
    // Placement key: (start cell, span cells, member count). Drum onsets
    // and spans are on the 16th grid by construction (parse lanes and
    // quantize both emit cell-aligned values); QSong::validate() rejects
    // off-grid host-built hits before they can panic here.
    let mut placed: BTreeMap<(u8, u32, u32, u8), Vec<u8>> = BTreeMap::new();
    for s in segs {
        let start = s.onset.as_sixteenths_exact();
        let span = s.dur.as_sixteenths_exact();
        let members = placed
            .entry((s.pitch, start, span, s.strokes))
            .or_insert_with(|| vec![LANE_EMPTY; s.strokes as usize]);
        let code = stroke_code(s.vel, base);
        for i in 0..s.strokes {
            if s.stroke_mask & (1 << i) != 0 {
                members[i as usize] = code;
            }
        }
    }
    let mut lanes: Lanes = BTreeMap::new();
    let mut cursor: BTreeMap<u8, u32> = BTreeMap::new();
    for ((pitch, start, span, n), members) in placed {
        let lane = lanes.entry(pitch).or_default();
        let pos = cursor.entry(pitch).or_insert(0);
        if start < *pos || start + span > cells_per_bar {
            // Overlapping or bar-crossing placements are unrepresentable
            // in one lane; validated resolver output never produces them.
            continue;
        }
        while *pos < start {
            lane.push(LaneItem::Cell(LANE_EMPTY));
            *pos += 1;
        }
        let uniform_base = members.iter().all(|m| *m == LANE_HIT);
        let item = if n == 1 {
            LaneItem::Cell(members[0])
        } else if span == 1 && uniform_base && (2..=4).contains(&n) {
            // The digit spellings own their shapes: `2`/`3`/`4`.
            LaneItem::Cell(match n {
                2 => LANE_D2,
                3 => LANE_D3,
                _ => LANE_D4,
            })
        } else {
            LaneItem::Group { n, members, span: span as u8 }
        };
        lane.push(item);
        *pos += span;
    }
    for (pitch, lane) in &mut lanes {
        let pos = cursor.get(pitch).copied().unwrap_or(0);
        for _ in pos..cells_per_bar {
            lane.push(LaneItem::Cell(LANE_EMPTY));
        }
    }
    lanes
}

/// Render lanes as tab lines, cells grouped by beat where item edges
/// allow it.
fn render_lanes(entries: &[(u8, Vec<LaneItem>)]) -> String {
    let label_w = entries.iter().map(|(p, _)| drums::lane_label(*p).len()).max().unwrap_or(1);
    entries
        .iter()
        .map(|(pitch, items)| {
            let mut grid = String::new();
            let mut pos = 0u32;
            for item in items {
                if pos > 0 && pos.is_multiple_of(4) {
                    grid.push(' ');
                }
                grid.push_str(&lane_item_text(item));
                pos += item.width();
            }
            format!("  {:<label_w$} |{grid}|", drums::lane_label(*pitch))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lanes_sorted(lanes: &Lanes) -> Vec<(u8, Vec<LaneItem>)> {
    let mut entries: Vec<(u8, Vec<LaneItem>)> =
        lanes.iter().map(|(p, c)| (*p, c.clone())).collect();
    entries.sort_by_key(|(p, _)| drums::lane_order(*p));
    entries
}

/// One bar's emitted form. `base` is the pattern's dynamic bucket; `text`
/// is the canonical spelling (dedup key + kinship similarity input).
enum Body {
    Melodic { base: u8, meter: (u32, u32), voices: Vec<Vec<Tok>>, text: String },
    Chordal { base: u8, meter: (u32, u32), cols: Vec<ChordCol>, text: String },
    Drums { base: u8, meter: (u32, u32), lanes: Lanes },
}

impl Body {
    /// Dedup key: kind tag + dynamic + meter + content (a chordal body
    /// must never collide with an identical-looking melodic one, nor `f`
    /// with `p`, nor a 3/4 bar with a 6/8 one that spells the same).
    fn key(&self) -> String {
        match self {
            Body::Melodic { base, meter, text, .. } => {
                format!("m{base}:{}/{}:{text}", meter.0, meter.1)
            }
            Body::Chordal { base, meter, text, .. } => {
                format!("c{base}:{}/{}:{text}", meter.0, meter.1)
            }
            Body::Drums { base, meter, lanes } => {
                let mut s = format!("d{base}:{}/{}:", meter.0, meter.1);
                for (pitch, items) in lanes {
                    s.push_str(&pitch.to_string());
                    s.push('=');
                    s.push_str(&lane_text(items));
                    s.push(';');
                }
                s
            }
        }
    }

    fn base(&self) -> u8 {
        match self {
            Body::Melodic { base, .. } | Body::Chordal { base, .. } | Body::Drums { base, .. } => {
                *base
            }
        }
    }

    fn meter(&self) -> (u32, u32) {
        match self {
            Body::Melodic { meter, .. }
            | Body::Chordal { meter, .. }
            | Body::Drums { meter, .. } => *meter,
        }
    }
}

/// `@dyn` suffix for a pattern's name field; empty at the default dynamic.
fn dyn_suffix(base: u8) -> String {
    if base == notation::DEFAULT_VEL {
        String::new()
    } else {
        format!("@{}", notation::vel_to_dynamic(base).0)
    }
}

/// Token-multiset overlap in [0, 1] for melodic/chordal kinship.
fn body_similarity(a: &str, b: &str) -> f64 {
    let ta: Vec<&str> = a.split_whitespace().collect();
    let tb: Vec<&str> = b.split_whitespace().collect();
    if ta.is_empty() && tb.is_empty() {
        return 1.0;
    }
    let mut counts: std::collections::HashMap<&str, i32> = Default::default();
    for t in &ta {
        *counts.entry(t).or_default() += 1;
    }
    let mut common = 0usize;
    for t in &tb {
        let c = counts.entry(t).or_default();
        if *c > 0 {
            *c -= 1;
            common += 1;
        }
    }
    2.0 * common as f64 / (ta.len() + tb.len()) as f64
}

const KINSHIP_THRESHOLD: f64 = 0.6;

/// How each pattern gets written: possibly as a variant of an earlier one.
enum PatternForm {
    Full {
        kin: Option<usize>,
    },
    /// Drums only: base pattern index + the lanes that differ from it
    /// (a lane cleared relative to the base appears as all dots).
    DrumsDiff {
        base: usize,
        lanes: Vec<(u8, Vec<LaneItem>)>,
    },
}

fn instrument_field(t: &Instrument) -> String {
    if t.is_drums { format!("{}:kit", t.name) } else { format!("{}:{}", t.name, t.program) }
}

/// Invent structure for a compiled song: bar splitting, dedup into
/// patterns, drum-variant diffs, melodic kinship, RLE arrangement rows
/// with self-similarity section labels — the compressor's Document.
pub fn from_qsong(q: &QSong) -> Document {
    let flats = q.key.map(|k| k.use_flats()).unwrap_or(false);
    let starts = q.bar_starts();
    let bodies: Vec<Vec<Option<Body>>> = q
        .tracks
        .iter()
        .map(|t| {
            // Reconstructed velocity per note (see record_reconstructed_vels).
            let mut recon: Vec<Option<u8>> = vec![None; t.notes.len()];
            split_bars(t, &starts)
                .into_iter()
                .enumerate()
                .map(|(bar, mut segs)| {
                    if segs.is_empty() {
                        return None;
                    }
                    let meter = q.bar_meter(bar as u32);
                    let bar_len = starts[bar + 1] - starts[bar];
                    let cpb = crate::grid::meter_cells(meter);
                    // Tie-in segments vote and mark with the velocity parse
                    // will assign them (fixed where the note started).
                    for s in &mut segs {
                        if s.tie_in
                            && let Some(v) = recon[s.note]
                        {
                            s.vel = v;
                        }
                    }
                    let base = base_velocity(&segs, t.is_drums);
                    if !t.is_drums {
                        record_reconstructed_vels(&segs, base, &mut recon);
                    }
                    Some(if t.is_drums {
                        Body::Drums { base, meter, lanes: drum_lane_map(&segs, cpb, base) }
                    } else if let Some(cols) =
                        // Chord columns are quarter-note beats; only /4 meters.
                        (meter.1 == 4).then(|| try_chordal(&segs, bar_len)).flatten()
                    {
                        let text = spell_chordal_bar(&cols, flats);
                        Body::Chordal { base, meter, cols, text }
                    } else {
                        let voices = bar_voices(&segs, bar_len, base);
                        let text = voices
                            .iter()
                            .map(|v| spell_voice(v, flats))
                            .collect::<Vec<_>>()
                            .join(" & ");
                        Body::Melodic { base, meter, voices, text }
                    })
                })
                .collect()
        })
        .collect();

    let keys: Vec<Vec<Option<String>>> = bodies
        .iter()
        .map(|track| track.iter().map(|b| b.as_ref().map(Body::key)).collect())
        .collect();
    let set = pattern::build(&keys);

    // Resolve each pattern back to its Body (first occurrence of its key).
    let pattern_bodies: Vec<&Body> = set
        .patterns
        .iter()
        .map(|p| {
            let bar = keys[p.track]
                .iter()
                .position(|k| k.as_deref() == Some(p.body.as_str()))
                .expect("pattern came from these bodies");
            bodies[p.track][bar].as_ref().unwrap()
        })
        .collect();

    // Variant planning: best earlier same-track same-kind pattern.
    let forms: Vec<PatternForm> = (0..set.patterns.len())
        .map(|i| match pattern_bodies[i] {
            Body::Drums { lanes: lanes_i, meter: meter_i, .. } => {
                let mut best: Option<(usize, usize)> = None; // (cost, base)
                for (j, body_j) in pattern_bodies.iter().enumerate().take(i) {
                    if set.patterns[j].track != set.patterns[i].track {
                        continue;
                    }
                    let Body::Drums { lanes: lanes_j, meter: meter_j, .. } = body_j else {
                        continue;
                    };
                    // A variant inherits its base's lanes verbatim, so
                    // the meters must match.
                    if meter_j != meter_i {
                        continue;
                    }
                    let pitches: std::collections::BTreeSet<u8> =
                        lanes_i.keys().chain(lanes_j.keys()).copied().collect();
                    let cost = pitches.iter().filter(|p| lanes_i.get(p) != lanes_j.get(p)).count();
                    if best.is_none_or(|(c, _)| cost < c) {
                        best = Some((cost, j));
                    }
                }
                match best {
                    Some((cost, base)) if cost < lanes_i.len() => {
                        let Body::Drums { lanes: base_lanes, .. } = pattern_bodies[base] else {
                            unreachable!()
                        };
                        let cpb = crate::grid::meter_cells(pattern_bodies[i].meter()) as usize;
                        let mut diff: Vec<(u8, Vec<LaneItem>)> = Vec::new();
                        let pitches: std::collections::BTreeSet<u8> =
                            lanes_i.keys().chain(base_lanes.keys()).copied().collect();
                        for pitch in pitches {
                            if lanes_i.get(&pitch) != base_lanes.get(&pitch) {
                                let cells = lanes_i
                                    .get(&pitch)
                                    .cloned()
                                    .unwrap_or_else(|| vec![LaneItem::Cell(LANE_EMPTY); cpb]); // cleared lane
                                diff.push((pitch, cells));
                            }
                        }
                        diff.sort_by_key(|(p, _)| drums::lane_order(*p));
                        PatternForm::DrumsDiff { base, lanes: diff }
                    }
                    _ => PatternForm::Full { kin: None },
                }
            }
            Body::Melodic { text: body_i, .. } | Body::Chordal { text: body_i, .. } => {
                let chordal_i = matches!(pattern_bodies[i], Body::Chordal { .. });
                let mut kin: Option<(f64, usize)> = None;
                for (j, body_j) in pattern_bodies.iter().enumerate().take(i) {
                    if set.patterns[j].track != set.patterns[i].track {
                        continue;
                    }
                    let body_j = match body_j {
                        Body::Melodic { text, .. } if !chordal_i => text,
                        Body::Chordal { text, .. } if chordal_i => text,
                        _ => continue,
                    };
                    let sim = body_similarity(body_i, body_j);
                    if sim >= KINSHIP_THRESHOLD && kin.is_none_or(|(best, _)| sim > best) {
                        kin = Some((sim, j));
                    }
                }
                PatternForm::Full { kin: kin.map(|(_, j)| j) }
            }
        })
        .collect();

    let labels = section_labels(&set, &forms);
    let patterns: Vec<PatternDef> = (0..set.patterns.len())
        .map(|i| {
            let p = &set.patterns[i];
            let (kin, body) = match (&forms[i], pattern_bodies[i]) {
                (PatternForm::DrumsDiff { base, lanes }, _) => (
                    None,
                    PatternBody::Drums(DrumsBody {
                        variant_base: Some(set.patterns[*base].id),
                        lanes: lanes.clone(),
                    }),
                ),
                (PatternForm::Full { .. }, Body::Drums { lanes, .. }) => (
                    None,
                    PatternBody::Drums(DrumsBody {
                        variant_base: None,
                        lanes: lanes_sorted(lanes),
                    }),
                ),
                (PatternForm::Full { kin }, Body::Melodic { voices, .. }) => (
                    kin.map(|j| set.patterns[j].id),
                    PatternBody::Melodic(vec![MelodicBar { voices: voices.clone() }]),
                ),
                (PatternForm::Full { kin }, Body::Chordal { cols, .. }) => {
                    (kin.map(|j| set.patterns[j].id), PatternBody::Chordal(vec![cols.clone()]))
                }
            };
            PatternDef {
                id: p.id,
                track: p.track,
                base_vel: pattern_bodies[i].base(),
                meter: Some(pattern_bodies[i].meter()).filter(|m| *m != q.meter),
                kin,
                body,
                autos: Vec::new(),
            }
        })
        .collect();
    let timeline: Vec<TimelineItem> = set
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            TimelineItem::Row(Row {
                label: labels.get(&i).cloned(),
                stack: row.stack.clone(),
                reps: row.reps,
            })
        })
        .collect();
    Document {
        header: Header {
            name: q.name.clone(),
            // Source BPM is hundredth-canonical (B1); measured tempos
            // (ingest) quantize here, at the boundary into source.
            bpm: format!("{:.2}", q.bpm).parse().expect("a {:.2} spelling always parses"),
            meter: q.meter,
            key: q.key,
            swing: q.swing,
        },
        instruments: q
            .tracks
            .iter()
            .map(|t| Instrument {
                name: t.name.clone(),
                // The text has no slot for a kit program (`drums:kit`);
                // a measured GM2 kit select quantizes away here, at the
                // boundary into source (A1, same shape as BPM).
                program: if t.is_drums { 0 } else { t.program },
                is_drums: t.is_drums,
            })
            .collect(),
        binds: Vec::new(),
        patterns,
        timeline,
    }
}

/// Render a Document as canonical text. The exact inverse of
/// [`crate::parse::parse_document`] on canonical input, and the layout
/// engine for both compressor output and `leadsheet fmt`
/// (Document-canonical: author structure survives).
///
/// **Precondition:** the input must satisfy [`Document::validate`].
/// Emission of an invalid Document may not reparse (`piano:255`,
/// `tempo: NaN`) or may silently drift (`base_vel: 70` re-reads as
/// `@mp` = 64) — the canonicality theorem only speaks for validated
/// inputs. Parser output is always valid; host-built Documents must
/// call `validate()` first.
pub fn emit_document(d: &Document) -> String {
    let flats = d.header.key.map(|k| k.use_flats()).unwrap_or(false);
    let mut out = String::new();
    let _ = write!(
        out,
        "song: {}  tempo: {:.2}  meter: {}/{}",
        d.header.name, d.header.bpm, d.header.meter.0, d.header.meter.1
    );
    if let Some(key) = d.header.key {
        let _ = write!(out, "  key: {}", key.name());
    }
    if let Some(sw) = d.header.swing {
        match sw.level {
            16 => {
                let _ = write!(out, "  swing: 16th {}%", sw.percent);
            }
            _ => {
                let _ = write!(out, "  swing: {}%", sw.percent);
            }
        }
    }
    out.push_str("  grid: 1/16\n");
    if d.instruments.is_empty() {
        out.push_str("instruments:\n");
    } else {
        let _ = writeln!(
            out,
            "instruments: {}",
            d.instruments.iter().map(instrument_field).collect::<Vec<_>>().join(" ")
        );
    }
    if !d.binds.is_empty() {
        // Sorted by their spelled key so emission is order-independent
        // (canonical); an instrument scope prefixes `inst.`.
        let bind_key = |b: &Bind| match b.scope {
            BindScope::Song => b.name.clone(),
            BindScope::Instrument(i) => format!("{}.{}", d.instruments[i].name, b.name),
        };
        let mut binds: Vec<&Bind> = d.binds.iter().collect();
        binds.sort_by_key(|b| bind_key(b));
        for b in binds {
            let _ = writeln!(
                out,
                "bind {} = {}{}",
                bind_key(b),
                target_text(&b.target),
                domain_text(b.domain)
            );
        }
    }
    out.push('\n');

    let name_field = |p: &PatternDef| -> String {
        let name = &d.instruments[p.track].name;
        let star = if matches!(p.body, PatternBody::Chordal(_)) { "*" } else { "" };
        let dynamic = dyn_suffix(p.base_vel);
        let meter = meter_suffix(p.meter.filter(|m| *m != d.header.meter));
        match p.kin {
            Some(k) => format!("{name}{star}{dynamic}{meter} ~P{k}"),
            None => format!("{name}{star}{dynamic}{meter}"),
        }
    };
    if !d.patterns.is_empty() {
        let id_w = d.patterns.iter().map(|p| p.id).max().unwrap_or(1).to_string().len();
        let name_w = d
            .patterns
            .iter()
            .filter(|p| !matches!(p.body, PatternBody::Drums(_)))
            .map(|p| name_field(p).len())
            .max()
            .unwrap_or(0);
        for p in &d.patterns {
            match &p.body {
                PatternBody::Drums(db) => match db.variant_base {
                    Some(base) => {
                        let _ = writeln!(
                            out,
                            "P{:<id_w$} {}{}{} ~P{base}",
                            p.id,
                            d.instruments[p.track].name,
                            dyn_suffix(p.base_vel),
                            meter_suffix(p.meter.filter(|m| *m != d.header.meter))
                        );
                        if !db.lanes.is_empty() {
                            let _ = writeln!(out, "{}", render_lanes(&db.lanes));
                        }
                    }
                    None => {
                        let _ = writeln!(
                            out,
                            "P{:<id_w$} {}{}{}",
                            p.id,
                            d.instruments[p.track].name,
                            dyn_suffix(p.base_vel),
                            meter_suffix(p.meter.filter(|m| *m != d.header.meter))
                        );
                        let _ = writeln!(out, "{}", render_lanes(&db.lanes));
                    }
                },
                PatternBody::Melodic(bars) => {
                    let text = bars
                        .iter()
                        .map(|b| spell_melodic_bar(b, flats))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let _ = writeln!(out, "P{:<id_w$} {:<name_w$} | {text} |", p.id, name_field(p));
                }
                PatternBody::Chordal(bars) => {
                    let text = bars
                        .iter()
                        .map(|b| spell_chordal_bar(b, flats))
                        .collect::<Vec<_>>()
                        .join(" | ");
                    let _ = writeln!(out, "P{:<id_w$} {:<name_w$} | {text} |", p.id, name_field(p));
                }
            }
            emit_autos(&mut out, &p.autos);
        }
    }

    let mut started = false;
    let mut in_rows = false;
    for item in &d.timeline {
        if !started {
            if !d.patterns.is_empty() {
                out.push('\n');
            }
            started = true;
        }
        match item {
            TimelineItem::Row(row) => {
                if !in_rows {
                    out.push_str("arrangement:\n");
                    in_rows = true;
                }
                let stack = if row.stack.is_empty() {
                    "z".to_string()
                } else {
                    row.stack.iter().map(|id| format!("P{id}")).collect::<Vec<_>>().join("+")
                };
                let label = row.label.as_ref().map(|l| format!("{l}: ")).unwrap_or_default();
                match row.reps {
                    1 => {
                        let _ = writeln!(out, "  {label}[{stack}]");
                    }
                    n => {
                        let _ = writeln!(out, "  {label}[{stack}] x{n}");
                    }
                }
            }
            TimelineItem::Direct(di) => {
                in_rows = false;
                emit_direct(&mut out, d, di, flats);
            }
        }
    }
    out
}

fn emit_direct(out: &mut String, d: &Document, di: &DirectItem, flats: bool) {
    let name = &d.instruments[di.track].name;
    let dynamic = dyn_suffix(di.base_vel);
    let meter = meter_suffix(di.meter.filter(|m| *m != d.header.meter));
    match &di.body {
        PatternBody::Drums(db) => {
            match db.variant_base {
                Some(base) => {
                    let _ = writeln!(out, "b{} {name}{dynamic}{meter} ~P{base}", di.bar);
                }
                None => {
                    let _ = writeln!(out, "b{} {name}{dynamic}{meter}", di.bar);
                }
            }
            if !db.lanes.is_empty() {
                let _ = writeln!(out, "{}", render_lanes(&db.lanes));
            }
        }
        PatternBody::Melodic(bars) => {
            let text =
                bars.iter().map(|b| spell_melodic_bar(b, flats)).collect::<Vec<_>>().join(" | ");
            let _ = writeln!(out, "b{} {name}{dynamic}{meter} | {text} |", di.bar);
        }
        PatternBody::Chordal(bars) => {
            let text =
                bars.iter().map(|b| spell_chordal_bar(b, flats)).collect::<Vec<_>>().join(" | ");
            let _ = writeln!(out, "b{} {name}*{dynamic}{meter} | {text} |", di.bar);
        }
    }
    emit_autos(out, &di.autos);
}

/// ` N/D` when a pattern/direct overrides the header meter.
fn meter_suffix(m: Option<(u32, u32)>) -> String {
    m.map(|(n, d)| format!(" {n}/{d}")).unwrap_or_default()
}

/// Canonical spelling of a bind target.
fn target_text(t: &Target) -> String {
    match t {
        Target::Cc(n) => format!("cc{n}"),
        Target::PitchBend => "bend".to_string(),
        Target::ChannelPressure => "at".to_string(),
        Target::PolyPressure(n) => format!("poly{n}"),
        Target::Nrpn(p) => format!("nrpn{p}"),
        Target::Rpn(p) => format!("rpn{p}"),
        Target::Program => "prog".to_string(),
        Target::Extern { kind, path } => format!("{}:{}", kind.tag(), path),
    }
}

/// ` [min..max]` when a bind carries a value domain, else empty.
fn domain_text(domain: Option<(f64, f64)>) -> String {
    match domain {
        None => String::new(),
        Some((lo, hi)) => {
            format!(" [{}..{}]", crate::grid::value_text(lo), crate::grid::value_text(hi))
        }
    }
}

/// Canonical spelling of an ease-to-next: `None` for the default `Lin`
/// (omitted), else the keyword. `Exp`/`Bez` carry their decimal params.
fn ease_text(ease: Ease) -> Option<String> {
    let v = crate::grid::value_text;
    match ease {
        Ease::Lin => None,
        Ease::Hold => Some("hold".to_string()),
        Ease::Smooth => Some("smooth".to_string()),
        Ease::Exp(k) => Some(format!("exp:{}", v(k))),
        Ease::Bez(x1, y1, x2, y2) => Some(format!("bez:{},{},{},{}", v(x1), v(y1), v(x2), v(y2))),
    }
}

/// Canonical spelling of one automation lane. Keyframes are already sorted
/// (parse/validate guarantee it); positions are the tick-exact rational
/// cell spelling, the default `lin` ease is omitted, and the last keyframe
/// — which eases nowhere — carries none.
fn auto_lane_text(lane: &AutoLane) -> String {
    let mut s = format!("@{} {{", lane.name);
    for (i, k) in lane.keys.iter().enumerate() {
        let _ = write!(s, " {}:{}", crate::grid::pos_text(k.at), crate::grid::value_text(k.value));
        if i + 1 < lane.keys.len()
            && let Some(tok) = ease_text(k.ease)
        {
            s.push(' ');
            s.push_str(&tok);
        }
    }
    s.push_str(" }");
    s
}

/// Emit a pattern/direct's automation lanes, indented one per line.
fn emit_autos(out: &mut String, autos: &[AutoLane]) {
    for lane in autos {
        let _ = writeln!(out, "  {}", auto_lane_text(lane));
    }
}

/// QSong → canonical text (the historical entry point): structure is
/// invented by [`from_qsong`], spelled by [`emit_document`].
///
/// **Precondition:** the input must satisfy [`QSong::validate`]
/// (quantizer/resolver output always does); see [`emit_document`].
pub fn emit(q: &QSong) -> String {
    emit_document(&from_qsong(q))
}

/// Self-similarity section labels over arrangement rows.
///
/// Rows are compared by the set of *variant roots* they stack (a hat
/// variation doesn't start a new section); a similarity drop opens a
/// section, and sections whose opening rows look alike share a letter
/// (a reprise is labeled `A` again). Sparse first/last sections become
/// `intro`/`outro`. Purely informational: the parser ignores labels, and
/// they re-derive deterministically, keeping emission canonical.
fn section_labels(
    set: &pattern::PatternSet,
    forms: &[PatternForm],
) -> std::collections::HashMap<usize, String> {
    use std::collections::{BTreeSet, HashMap};
    // Variant chains point backwards, so roots resolve in one forward pass.
    let mut root: Vec<usize> = (0..set.patterns.len()).collect();
    for i in 0..set.patterns.len() {
        match &forms[i] {
            PatternForm::DrumsDiff { base, .. } => root[i] = root[*base],
            PatternForm::Full { kin: Some(j) } => root[i] = root[*j],
            PatternForm::Full { kin: None } => {}
        }
    }
    let row_roots: Vec<BTreeSet<usize>> =
        set.rows.iter().map(|r| r.stack.iter().map(|id| root[id - 1]).collect()).collect();
    fn jaccard(
        a: &std::collections::BTreeSet<usize>,
        b: &std::collections::BTreeSet<usize>,
    ) -> f64 {
        if a.is_empty() && b.is_empty() {
            return 1.0;
        }
        let inter = a.intersection(b).count() as f64;
        let union = a.union(b).count() as f64;
        inter / union
    }

    // Novelty detection over bar-weighted windows: a boundary is where the
    // material of the previous ~8 bars and the next ~8 bars stops
    // overlapping. Sections are at least 4 bars — a one-bar fill is not a
    // section.
    const WINDOW_BARS: u32 = 8;
    const MIN_SECTION_BARS: u32 = 4;
    let window_union = |mut range: std::ops::Range<usize>, backwards: bool| -> BTreeSet<usize> {
        let mut acc = BTreeSet::new();
        let mut bars = 0u32;
        while !range.is_empty() && bars < WINDOW_BARS {
            let i = if backwards { range.end - 1 } else { range.start };
            acc.extend(row_roots[i].iter().copied());
            bars += set.rows[i].reps;
            if backwards {
                range.end -= 1;
            } else {
                range.start += 1;
            }
        }
        acc
    };
    // Novelty curve + adaptive threshold: through-composed material has a
    // uniformly low baseline similarity, so boundaries are *relative* peaks
    // (mean + σ/2, floored at 0.6), not absolute dissimilarity.
    let n_rows = row_roots.len();
    let mut novelty = vec![0.0f64; n_rows];
    for (i, nov) in novelty.iter_mut().enumerate().skip(1) {
        let before = window_union(0..i, true);
        let after = window_union(i..n_rows, false);
        *nov = 1.0 - jaccard(&before, &after);
    }
    let mean = novelty[1..].iter().sum::<f64>() / (n_rows - 1).max(1) as f64;
    let sd = (novelty[1..].iter().map(|v| (v - mean).powi(2)).sum::<f64>()
        / (n_rows - 1).max(1) as f64)
        .sqrt();
    let threshold = (mean + 0.5 * sd).max(0.6);

    let mut starts: Vec<usize> = vec![0];
    let mut bars_since_start = 0u32;
    for i in 1..n_rows {
        bars_since_start += set.rows[i - 1].reps;
        if bars_since_start < MIN_SECTION_BARS {
            continue;
        }
        let is_peak = novelty[i] >= novelty[i - 1]
            && novelty[i] >= novelty.get(i + 1).copied().unwrap_or(0.0);
        if is_peak && novelty[i] >= threshold {
            starts.push(i);
            bars_since_start = 0;
        }
    }

    let track_count = |sig: &BTreeSet<usize>| {
        sig.iter().map(|&i| set.patterns[i].track).collect::<BTreeSet<_>>().len()
    };
    let letter = |n: usize| {
        let c = (b'A' + (n % 26) as u8) as char;
        std::iter::repeat_n(c, n / 26 + 1).collect::<String>()
    };

    // A section's signature is its opening window (more stable than its
    // first row alone).
    let sigs: Vec<BTreeSet<usize>> = starts
        .iter()
        .enumerate()
        .map(|(k, &start)| {
            let end = starts.get(k + 1).copied().unwrap_or(row_roots.len());
            window_union(start..end, false)
        })
        .collect();
    let mut named: Vec<(BTreeSet<usize>, String, usize)> = Vec::new(); // (sig, label, uses)
    let mut labels: HashMap<usize, String> = HashMap::new();
    let mut order: Vec<usize> = Vec::new(); // labeled start rows, in order
    for (&start, sig) in starts.iter().zip(&sigs) {
        if sig.is_empty() {
            continue; // silent gap, no label
        }
        let label = match named.iter_mut().find(|(s, _, _)| jaccard(s, sig) > 0.5) {
            Some((_, name, uses)) => {
                *uses += 1;
                name.clone()
            }
            None => {
                let name = letter(named.len());
                named.push((sig.clone(), name.clone(), 1));
                name
            }
        };
        labels.insert(start, label);
        order.push(start);
    }
    if order.len() < 2 {
        return HashMap::new(); // one section = no information
    }
    // Sparse endpoints read better as intro/outro — only when their letter
    // isn't reused elsewhere.
    let uses_of = |row: usize, labels: &HashMap<usize, String>| {
        named.iter().find(|(_, l, _)| l == &labels[&row]).map(|(_, _, u)| *u).unwrap_or(0)
    };
    let sig_of = |row: usize| &sigs[starts.iter().position(|s| *s == row).unwrap()];
    let (first, second) = (order[0], order[1]);
    if uses_of(first, &labels) == 1 && track_count(sig_of(first)) < track_count(sig_of(second)) {
        labels.insert(first, "intro".into());
    }
    let (last, prev) = (order[order.len() - 1], order[order.len() - 2]);
    if last != first
        && uses_of(last, &labels) == 1
        && track_count(sig_of(last)) < track_count(sig_of(prev))
    {
        labels.insert(last, "outro".into());
    }
    labels
}
