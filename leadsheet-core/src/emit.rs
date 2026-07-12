//! Layer 4 — QSong → text: patterns + arrangement.
//!
//! ```text
//! # song: demo  tempo: 120.00  meter: 4/4  key: Am  grid: 1/16
//! # instruments: bass:33 drums:kit piano:0 lead:81
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
use crate::drums;
use crate::grid::{QSong, QTrack};
use crate::notation::{self, Tok, emit_token_spelled};
use crate::pattern;
use std::collections::BTreeMap;
use std::fmt::Write;

/// A note fragment clipped to one bar.
struct Seg {
    cell: u32, // bar-relative
    dur: u32,
    pitch: u8,
    vel: u8,
    tie_in: bool,
    tie_out: bool,
}

/// Median velocity of a bar, bucketed to a dynamic level.
fn base_velocity(segs: &[Seg]) -> u8 {
    let mut vels: Vec<u8> = segs.iter().map(|s| s.vel).collect();
    vels.sort_unstable();
    let median = vels[(vels.len() - 1) / 2];
    notation::vel_to_dynamic(median).1
}

/// Split a track's notes at bar boundaries: per-bar segment lists.
/// Drum notes never span: their `dur_cells` is a stroke count (see
/// [`crate::grid::QNote`]), carried through in `Seg::dur`.
fn split_bars(track: &QTrack, cells_per_bar: u32, n_bars: u32) -> Vec<Vec<Seg>> {
    let mut bars: Vec<Vec<Seg>> = (0..n_bars).map(|_| Vec::new()).collect();
    for n in &track.notes {
        if track.is_drums {
            let bar = n.cell / cells_per_bar;
            if let Some(slot) = bars.get_mut(bar as usize) {
                slot.push(Seg {
                    cell: n.cell - bar * cells_per_bar,
                    dur: n.dur_cells,
                    pitch: n.pitch,
                    vel: n.vel,
                    tie_in: false,
                    tie_out: false,
                });
            }
            continue;
        }
        let end = n.cell + n.dur_cells;
        let mut cell = n.cell;
        while cell < end {
            let bar = cell / cells_per_bar;
            let bar_end = (bar + 1) * cells_per_bar;
            let seg_end = end.min(bar_end);
            if let Some(slot) = bars.get_mut(bar as usize) {
                slot.push(Seg {
                    cell: cell - bar * cells_per_bar,
                    dur: seg_end - cell,
                    pitch: n.pitch,
                    vel: n.vel,
                    tie_in: cell > n.cell,
                    tie_out: seg_end < end,
                });
            }
            cell = seg_end;
        }
    }
    bars
}

/// Render one bar's segments as melodic voice strings (usually one voice).
/// `base` is the bar's dynamic bucket; deviating notes get `>` / `~` marks.
fn bar_voices(segs: &[Seg], cells_per_bar: u32, flats: bool, base: u8) -> Vec<String> {
    // Segments sharing (onset, duration, tie) stack into one chord token.
    let mut groups: BTreeMap<(u32, u32, bool), Vec<(u8, u8)>> = BTreeMap::new();
    for s in segs {
        groups.entry((s.cell, s.dur, s.tie_out)).or_default().push((s.pitch, s.vel));
    }
    // Greedy voice allocation: each group goes to the first voice that has
    // already ended when the group starts.
    struct Voice {
        end: u32,
        toks: Vec<Tok>,
    }
    let mut voices: Vec<Voice> = Vec::new();
    for ((cell, dur, tie), mut notes) in groups {
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
        let voice = match voices.iter_mut().find(|v| v.end <= cell) {
            Some(v) => v,
            None => {
                voices.push(Voice { end: 0, toks: Vec::new() });
                voices.last_mut().unwrap()
            }
        };
        if cell > voice.end {
            voice.toks.push(Tok::Rest { dur: cell - voice.end });
        }
        voice.toks.push(tok);
        voice.end = cell + dur;
    }
    voices
        .into_iter()
        .map(|mut v| {
            if v.end < cells_per_bar {
                v.toks.push(Tok::Rest { dur: cells_per_bar - v.end });
            }
            v.toks.iter().map(|t| emit_token_spelled(t, flats)).collect::<Vec<_>>().join(" ")
        })
        .collect()
}

