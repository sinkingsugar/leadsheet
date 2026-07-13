//! Text → [`Document`] (the faithful AST), then [`Document::resolve`] →
//! `QSong`. The inverse of [`crate::emit`]; the renderer's front door.
//!
//! Three bar-body forms, matching the emitter:
//!
//! - Melodic: `P1 bass | A,,4 ^F2 z2 [CEG]4 |` (voices with `&`, ties `-`,
//!   fractions `e/2`, tuplet groups `(3 C D E)4`)
//! - Chordal: `P3 piano* | Am . F G7 |` — `*` marks chord mode; 1 column =
//!   1 beat, `.` holds, `z` rests; symbols render as canonical voicings.
//! - Drums: a `P2 drums` line followed by indented lanes `K |x...x...|`.
//!
//! Notes can also be placed directly with `b<n>` instead of `P<n>` +
//! arrangement (handy when writing by hand). Arrangement rows may carry a
//! label (`chorus: [P1+P2] x4`); labels are source content on the
//! Document. `[z]` is a silent bar.
//!
//! Tolerant of whitespace and of anything after the closing `|`
//! (annotation comments — dropped, canonical form is the product). Strict
//! about what matters: unknown instruments, patterns or header fields,
//! bad tokens, and voices that don't sum to a full bar are hard errors
//! carrying a structured [`Diagnostic`] (code, line/col, message,
//! suggestion) precise enough for an LLM to repair the text from the
//! diagnostic alone. All validation happens HERE, line-accurately;
//! `resolve()` on a parsed Document does not fail.

use crate::chord;
use crate::doc::{
    ChordCol, DirectItem, Document, DrumsBody, Header, Instrument, LaneItem, MelodicBar,
    PatternBody, PatternDef, Row, TimelineItem,
};
use crate::drums::{
    self, LANE_ACCENT, LANE_D2, LANE_D3, LANE_D4, LANE_EMPTY, LANE_GHOST, LANE_HIT,
};
use crate::error::{Diagnostic, Error};
use crate::grid::{MusicalTime, QSong};
use crate::key::Key;
use crate::notation;
use std::collections::HashMap;

/// Hard ceilings against pathological input — parse must stay panic-free
/// with bounded time and memory on garbage. No real song gets anywhere
/// close (100k bars ≈ two days of 4/4 at 120 BPM).
const MAX_BARS: u32 = 100_000;
const MAX_METER_NUMERATOR: u32 = 64;

/// A diagnostic under construction: everything except the location, which
/// the line loop fills in (finding `at` on the raw line for the column).
struct Raw {
    code: &'static str,
    msg: String,
    hint: Option<String>,
    at: Option<String>,
}

fn raw(code: &'static str, msg: impl Into<String>) -> Raw {
    Raw { code, msg: msg.into(), hint: None, at: None }
}

impl Raw {
    fn hint(mut self, h: impl Into<String>) -> Self {
        self.hint = Some(h.into());
        self
    }

    fn at(mut self, t: impl Into<String>) -> Self {
        self.at = Some(t.into());
        self
    }
}

/// Locate `r.at` on the raw line and produce the final error.
fn diag(lineno: usize, raw_line: &str, r: Raw) -> Error {
    let col = r.at.as_deref().and_then(|t| raw_line.find(t)).map(|i| i + 1).unwrap_or(0);
    Error::Parse(Diagnostic { code: r.code, line: lineno, col, message: r.msg, suggestion: r.hint })
}

const MUSIC_LINE_SHAPE: &str =
    "music lines look like `P1 name | tokens |` (pattern) or `b3 name | tokens |` (direct bar)";
const TOKEN_SHAPE: &str = "melodic tokens are a pitch (A-G/a-g with ^ _ = accidentals, , ' \
                           octaves), a cell count, and an optional `-` tie: ^F2, [CEG]4-, z8";

/// Split `name`, `name*`, `name@mf`, `name*@mf` into (name, chordal, base).
fn parse_inst_token(tok: &str) -> Result<(&str, bool, u8), Raw> {
    let (left, base) = match tok.split_once('@') {
        None => (tok, notation::DEFAULT_VEL),
        Some((left, dynamic)) => (
            left,
            notation::dynamic_to_vel(dynamic).ok_or_else(|| {
                raw("unknown-dynamic", format!("unknown dynamic {dynamic:?} in {tok:?}"))
                    .hint("dynamics are @pp @p @mp @mf @f @ff (soft to loud)")
                    .at(tok)
            })?,
        ),
    };
    match left.strip_suffix('*') {
        Some(name) => Ok((name, true, base)),
        None => Ok((left, false, base)),
    }
}

fn parse_chord_cols(content: &str, cpb: u32) -> Result<Vec<ChordCol>, Raw> {
    let beats = (cpb / 4) as usize;
    let cols: Vec<&str> = content.split_whitespace().collect();
    if cols.len() != beats {
        return Err(raw(
            "bar-length",
            format!("chord line has {} columns, expected {beats} (1 per beat)", cols.len()),
        )
        .hint("chord mode (`*`) takes exactly one symbol, `.` (hold) or `z` (rest) per beat"));
    }
    let mut out = Vec::with_capacity(beats);
    let mut have_chord = false;
    for col in cols {
        out.push(match col {
            "." => {
                if !have_chord {
                    return Err(raw("hold-across-bar", "`.` hold with no chord before it").hint(
                        "a hold cannot open a bar or follow a rest — restate the chord symbol \
                         (holds never cross barlines)",
                    ));
                }
                ChordCol::Hold
            }
            "z" => {
                have_chord = false;
                ChordCol::Rest
            }
            sym => {
                let sym_parsed = chord::parse_symbol(sym).map_err(|m| {
                    raw("bad-chord", m)
                        .hint(
                            "chord symbols look like C, Am7, F/A, G7(2); a voicing that has no \
                             exact name must be written as explicit pitches, e.g. [CEG]4",
                        )
                        .at(sym)
                })?;
                have_chord = true;
                ChordCol::Sym(sym_parsed)
            }
        });
    }
    Ok(out)
}

/// Diagnostics-friendly cell count for a tick position ("8", "7.5").
fn cells_display(t: MusicalTime) -> String {
    let cells = t.ticks() as f64 / MusicalTime::from_sixteenths(1).ticks() as f64;
    if cells.fract() == 0.0 { format!("{cells:.0}") } else { format!("{cells}") }
}

/// Parse and validate one melodic bar: every `&` voice must sum to the
/// bar exactly (or be empty).
fn parse_melodic_bar(content: &str, cpb: u32) -> Result<MelodicBar, Raw> {
    let bar = MusicalTime::from_sixteenths(cpb).ticks();
    let mut voices = Vec::new();
    for voice in content.split('&') {
        let toks =
            notation::parse_tokens(voice).map_err(|m| raw("bad-token", m).hint(TOKEN_SHAPE))?;
        // Saturating: hostile durations must not wrap the sum.
        let mut sum = 0i64;
        for tok in &toks {
            let dur = tok.dur().ticks();
            if sum.saturating_add(dur) > bar {
                let spelled = notation::emit_token(tok);
                return Err(raw(
                    "bar-length",
                    format!(
                        "bar overflows at token {spelled:?} ({} of {cpb} cells already used)",
                        cells_display(MusicalTime(sum))
                    ),
                )
                .hint(
                    "durations sum past the bar — shorten one, or carry the note into the next \
                     bar with a tie (`-`)",
                )
                .at(spelled));
            }
            sum = sum.saturating_add(dur);
        }
        if sum != bar && sum != 0 {
            return Err(raw(
                "bar-length",
                format!("voice covers {} of {cpb} cells", cells_display(MusicalTime(sum))),
            )
            .hint(
                "every `&` voice must fill the bar exactly — pad with rests (z<n>) or adjust \
                 durations",
            ));
        }
        if !toks.is_empty() {
            voices.push(toks);
        }
    }
    Ok(MelodicBar { voices })
}

fn parse_kin(tok: &str) -> Result<usize, Raw> {
    tok.strip_prefix("~P").and_then(|n| n.parse().ok()).ok_or_else(|| {
        raw("bad-variant", format!("expected ~P<n>, got {tok:?}"))
            .hint("a variant marker names its base pattern: `P8 drums ~P3`")
            .at(tok)
    })
}

/// (prefix-token, instrument, chordal?, base vel, kin base, content).
type MusicLine<'a> = (&'a str, &'a str, bool, u8, Option<usize>, &'a str);

/// Split a `P1 bass | ... |` / `b3 piano*@mp | ... |` / `P9 piano ~P7 | ... |`
/// line into its [`MusicLine`] parts.
fn split_music_line(line: &str) -> Result<MusicLine<'_>, Raw> {
    let (prefix, rest) = line.split_once('|').ok_or_else(|| {
        raw("bad-line", format!("expected `| ... |` in {line:?}")).hint(MUSIC_LINE_SHAPE)
    })?;
    let content = match rest.rfind('|') {
        Some(i) => &rest[..i],
        None => return Err(raw("bad-line", "missing closing `|`").hint(MUSIC_LINE_SHAPE)),
    };
    let mut parts = prefix.split_whitespace();
    let head = parts
        .next()
        .ok_or_else(|| raw("bad-line", "missing P<n>/b<n> label").hint(MUSIC_LINE_SHAPE))?;
    let inst = parts
        .next()
        .ok_or_else(|| raw("bad-line", "missing instrument name").hint(MUSIC_LINE_SHAPE))?;
    let kin = match parts.next() {
        None => None,
        Some(tok) => Some(parse_kin(tok)?),
    };
    if let Some(junk) = parts.next() {
        return Err(raw("bad-line", format!("unexpected {junk:?} before `|`"))
            .hint(MUSIC_LINE_SHAPE)
            .at(junk));
    }
    let (inst, chordal, base) = parse_inst_token(inst)?;
    Ok((head, inst, chordal, base, kin, content))
}