/// Chord-mode body (`Am . F G7`) if — and only if — every onset group in
/// the bar is a beat-aligned, uniformly-held, canonically-voiced chord.
fn try_chordal(segs: &[Seg], cells_per_bar: u32, flats: bool) -> Option<String> {
    if segs.is_empty() || segs.iter().any(|s| s.tie_in || s.tie_out) {
        return None;
    }
    let mut groups: BTreeMap<u32, Vec<&Seg>> = BTreeMap::new();
    for s in segs {
        groups.entry(s.cell).or_default().push(s);
    }
    let onsets: Vec<u32> = groups.keys().copied().collect();
    let beats = (cells_per_bar / 4) as usize;
    let mut columns: Vec<Option<String>> = vec![None; beats]; // None = rest/hold slot
    let mut covered = vec![false; beats];
    for (i, (&onset, group)) in groups.iter().enumerate() {
        if onset % 4 != 0 {
            return None;
        }
        let dur = group[0].dur;
        if dur % 4 != 0 || group.iter().any(|s| s.dur != dur) {
            return None;
        }
        let limit = onsets.get(i + 1).copied().unwrap_or(cells_per_bar);
        if onset + dur > limit {
            return None; // overlaps the next chord (or the bar line)
        }
        let mut pitches: Vec<u8> = group.iter().map(|s| s.pitch).collect();
        pitches.sort_unstable();
        let sym = chord::detect(&pitches)?;
        let beat = (onset / 4) as usize;
        columns[beat] = Some(chord::symbol_to_string(&sym, flats));
        covered[beat..beat + (dur / 4) as usize].fill(true);
    }
    let cols: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(b, c)| match c {
            Some(sym) => sym.clone(),
            None if covered[b] => ".".to_string(),
            None => "z".to_string(),
        })
        .collect();
    Some(cols.join(" "))
}

/// Drum lane cell: empty / ghost / hit / accent, or a multi-stroke
/// subdivision (drag, triplet, buzz) filling the cell.
pub(crate) const LANE_EMPTY: u8 = 0;
pub(crate) const LANE_GHOST: u8 = 1;
pub(crate) const LANE_HIT: u8 = 2;
pub(crate) const LANE_ACCENT: u8 = 3;
pub(crate) const LANE_D2: u8 = 4; // two 32nd strokes
pub(crate) const LANE_D3: u8 = 5; // triplet strokes
pub(crate) const LANE_D4: u8 = 6; // four 64th strokes

type Lanes = BTreeMap<u8, Vec<u8>>;