/// A drum lane line: `K |x... x.x.|` (exactly one token before `|`).
fn try_lane_line(line: &str) -> Option<(u8, &str)> {
    let (label, content) = lane_shape(line)?;
    Some((drums::lane_pitch(label)?, content))
}

/// The shape of a lane line — one bare token, then `|cells|` — regardless
/// of whether the label is a known lane.
fn lane_shape(line: &str) -> Option<(&str, &str)> {
    let (prefix, rest) = line.split_once('|')?;
    let mut toks = prefix.split_whitespace();
    let label = toks.next()?;
    if toks.next().is_some() {
        return None;
    }
    let content = &rest[..rest.rfind('|')?];
    Some((label, content))
}

const LANE_GROUP_HINT: &str = "lane tuplet groups are `(n:span strokes)` — n strokes over span \
                               cells, strokes `x`/`X`/`o`/`.`: `(3:4xxx)` is an 8th-note triplet \
                               over a beat";

fn parse_lane_items(content: &str, cpb: u32) -> Result<Vec<LaneItem>, Raw> {
    let mut items = Vec::with_capacity(cpb as usize);
    let mut width = 0u32;
    let mut chars = content.chars().peekable();
    while let Some(c) = chars.next() {
        let item = match c {
            'x' => LaneItem::Cell(LANE_HIT),
            'X' => LaneItem::Cell(LANE_ACCENT),
            'o' => LaneItem::Cell(LANE_GHOST),
            '2' => LaneItem::Cell(LANE_D2),
            '3' => LaneItem::Cell(LANE_D3),
            '4' => LaneItem::Cell(LANE_D4),
            '.' | '-' => LaneItem::Cell(LANE_EMPTY),
            '(' => {
                let mut arity = String::new();
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    arity.push(chars.next().unwrap());
                }
                let n: u8 =
                    arity.parse().ok().filter(|n| (2..=24).contains(n)).ok_or_else(|| {
                        raw("bad-lane-char", format!("bad tuplet arity {arity:?} (want 2..=24)"))
                            .hint(LANE_GROUP_HINT)
                    })?;
                if chars.next() != Some(':') {
                    return Err(raw("bad-lane-char", format!("expected `:span` after ({n}"))
                        .hint(LANE_GROUP_HINT));
                }
                let mut span_txt = String::new();
                while chars.peek().is_some_and(char::is_ascii_digit) {
                    span_txt.push(chars.next().unwrap());
                }
                let span: u8 = span_txt
                    .parse()
                    .ok()
                    .filter(|s| (1..=cpb.min(255) as u8).contains(s))
                    .ok_or_else(|| {
                        raw("bad-lane-char", format!("bad group span {span_txt:?} (cells, 1..)"))
                            .hint(LANE_GROUP_HINT)
                    })?;
                if MusicalTime::from_sixteenths(span as u32).ticks() < n as i64 {
                    return Err(raw(
                        "bad-lane-char",
                        format!("a ({n}:{span} …) group has fewer ticks than strokes"),
                    ));
                }
                let mut members = Vec::with_capacity(n as usize);
                loop {
                    match chars.next() {
                        Some('x') => members.push(LANE_HIT),
                        Some('X') => members.push(LANE_ACCENT),
                        Some('o') => members.push(LANE_GHOST),
                        Some('.') | Some('-') => members.push(LANE_EMPTY),
                        Some(w) if w.is_whitespace() => {}
                        Some(')') => break,
                        Some(c) => {
                            return Err(raw(
                                "bad-lane-char",
                                format!("bad group stroke {c:?} (strokes are . o x X)"),
                            )
                            .hint(LANE_GROUP_HINT));
                        }
                        None => {
                            return Err(raw("bad-lane-char", "unclosed lane group — missing `)`")
                                .hint(LANE_GROUP_HINT));
                        }
                    }
                }
                if members.len() != n as usize {
                    return Err(raw(
                        "bad-lane-char",
                        format!(
                            "lane group ({n}:{span} …) has {} strokes, needs exactly {n} (use \
                             `.` for silent slots)",
                            members.len()
                        ),
                    )
                    .hint(LANE_GROUP_HINT));
                }
                if members.iter().all(|m| *m == LANE_EMPTY) {
                    return Err(raw(
                        "bad-lane-char",
                        "a lane group needs at least one sounding stroke",
                    )
                    .hint("an all-silent group is plain dots — write the cells directly"));
                }
                LaneItem::Group { n, members, span }
            }
            c if c.is_whitespace() => continue,
            c => {
                return Err(raw("bad-lane-char", format!("bad lane char {c:?}")).hint(
                    "lane cells are `.` empty, `x` hit, `X` accent, `o` ghost, `2`/`3`/`4` \
                     sub-strokes, or a `(3xxx)4` tuplet group; spaces are cosmetic",
                ));
            }
        };
        width = width.saturating_add(item.width());
        items.push(item);
    }
    if width != cpb {
        return Err(raw("bar-length", format!("lane covers {width} cells, expected {cpb}")).hint(
            "one cell per 16th between the `|`s (a group counts its span); spaces don't count",
        ));
    }
    Ok(items)
}

/// An arrangement row: `label: [P1+P2+z] x4` → (label, pattern ids, reps).
fn parse_row(line: &str) -> Result<(Option<String>, Vec<usize>, u32), Raw> {
    const ROW_SHAPE: &str = "arrangement rows look like `label: [P1+P2] x4` or `[z]`";
    let open = line
        .find('[')
        .ok_or_else(|| raw("bad-row", format!("expected `[` in row {line:?}")).hint(ROW_SHAPE))?;
    let label = line[..open].trim();
    if !label.is_empty() && !label.ends_with(':') {
        return Err(raw("bad-row", format!("bad row prefix {label:?}"))
            .hint("row labels end with `:`, e.g. `chorus: [P1] x4`")
            .at(label));
    }
    let label = label.strip_suffix(':').map(|l| l.trim_end().to_string());
    let rest = &line[open + 1..];
    let (inner, after) =
        rest.split_once(']').ok_or_else(|| raw("bad-row", "unclosed `[`").hint(ROW_SHAPE))?;
    let mut ids = Vec::new();
    for part in inner.split('+') {
        let part = part.trim();
        if part == "z" || part.is_empty() {
            continue;
        }
        let id: usize = part.strip_prefix('P').and_then(|n| n.parse().ok()).ok_or_else(|| {
            raw("bad-row", format!("bad pattern ref {part:?}"))
                .hint("stack entries are P<n> (or z for silence)")
                .at(part)
        })?;
        ids.push(id);
    }
    let mut after_toks = after.split_whitespace();
    let reps = match after_toks.next() {
        None => 1,
        Some(x) => {
            x.strip_prefix('x').and_then(|n| n.parse::<u32>().ok()).filter(|n| *n >= 1).ok_or_else(
                || {
                    raw("bad-row", format!("bad repeat {x:?}"))
                        .hint("repeats are written x<n> with n >= 1, e.g. x4")
                        .at(x)
                },
            )?
        }
    };
    if let Some(junk) = after_toks.next() {
        return Err(raw("bad-row", format!("unexpected {junk:?} after the row"))
            .hint(ROW_SHAPE)
            .at(junk));
    }
    Ok((label, ids, reps))
}

/// Where a drum-lane block will land once complete.
enum BlockTarget {
    Pattern(usize),
    Direct(u32), // bar number (1-based)
}

struct DrumBlock {
    track: usize,
    target: BlockTarget,
    /// Line the block opened on (for diagnostics at flush time).
    line: usize,
    /// Base velocity from the `@dyn` mark.
    base_vel: u8,
    /// Variant base: lanes not listed here are inherited from it.
    variant_base: Option<usize>,
    lanes: Vec<(u8, Vec<LaneItem>)>,
}

const TICK_CAP_HINT: &str =
    "songs cap at 2^28 ticks (MIDI's delta domain) — fewer bars fit a bigger meter";

/// Meter-aware bar cap: MAX_BARS, tightened so the whole song fits the
/// renderable tick domain ([`crate::grid::MAX_SONG_TICKS`] — u28 MIDI
/// deltas; midly masks silently past them). Falls back to the flat cap
/// while no header has been seen (the parse fails later anyway).
fn bar_cap(header: Option<&Header>) -> u32 {
    header
        .map(|h| (crate::grid::MAX_SONG_TICKS / h.bar_ticks().ticks()).min(MAX_BARS as i64) as u32)
        .unwrap_or(MAX_BARS)
}

/// `P<n>` / `b<n>` head token → block target, with bar-number limits.
fn parse_head(head: &str) -> Result<BlockTarget, Raw> {
    if let Some(id) = head.strip_prefix('P').and_then(|n| n.parse().ok()) {
        return Ok(BlockTarget::Pattern(id));
    }
    if let Some(bar) = head.strip_prefix('b').and_then(|n| n.parse::<u32>().ok()) {
        if bar == 0 {
            return Err(raw("bad-line", "bar numbers start at 1").at(head));
        }
        if bar > MAX_BARS {
            return Err(
                raw("too-large", format!("bar {bar} is beyond the {MAX_BARS}-bar limit")).at(head)
            );
        }
        return Ok(BlockTarget::Direct(bar));
    }
    Err(raw("bad-line", format!("expected P<n> or b<n>, got {head:?}"))
        .hint("patterns start with P<n>, direct bars with b<n> (n >= 1)")
        .at(head))
}