fn lane_char(code: u8) -> char {
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

/// Drum step-grid: one lane per GM voice, cells marked by dynamic.
fn drum_lane_map(segs: &[Seg], cells_per_bar: u32, base: u8) -> Lanes {
    let mut lanes: Lanes = BTreeMap::new();
    for s in segs {
        let code = match s.dur {
            2 => LANE_D2,
            3 => LANE_D3,
            d if d >= 4 => LANE_D4,
            _ => match notation::mark_for_vel(s.vel, base) {
                notation::Mark::Accent => LANE_ACCENT,
                notation::Mark::Ghost => LANE_GHOST,
                notation::Mark::None => LANE_HIT,
            },
        };
        lanes.entry(s.pitch).or_insert_with(|| vec![LANE_EMPTY; cells_per_bar as usize])
            [s.cell as usize] = code;
    }
    lanes
}

/// Render lanes as tab lines, cells grouped by beat.
fn render_lanes(entries: &[(u8, Vec<u8>)]) -> String {
    let label_w =
        entries.iter().map(|(p, _)| drums::lane_label(*p).len()).max().unwrap_or(1);
    entries
        .iter()
        .map(|(pitch, cells)| {
            let mut grid = String::new();
            for (i, code) in cells.iter().enumerate() {
                if i > 0 && i % 4 == 0 {
                    grid.push(' ');
                }
                grid.push(lane_char(*code));
            }
            format!("  {:<label_w$} |{grid}|", drums::lane_label(*pitch))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn lanes_sorted(lanes: &Lanes) -> Vec<(u8, Vec<u8>)> {
    let mut entries: Vec<(u8, Vec<u8>)> = lanes.iter().map(|(p, c)| (*p, c.clone())).collect();
    entries.sort_by_key(|(p, _)| drums::lane_order(*p));
    entries
}

/// One bar's emitted form. `base` is the pattern's dynamic bucket.
enum Body {
    Melodic { base: u8, text: String },
    Chordal { base: u8, text: String },
    Drums { base: u8, lanes: Lanes },
}

impl Body {
    /// Dedup key: kind tag + dynamic + content (a chordal body must never
    /// collide with an identical-looking melodic one, nor `f` with `p`).
    fn key(&self) -> String {
        match self {
            Body::Melodic { base, text } => format!("m{base}:{text}"),
            Body::Chordal { base, text } => format!("c{base}:{text}"),
            Body::Drums { base, lanes } => {
                let mut s = format!("d{base}:");
                for (pitch, cells) in lanes {
                    s.push_str(&pitch.to_string());
                    s.push('=');
                    s.extend(cells.iter().map(|c| lane_char(*c)));
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
    Full { kin: Option<usize> },
    /// Drums only: base pattern index + the lanes that differ from it
    /// (a lane cleared relative to the base appears as all dots).
    DrumsDiff { base: usize, lanes: Vec<(u8, Vec<u8>)> },
}

fn instrument_field(t: &QTrack) -> String {
    if t.is_drums { format!("{}:kit", t.name) } else { format!("{}:{}", t.name, t.program) }
}

pub fn emit(q: &QSong) -> String {
    let flats = q.key.map(|k| k.use_flats()).unwrap_or(false);
    let mut out = String::new();
    let _ = write!(
        out,
        "# song: {}  tempo: {:.2}  meter: {}/{}",
        q.name, q.bpm, q.meter.0, q.meter.1
    );
    if let Some(key) = q.key {
        let _ = write!(out, "  key: {}", key.name());
    }
    out.push_str("  grid: 1/16\n");
    let _ = writeln!(
        out,
        "# instruments: {}",
        q.tracks.iter().map(instrument_field).collect::<Vec<_>>().join(" ")
    );
    out.push('\n');

    let cpb = q.cells_per_bar();
    let bodies: Vec<Vec<Option<Body>>> = q
        .tracks
        .iter()
        .map(|t| {
            split_bars(t, cpb, q.n_bars)
                .iter()
                .map(|segs| {
                    if segs.is_empty() {
                        return None;
                    }
                    let base = base_velocity(segs);
                    Some(if t.is_drums {
                        Body::Drums { base, lanes: drum_lane_map(segs, cpb, base) }
                    } else if let Some(c) =
                        // Chord columns are quarter-note beats; only /4 meters.
                        (q.meter.1 == 4).then(|| try_chordal(segs, cpb, flats)).flatten()
                    {
                        Body::Chordal { base, text: c }
                    } else {
                        Body::Melodic { base, text: bar_voices(segs, cpb, flats, base).join(" & ") }
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
            Body::Drums { lanes: lanes_i, .. } => {
                let mut best: Option<(usize, usize)> = None; // (cost, base)
                for j in 0..i {
                    if set.patterns[j].track != set.patterns[i].track {
                        continue;
                    }
                    let Body::Drums { lanes: lanes_j, .. } = pattern_bodies[j] else { continue };
                    let pitches: std::collections::BTreeSet<u8> =
                        lanes_i.keys().chain(lanes_j.keys()).copied().collect();
                    let cost =
                        pitches.iter().filter(|p| lanes_i.get(p) != lanes_j.get(p)).count();
                    if best.is_none_or(|(c, _)| cost < c) {
                        best = Some((cost, j));
                    }
                }
                match best {
                    Some((cost, base)) if cost < lanes_i.len() => {
                        let Body::Drums { lanes: base_lanes, .. } = pattern_bodies[base] else {
                            unreachable!()
                        };
                        let cpb = q.cells_per_bar() as usize;
                        let mut diff: Vec<(u8, Vec<u8>)> = Vec::new();
                        let pitches: std::collections::BTreeSet<u8> =
                            lanes_i.keys().chain(base_lanes.keys()).copied().collect();
                        for pitch in pitches {
                            if lanes_i.get(&pitch) != base_lanes.get(&pitch) {
                                let cells = lanes_i
                                    .get(&pitch)
                                    .cloned()
                                    .unwrap_or_else(|| vec![LANE_EMPTY; cpb]); // cleared lane
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
                for j in 0..i {
                    if set.patterns[j].track != set.patterns[i].track {
                        continue;
                    }
                    let body_j = match pattern_bodies[j] {
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

    let id_w = set.patterns.len().to_string().len();
    let name_field = |i: usize| {
        let p = &set.patterns[i];
        let name = &q.tracks[p.track].name;
        let star = if matches!(pattern_bodies[i], Body::Chordal { .. }) { "*" } else { "" };
        let dynamic = dyn_suffix(pattern_bodies[i].base());
        match &forms[i] {
            PatternForm::Full { kin: Some(j) } => {
                format!("{name}{star}{dynamic} ~P{}", set.patterns[*j].id)
            }
            _ => format!("{name}{star}{dynamic}"),
        }
    };
    let name_w = (0..set.patterns.len())
        .filter(|i| !matches!(pattern_bodies[*i], Body::Drums { .. }))
        .map(|i| name_field(i).len())
        .max()
        .unwrap_or(0);
    for i in 0..set.patterns.len() {
        let p = &set.patterns[i];
        match (&forms[i], pattern_bodies[i]) {
            (PatternForm::DrumsDiff { base, lanes }, body) => {
                let _ = writeln!(
                    out,
                    "P{:<id_w$} {}{} ~P{}",
                    p.id,
                    q.tracks[p.track].name,
                    dyn_suffix(body.base()),
                    set.patterns[*base].id
                );
                if !lanes.is_empty() {
                    let _ = writeln!(out, "{}", render_lanes(lanes));
                }
            }
            (_, Body::Drums { base, lanes }) => {
                let _ =
                    writeln!(out, "P{:<id_w$} {}{}", p.id, q.tracks[p.track].name, dyn_suffix(*base));
                let _ = writeln!(out, "{}", render_lanes(&lanes_sorted(lanes)));
            }
            (_, Body::Melodic { text, .. } | Body::Chordal { text, .. }) => {
                let _ = writeln!(out, "P{:<id_w$} {:<name_w$} | {text} |", p.id, name_field(i));
            }
        }
    }

    if !set.rows.is_empty() {
        let labels = section_labels(&set, &forms);
        out.push('\n');
        out.push_str("arrangement:\n");
        for (i, row) in set.rows.iter().enumerate() {
            let stack = if row.stack.is_empty() {
                "z".to_string()
            } else {
                row.stack.iter().map(|id| format!("P{id}")).collect::<Vec<_>>().join("+")
            };
            let label = labels.get(&i).map(|l| format!("{l}: ")).unwrap_or_default();
            match row.reps {
                1 => {
                    let _ = writeln!(out, "  {label}[{stack}]");
                }
                n => {
                    let _ = writeln!(out, "  {label}[{stack}] x{n}");
                }
            }
        }
    }
    out
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
    let row_roots: Vec<BTreeSet<usize>> = set
        .rows
        .iter()
        .map(|r| r.stack.iter().map(|id| root[id - 1]).collect())
        .collect();
    fn jaccard(a: &std::collections::BTreeSet<usize>, b: &std::collections::BTreeSet<usize>) -> f64 {
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
    let window_union = |mut range: std::ops::Range<usize>,
                        backwards: bool|
     -> BTreeSet<usize> {
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