/// Parse `.ls` text into its faithful AST. Every structural error is
/// caught here with a line-accurate diagnostic; the returned Document
/// resolves without error.
pub fn parse_document(text: &str) -> Result<Document, Error> {
    let mut header: Option<Header> = None;
    let mut instruments: Vec<Instrument> = Vec::new();
    let mut track_index: HashMap<String, usize> = HashMap::new();
    let mut patterns: Vec<PatternDef> = Vec::new();
    let mut pattern_index: HashMap<usize, usize> = HashMap::new(); // id → patterns idx
    let mut timeline: Vec<TimelineItem> = Vec::new();
    let mut pending: Option<DrumBlock> = None;
    let mut next_bar = 0u32; // arrangement cursor (for the MAX_BARS cap)

    let known_patterns = |pattern_index: &HashMap<usize, usize>| -> String {
        let mut ids: Vec<usize> = pattern_index.keys().copied().collect();
        ids.sort_unstable();
        if ids.is_empty() {
            "no patterns are defined yet".into()
        } else {
            format!(
                "defined patterns: {}",
                ids.iter().map(|i| format!("P{i}")).collect::<Vec<_>>().join(" ")
            )
        }
    };
    let known_instruments = |instruments: &[Instrument]| -> String {
        if instruments.is_empty() {
            "no instruments are declared — add them to the `# instruments:` header line".into()
        } else {
            format!(
                "declared instruments: {}",
                instruments.iter().map(|t| t.name.as_str()).collect::<Vec<_>>().join(" ")
            )
        }
    };

    // Flushing a completed drum block, shared by the loop and EOF. The
    // variant base is validated here but lanes stay as the written diff.
    fn flush_block(
        block: DrumBlock,
        patterns: &mut Vec<PatternDef>,
        pattern_index: &mut HashMap<usize, usize>,
        timeline: &mut Vec<TimelineItem>,
        bar_cap: u32,
    ) -> Result<(), Raw> {
        if let Some(base_id) = block.variant_base {
            let base = pattern_index.get(&base_id).map(|i| &patterns[*i]).ok_or_else(|| {
                raw("unknown-pattern", format!("unknown variant base P{base_id}"))
                    .hint("the ~P base must be a drum pattern defined earlier in the file")
            })?;
            if base.track != block.track {
                return Err(raw(
                    "bad-variant",
                    format!("variant base P{base_id} is a different instrument"),
                )
                .hint("a variant inherits lanes, so base and variant must share a kit"));
            }
            if !matches!(base.body, PatternBody::Drums(_)) {
                return Err(raw(
                    "bad-variant",
                    format!("variant base P{base_id} is not a drum pattern"),
                )
                .hint("drum lane inheritance needs a drum-pattern base"));
            }
        }
        let body =
            PatternBody::Drums(DrumsBody { variant_base: block.variant_base, lanes: block.lanes });
        match block.target {
            BlockTarget::Pattern(id) => {
                if pattern_index.contains_key(&id) {
                    return Err(raw("duplicate-pattern", format!("duplicate pattern P{id}"))
                        .hint("every P<n> must be unique — renumber this one"));
                }
                pattern_index.insert(id, patterns.len());
                patterns.push(PatternDef {
                    id,
                    track: block.track,
                    base_vel: block.base_vel,
                    kin: None,
                    body,
                });
            }
            BlockTarget::Direct(bar) => {
                if bar > bar_cap {
                    return Err(raw(
                        "too-large",
                        format!("bar {bar} is beyond the {bar_cap}-bar limit for this meter"),
                    )
                    .hint(TICK_CAP_HINT));
                }
                timeline.push(TimelineItem::Direct(DirectItem {
                    bar,
                    track: block.track,
                    base_vel: block.base_vel,
                    body,
                }));
            }
        }
        Ok(())
    }

    for (lineno, raw_line) in text.lines().enumerate() {
        let lineno = lineno + 1;
        let err = |r: Raw| diag(lineno, raw_line, r);
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let cpb = header.as_ref().map(|h| h.cells_per_bar()).unwrap_or(16);

        // Lane lines extend a pending drum block; anything else closes it.
        if let Some(block) = &mut pending {
            if let Some((pitch, content)) = try_lane_line(line) {
                if block.lanes.iter().any(|(p, _)| *p == pitch) {
                    return Err(err(raw(
                        "duplicate-lane",
                        format!("duplicate drum lane {:?}", drums::lane_label(pitch)),
                    )
                    .hint("each lane appears once per block — merge the hits into one line")));
                }
                let items = parse_lane_items(content, cpb).map_err(err)?;
                block.lanes.push((pitch, items));
                continue;
            }
            // A lane-shaped line with an unknown label is almost certainly a
            // typo'd lane, not the next construct.
            if let Some((label, _)) = lane_shape(line)
                && !label.starts_with('P')
                && !label.starts_with('b')
            {
                return Err(err(raw("unknown-lane", format!("unknown drum lane {label:?}"))
                    .hint(
                        "lanes: K K2 S S2 st cp h hp O C C2 Cs Cn R R2 rb T1..T6 tm cb vs B1 B2 \
                     cg1 cg2 cg3, or d<key> for any GM percussion key",
                    )
                    .at(label)));
            }
            let block = pending.take().unwrap();
            let opened = block.line;
            flush_block(
                block,
                &mut patterns,
                &mut pattern_index,
                &mut timeline,
                bar_cap(header.as_ref()),
            )
            .map_err(|r| diag(opened, "", r))?;
        }

        if let Some(rest) = line.strip_prefix('#') {
            parse_header_line(rest, &mut header, &mut instruments, &mut track_index)
                .map_err(err)?;
            continue;
        }
        if line == "arrangement:" {
            continue;
        }
        if header.is_none() {
            return Err(err(raw("missing-header", "content before `# song:` header").hint(
                "start the file with `# song: NAME  tempo: 120.00  meter: 4/4  grid: 1/16` and \
                 `# instruments: name:PROGRAM ...`",
            )));
        }

        // Arrangement row?
        let before_pipe = line.split('|').next().unwrap_or(line);
        if before_pipe.contains('[') {
            let (label, ids, reps) = parse_row(line).map_err(err)?;
            // Row unit = the longest pattern; 1-bar patterns repeat per bar,
            // longer ones must all agree on the unit length.
            let mut unit = 1u32;
            for id in &ids {
                let p = pattern_index.get(id).map(|i| &patterns[*i]).ok_or_else(|| {
                    err(raw("unknown-pattern", format!("unknown pattern P{id}"))
                        .hint(known_patterns(&pattern_index))
                        .at(format!("P{id}")))
                })?;
                unit = unit.max(p.body.n_bars());
            }
            for id in &ids {
                let len = patterns[pattern_index[id]].body.n_bars();
                if len != 1 && len != unit {
                    return Err(err(raw(
                        "pattern-length",
                        format!("P{id} is {len} bars but the row unit is {unit}"),
                    )
                    .hint(
                        "in one stack, multi-bar patterns must all be the same length; 1-bar \
                         patterns repeat each bar of the unit",
                    )
                    .at(format!("P{id}"))));
                }
            }
            let cap = bar_cap(header.as_ref());
            if next_bar as u64 + reps as u64 * unit as u64 > cap as u64 {
                return Err(err(raw(
                    "too-large",
                    format!("arrangement exceeds the {cap}-bar limit for this meter"),
                )
                .hint(TICK_CAP_HINT)));
            }
            next_bar += reps * unit;
            timeline.push(TimelineItem::Row(Row { label, stack: ids, reps }));
            continue;
        }

        if !line.contains('|') {
            // Drum block opener: `P2 drums`, `b3 drums@p`, or `P8 drums ~P3`.
            let mut parts = line.split_whitespace();
            let (head, inst) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let variant_base = match parts.next() {
                None => None,
                Some(tok) => Some(parse_kin(tok).map_err(err)?),
            };
            if parts.next().is_some() || inst.is_empty() {
                return Err(err(raw("bad-line", format!("cannot parse {line:?}")).hint(
                    "a drum block opens with `P<n> <kit-instrument>` or `b<n> <kit-instrument>`, \
                     with its lanes on the following lines",
                )));
            }
            let (inst, chordal, base_vel) = parse_inst_token(inst).map_err(err)?;
            if chordal {
                return Err(err(raw("bad-line", "drum patterns cannot be chord-mode (`*`)")));
            }
            let ti = *track_index.get(inst).ok_or_else(|| {
                err(raw("unknown-instrument", format!("unknown instrument {inst:?}"))
                    .hint(known_instruments(&instruments))
                    .at(inst))
            })?;
            if !instruments[ti].is_drums {
                return Err(err(raw("not-a-kit", format!("{inst:?} is not a drum kit"))
                    .hint(format!(
                        "{inst:?} is a melodic program; drum lanes need an instrument declared \
                         as `{inst}:kit`"
                    ))
                    .at(inst)));
            }
            let target = parse_head(head).map_err(err)?;
            pending = Some(DrumBlock {
                track: ti,
                target,
                line: lineno,
                base_vel,
                variant_base,
                lanes: Vec::new(),
            });
            continue;
        }

        // Pattern definition or direct bar line. `content` may span several
        // bars separated by `|`.
        let (head, inst, chordal, base_vel, kin, content) = split_music_line(line).map_err(err)?;
        let ti = *track_index.get(inst).ok_or_else(|| {
            err(raw("unknown-instrument", format!("unknown instrument {inst:?}"))
                .hint(known_instruments(&instruments))
                .at(inst))
        })?;
        // Melodic/chordal kinship is informational; just sanity-check it.
        if let Some(base_id) = kin {
            let base = pattern_index.get(&base_id).map(|i| &patterns[*i]).ok_or_else(|| {
                err(raw("unknown-pattern", format!("unknown variant base P{base_id}"))
                    .hint(known_patterns(&pattern_index)))
            })?;
            if base.track != ti {
                return Err(err(raw(
                    "bad-variant",
                    format!("variant base P{base_id} is a different instrument"),
                )));
            }
        }
        let body = if chordal {
            let bars = content
                .split('|')
                .map(|seg| parse_chord_cols(seg, cpb))
                .collect::<Result<Vec<_>, Raw>>()
                .map_err(err)?;
            PatternBody::Chordal(bars)
        } else {
            let bars = content
                .split('|')
                .map(|seg| parse_melodic_bar(seg, cpb))
                .collect::<Result<Vec<_>, Raw>>()
                .map_err(err)?;
            PatternBody::Melodic(bars)
        };
        match parse_head(head).map_err(err)? {
            BlockTarget::Pattern(id) => {
                if pattern_index.contains_key(&id) {
                    return Err(err(raw("duplicate-pattern", format!("duplicate pattern P{id}"))
                        .hint("every P<n> must be unique — renumber this one")
                        .at(head)));
                }
                pattern_index.insert(id, patterns.len());
                patterns.push(PatternDef { id, track: ti, base_vel, kin, body });
            }
            BlockTarget::Direct(bar) => {
                let cap = bar_cap(header.as_ref());
                if bar as u64 + body.n_bars() as u64 - 1 > cap as u64 {
                    return Err(err(raw(
                        "too-large",
                        format!("bars beyond the {cap}-bar limit for this meter"),
                    )
                    .hint(TICK_CAP_HINT)
                    .at(head)));
                }
                timeline.push(TimelineItem::Direct(DirectItem { bar, track: ti, base_vel, body }));
            }
        }
    }

    let header = header.ok_or_else(|| {
        diag(
            0,
            "",
            raw("missing-header", "missing `# song:` header").hint(
                "start the file with `# song: NAME  tempo: 120.00  meter: 4/4  grid: 1/16` and \
                 `# instruments: name:PROGRAM ...`",
            ),
        )
    })?;
    if let Some(block) = pending.take() {
        let opened = block.line;
        flush_block(
            block,
            &mut patterns,
            &mut pattern_index,
            &mut timeline,
            bar_cap(Some(&header)),
        )
        .map_err(|r| diag(opened, "", r))?;
    }
    Ok(Document { header, instruments, patterns, timeline })
}

/// Text → compiled `QSong` (the historical entry point):
/// [`parse_document`] followed by [`Document::resolve`].
pub fn parse(text: &str) -> Result<QSong, Error> {
    parse_document(text)?.resolve()
}

fn parse_header_line(
    rest: &str,
    header: &mut Option<Header>,
    instruments: &mut Vec<Instrument>,
    track_index: &mut HashMap<String, usize>,
) -> Result<(), Raw> {
    const HEADER_SHAPE: &str =
        "the song line is `# song: NAME  tempo: BPM  meter: N/D  key: K  grid: 1/16`";
    let rest = rest.trim();
    if let Some(fields) = rest.strip_prefix("song:") {
        // `song: NAME  tempo: T  meter: N/D  key: K  grid: 1/16` — the name
        // runs until the `tempo:` key (names may contain single spaces).
        let (name_part, after) = fields
            .split_once("tempo:")
            .ok_or_else(|| raw("bad-header", "header missing `tempo:`").hint(HEADER_SHAPE))?;
        let name = name_part.trim().to_string();
        let mut fields_map = HashMap::new();
        let mut it = after.split_whitespace();
        let bpm_tok = it.next().ok_or_else(|| raw("bad-tempo", "missing tempo value"))?;
        let bpm: f64 = bpm_tok
            .parse()
            .map_err(|_| raw("bad-tempo", format!("bad tempo {bpm_tok:?}")).at(bpm_tok))?;
        let mut pending: Option<&str> = None;
        while let Some(k) = pending.take().or_else(|| it.next()) {
            if k.ends_with('%') {
                // Second word of a two-word swing value.
                fields_map.insert("swing2", k);
                continue;
            }
            let v = it
                .next()
                .ok_or_else(|| raw("bad-header", format!("missing value for {k}")).at(k))?;
            if v.ends_with('%') && !k.trim_end_matches(':').eq("swing") {
                return Err(raw("bad-swing", format!("unexpected {v:?} after {k}"))
                    .hint("swing is `swing: 66%` or `swing: 16th 58%`")
                    .at(v));
            }
            fields_map.insert(k.trim_end_matches(':'), v);
            if v == "8th" || v == "16th" {
                // The percent follows as a bare token.
                if let Some(nxt) = it.next() {
                    pending = Some(nxt);
                }
            }
        }
        for k in fields_map.keys() {
            if !matches!(*k, "meter" | "key" | "swing" | "grid" | "swing2") {
                return Err(raw(
                    "unknown-header-field",
                    format!("unknown field {k:?} on the song line"),
                )
                .hint("song-line fields are: tempo, meter, key, swing, grid")
                .at(*k));
            }
        }
        if fields_map.contains_key("swing2") && !fields_map.contains_key("swing") {
            return Err(raw("bad-swing", "stray percent value without `swing:`")
                .hint("swing is `swing: 66%` or `swing: 16th 58%`"));
        }
        const METER_HINT: &str = "meter is N/D with D = 4 or 8, e.g. 4/4, 3/4, 6/8";
        let meter = match fields_map.get("meter") {
            None => (4, 4),
            Some(m) => {
                let (n, d) = m.split_once('/').ok_or_else(|| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                let n: u32 = n.parse().map_err(|_| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                let d: u32 = d.parse().map_err(|_| {
                    raw("bad-meter", format!("bad meter {m:?}")).hint(METER_HINT).at(*m)
                })?;
                if d != 4 && d != 8 {
                    return Err(raw("bad-meter", format!("unsupported meter {m:?}"))
                        .hint(METER_HINT)
                        .at(*m));
                }
                if n == 0 || n > MAX_METER_NUMERATOR {
                    return Err(raw(
                        "bad-meter",
                        format!(
                            "meter numerator out of range in {m:?} (1..={MAX_METER_NUMERATOR})"
                        ),
                    )
                    .at(*m));
                }
                (n, d)
            }
        };
        let key = match fields_map.get("key") {
            None => None,
            Some(k) => Some(Key::parse(k).ok_or_else(|| {
                raw("bad-key", format!("bad key {k:?}"))
                    .hint("keys look like C, F#, Bb, Am, Ebm")
                    .at(*k)
            })?),
        };
        // `swing: 66%` (offbeat 8ths) or `swing: 16th 60%`. Because header
        // fields split on whitespace, the two-word form arrives as
        // swing->"16th" plus a stray percent token; recover it from the map.
        const SWING_HINT: &str =
            "swing is `swing: 66%` (offbeat 8ths) or `swing: 16th 58%`; range 50%..75%";
        let swing = match fields_map.get("swing") {
            None => None,
            Some(v) => {
                let (level, pct_str) = match *v {
                    "8th" | "16th" => {
                        let lvl = if *v == "16th" { 16 } else { 8 };
                        (
                            lvl,
                            *fields_map.get("swing2").ok_or_else(|| {
                                raw("bad-swing", "swing level without percent").hint(SWING_HINT)
                            })?,
                        )
                    }
                    other => (8u8, other),
                };
                let percent: u8 = pct_str
                    .strip_suffix('%')
                    .and_then(|n| n.parse().ok())
                    .filter(|p| (50..=75).contains(p))
                    .ok_or_else(|| {
                        raw("bad-swing", format!("bad swing {pct_str:?} (want 50%..75%)"))
                            .hint(SWING_HINT)
                            .at(pct_str)
                    })?;
                Some(crate::grid::Swing { level, percent })
            }
        };
        if let Some(g) = fields_map.get("grid")
            && *g != "1/16"
        {
            return Err(raw("bad-grid", format!("unsupported grid {g:?} (only 1/16)")).at(*g));
        }
        if !bpm.is_finite() || bpm <= 0.0 {
            return Err(raw("bad-tempo", format!("bad tempo {bpm}")).at(bpm_tok));
        }
        // MIDI tempo is 24-bit µs/quarter: rejecting the unrepresentable
        // here beats silently clamping it at render time.
        if !(1.0..=16_777_215.0).contains(&(60e6 / bpm)) {
            return Err(raw("bad-tempo", format!("tempo {bpm} is not MIDI-representable"))
                .hint("MIDI tempo is 24-bit microseconds per beat: roughly 3.6 to 60000000 BPM")
                .at(bpm_tok));
        }
        // B1: source BPM is canonically hundredth-quantized — the emitter
        // spells `{:.2}`, so finer precision would silently change on the
        // first fmt. Reject with the repair value instead.
        if !crate::doc::bpm_is_canonical(bpm) {
            return Err(raw("bad-tempo", format!("tempo {bpm} carries sub-hundredth precision"))
                .hint(format!("tempo is spelled to hundredths of a BPM: write `tempo: {bpm:.2}`"))
                .at(bpm_tok));
        }
        *header = Some(Header { name, bpm, meter, key, swing });
        return Ok(());
    }
    if let Some(fields) = rest.strip_prefix("instruments:") {
        const INST_HINT: &str = "instruments are `name:PROGRAM` (GM 0-127) or `name:kit`";
        for field in fields.split_whitespace() {
            let (name, prog) = field.split_once(':').ok_or_else(|| {
                raw("bad-instrument", format!("bad instrument {field:?}")).hint(INST_HINT).at(field)
            })?;
            let (program, is_drums) = if prog == "kit" {
                (0u8, true)
            } else {
                (
                    prog.parse::<u8>().ok().filter(|p| *p <= 127).ok_or_else(|| {
                        raw("bad-instrument", format!("bad program in {field:?}"))
                            .hint(INST_HINT)
                            .at(field)
                    })?,
                    false,
                )
            };
            if !crate::doc::valid_name(name) {
                return Err(raw(
                    "bad-instrument",
                    format!("instrument name {name:?} (use letters, digits, _ or -)"),
                )
                .hint(INST_HINT)
                .at(field));
            }
            if track_index.contains_key(name) {
                return Err(
                    raw("duplicate-instrument", format!("duplicate instrument {name:?}")).at(name)
                );
            }
            track_index.insert(name.to_string(), instruments.len());
            instruments.push(Instrument { name: name.to_string(), program, is_drums });
        }
        return Ok(());
    }
    // Any other `#` line is a comment.
    Ok(())
}
