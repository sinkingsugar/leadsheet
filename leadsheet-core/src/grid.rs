//! Layer 1b — the clock, and quantization: seconds-domain notes →
//! grid-aligned events.
//!
//! Internal time is integer **ticks at 960 per beat** ([`MusicalTime`]) —
//! the constant lives here and nowhere else, and is never serialized:
//! `.ls` text speaks 16th cells (240 ticks), and MIDI is rendered at
//! 960 PPQ so 1 internal tick = 1 MIDI tick. Quantization still snaps to
//! the 16th grid (inference resolution is unchanged; ticks buy authoring
//! resolution). µtiming residuals are measured and reported but discarded
//! (lossy by design; a sidecar is a later option). `MusicalTime` is score
//! position, not wall time: the only tick↔seconds conversions live in
//! quantization (in) and render (out), so a future tempo map replaces
//! those two spots without another clock migration.

use crate::model::{RawNote, RawSong};
use crate::tempo;
use std::ops::{Add, AddAssign, Div, Mul, Rem, Sub};

pub const CELLS_PER_BEAT: u32 = 4;
pub const TICKS_PER_BEAT: i64 = 960;
/// One 16th cell — the text format's duration unit.
pub const TICKS_PER_SIXTEENTH: i64 = TICKS_PER_BEAT / CELLS_PER_BEAT as i64;

/// The renderable tick domain. MIDI delta-times are u28 — and midly's
/// `u28::new` MASKS silently past 2^28 — so every tick a song can place,
/// including the swing shift (< one beat) and the minimum-length
/// note-off bump, must stay inside it: hence one beat of headroom on
/// each. Both validate boundaries and the parser enforce this, which is
/// what makes render's tick casts (u32 events, u28 deltas) provably
/// safe. In 4/4 this is ~69,904 bars — about 38 hours at 120 BPM.
pub const MAX_SONG_TICKS: i64 = (1 << 28) - 1 - 2 * TICKS_PER_BEAT;

/// A score position or duration in ticks (960 per beat). Integer math
/// only; never leaves the crate as a number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct MusicalTime(pub i64);

impl MusicalTime {
    pub const ZERO: MusicalTime = MusicalTime(0);

    pub fn from_sixteenths(n: u32) -> MusicalTime {
        MusicalTime(n as i64 * TICKS_PER_SIXTEENTH)
    }

    pub fn from_beats(n: u32) -> MusicalTime {
        MusicalTime(n as i64 * TICKS_PER_BEAT)
    }

    /// Ticks → whole 16th cells, when exactly on the cell grid.
    pub fn try_as_sixteenths(self) -> Option<u32> {
        (self.0 % TICKS_PER_SIXTEENTH == 0).then_some((self.0 / TICKS_PER_SIXTEENTH) as u32)
    }

    /// Ticks → 16th cells for callers that are on-grid by construction
    /// (quantizer output, drum lane positions). Panics off-grid in every
    /// build profile — silent truncation would corrupt positions, which
    /// is strictly worse than crashing. Sub-16th melodic time is spelled
    /// via [`crate::notation::dur_text`], never through this.
    pub fn as_sixteenths_exact(self) -> u32 {
        self.try_as_sixteenths()
            .unwrap_or_else(|| panic!("{} ticks is not on the 16th grid", self.0))
    }

    pub fn ticks(self) -> i64 {
        self.0
    }

    /// How many whole `span`s cover this time (ceiling division; both
    /// values non-negative).
    pub fn spans_ceil(self, span: MusicalTime) -> u32 {
        debug_assert!(self.0 >= 0 && span.0 > 0);
        ((self.0 + span.0 - 1) / span.0).max(0) as u32
    }
}

impl Add for MusicalTime {
    type Output = MusicalTime;
    fn add(self, rhs: MusicalTime) -> MusicalTime {
        MusicalTime(self.0 + rhs.0)
    }
}

impl AddAssign for MusicalTime {
    fn add_assign(&mut self, rhs: MusicalTime) {
        self.0 += rhs.0;
    }
}

impl Sub for MusicalTime {
    type Output = MusicalTime;
    fn sub(self, rhs: MusicalTime) -> MusicalTime {
        MusicalTime(self.0 - rhs.0)
    }
}

impl Mul<i64> for MusicalTime {
    type Output = MusicalTime;
    fn mul(self, rhs: i64) -> MusicalTime {
        MusicalTime(self.0 * rhs)
    }
}

/// How many whole `rhs` spans fit (bar index, beat index, …).
impl Div for MusicalTime {
    type Output = i64;
    fn div(self, rhs: MusicalTime) -> i64 {
        self.0.div_euclid(rhs.0)
    }
}

impl Rem for MusicalTime {
    type Output = MusicalTime;
    fn rem(self, rhs: MusicalTime) -> MusicalTime {
        MusicalTime(self.0.rem_euclid(rhs.0))
    }
}

/// A note on the grid: onset/duration in ticks, plus the drum stroke
/// shape. `strokes` is the member count of a uniform subdivision of
/// `dur` (the lane digits `2`/`3`/`4` subdivide one cell; a lane tuplet
/// group `(3x.x)4` subdivides its whole span); always 1 for melodic
/// notes and plain drum hits. `stroke_mask` says which members sound
/// (bit *i* = member *i*): all-ones for digits and plain hits, sparse
/// for groups with silent slots. Members place at the DESIGN-960
/// boundary rule `round(i·dur/strokes)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QNote {
    pub pitch: u8,
    pub onset: MusicalTime,
    pub dur: MusicalTime,
    pub strokes: u8,
    pub stroke_mask: u32,
    pub vel: u8,
}

/// The all-members-sound mask for an n-stroke subdivision.
pub fn full_stroke_mask(n: u8) -> u32 {
    (1u32 << n.min(31)) - 1
}

// ---- Automation value model (decimal-canonical continuous values) ----
//
// Note *positions* stay integer ticks — a grid you land on, the way a DAW
// stores PPQ ticks or samples. Continuous *parameter values* (automation,
// expression) are decimals instead: pinning a filter cutoff to an integer
// or rational grid was a category error — it's analog. Values are `f64`
// internally and canonicalize to a fixed decimal precision at the text
// boundary, exactly the way BPM snaps to hundredths, so the emit/parse
// fixpoint holds without a float ever being written raw. The domain on a
// bind maps this decimal to the target's wire units at render only.

/// Fractional digits an automation value snaps to. Fixed decimal places
/// (not significant digits) so the grid is uniform across domains: 1/10000
/// on a normalized 0..1 parameter, harmless overkill on Hz or dB.
pub const VALUE_DECIMALS: usize = 4;

/// Canonical spelling of an automation value: fixed-precision decimal with
/// trailing zeros and a bare trailing point stripped (`2000.5`, `0.25`,
/// `-1.5`, `440`, `0`). The one spelling that survives its own reparse.
pub fn value_text(v: f64) -> String {
    let mut s = format!("{:.*}", VALUE_DECIMALS, v);
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" { "0".to_string() } else { s }
}

/// A value is canonical iff it survives its own spelling — the BPM rule
/// ([`crate::doc::bpm_is_canonical`]) generalized to the value grid. Parse
/// rejects finer precision with the snapped repair value.
pub fn value_is_canonical(v: f64) -> bool {
    v.is_finite() && value_text(v).parse::<f64>() == Ok(v)
}

fn gcd(mut a: i64, mut b: i64) -> i64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a.abs()
}

/// Canonical spelling of an automation keyframe position: whole 16th cells
/// as an integer (`0`, `8`), else a lowest-terms fraction of a cell
/// (`1/2`, `17/2`, `25/3`) — the same tick-exact rational grid note
/// durations use ([`crate::notation::dur_text`]), so every tick has
/// exactly one spelling. Positions are time: rational-exact, *not* decimal
/// (that grid is for continuous values, which are analog).
pub fn pos_text(t: MusicalTime) -> String {
    let ticks = t.ticks();
    debug_assert!(ticks >= 0, "positions are non-negative");
    let g = gcd(ticks, TICKS_PER_SIXTEENTH).max(1);
    let (num, den) = (ticks / g, TICKS_PER_SIXTEENTH / g);
    if den == 1 { num.to_string() } else { format!("{num}/{den}") }
}

impl QNote {
    /// Construct from the text format's units (16th cells).
    pub fn from_cells(pitch: u8, cell: u32, dur_cells: u32, vel: u8) -> QNote {
        QNote {
            pitch,
            onset: MusicalTime::from_sixteenths(cell),
            dur: MusicalTime::from_sixteenths(dur_cells),
            strokes: 1,
            stroke_mask: 1,
            vel,
        }
    }

    /// Onset in 16th cells — the text-unit view, for on-grid content
    /// (panics on sub-16th onsets; see [`MusicalTime::as_sixteenths_exact`]).
    pub fn cell(&self) -> u32 {
        self.onset.as_sixteenths_exact()
    }

    /// Duration in 16th cells — the text-unit view, for on-grid content.
    pub fn dur_cells(&self) -> u32 {
        self.dur.as_sixteenths_exact()
    }
}

/// An opaque, beyond-MIDI automation destination — a plugin/host/OSC
/// parameter the format carries as *intent*. These have no Standard MIDI
/// File wire form, so [`crate::render`] skips them (an LLM may rewrite a
/// lane onto a MIDI target if it wants it to sound); a host that speaks
/// the protocol honors them directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternKind {
    /// A VST3 plugin parameter (by id/path).
    Vst3,
    /// A CLAP plugin parameter.
    Clap,
    /// An Open Sound Control address.
    Osc,
    /// A host/DAW parameter (mixer, transport, arbitrary automation).
    Host,
}

impl ExternKind {
    pub fn tag(self) -> &'static str {
        match self {
            ExternKind::Vst3 => "vst3",
            ExternKind::Clap => "clap",
            ExternKind::Osc => "osc",
            ExternKind::Host => "host",
        }
    }
}

/// Where a bound automation name sends. MIDI targets ([`Target::Cc`],
/// pitch bend, channel aftertouch, NRPN) render to wire events on the
/// track's channel; [`Target::Extern`] is beyond-MIDI intent that render
/// skips. Without a bind [`domain`](crate::doc::Bind), values are decimals
/// in the target's own wire units ([`Target::wire_range`]); a `[min..max]`
/// domain instead maps the authored range onto that wire range at render:
/// - `Cc(n)`: controller `n` (0..=127), wire 0..=127
/// - `PitchBend`: signed 14-bit bend, wire −8192..=8191 (0 = center)
/// - `ChannelPressure`: channel aftertouch, wire 0..=127
/// - `PolyPressure(note)`: polyphonic aftertouch on `note` (0..=127), wire 0..=127
/// - `Nrpn(param)` / `Rpn(param)`: (non-)registered parameter (0..=16383), wire 0..=16383
/// - `Program`: program change 0..=127 — *discrete*, so it emits at the
///   keyframes only (no interpolation; ease is ignored)
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Cc(u8),
    PitchBend,
    ChannelPressure,
    PolyPressure(u8),
    Nrpn(u16),
    Rpn(u16),
    Program,
    Extern { kind: ExternKind, path: String },
}

impl Target {
    /// Whether this target renders to a Standard MIDI File (false for the
    /// opaque `Extern` intents, which render skips).
    pub fn is_midi(&self) -> bool {
        !matches!(self, Target::Extern { .. })
    }

    /// The `(lo, hi)` wire range values map into (rounded, clamped) at
    /// render, or `None` for the render-skipped `Extern` intents.
    pub fn wire_range(&self) -> Option<(f64, f64)> {
        match self {
            Target::Cc(_) | Target::ChannelPressure | Target::PolyPressure(_) | Target::Program => {
                Some((0.0, 127.0))
            }
            Target::PitchBend => Some((-8192.0, 8191.0)),
            Target::Nrpn(_) | Target::Rpn(_) => Some((0.0, 16383.0)),
            Target::Extern { .. } => None,
        }
    }
}

/// Whether a bind's `[min..max]` value domain is well-formed: both bounds
/// decimal-canonical and `min < max` (a non-degenerate, non-inverted
/// range). `None` (no domain — values are wire units) is trivially valid.
pub fn domain_is_canonical(domain: Option<(f64, f64)>) -> bool {
    match domain {
        None => true,
        Some((lo, hi)) => value_is_canonical(lo) && value_is_canonical(hi) && lo < hi,
    }
}

/// The largest exponential-ease tension magnitude. Bounds `Exp(k)` so the
/// render-time `exp(k)` stays finite (no inf/nan in the curve); `exp(16)`
/// is ~9e6, plenty of curvature for any audible ramp.
pub const EXP_TENSION_MAX: f64 = 16.0;

/// Interpolation from one keyframe to the next.
///
/// - `Lin` — straight line (the default; canonically omitted).
/// - `Hold` — step: holds the value, then jumps at the next keyframe.
/// - `Smooth` — smoothstep (3t²−2t³), a symmetric ease-in-out.
/// - `Exp(k)` — exponential tension, `k` a nonzero decimal in
///   `[-16, 16]`: `k > 0` starts slow then accelerates, `k < 0` the
///   reverse, `|k|` sets the curvature. `k == 0` is [`Ease::Lin`].
/// - `Bez(x1, y1, x2, y2)` — a cubic Bézier ease (CSS `cubic-bezier`):
///   control points `(x1,y1)`, `(x2,y2)` between the fixed `(0,0)` and
///   `(1,1)`. `x1`/`x2` in `[0, 1]` keep it single-valued in time; `y`
///   may overshoot. The fully general curve.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ease {
    Lin,
    Hold,
    Smooth,
    Exp(f64),
    Bez(f64, f64, f64, f64),
}

impl Ease {
    /// The eased fraction in [0, 1] for a raw fraction `t` in [0, 1].
    /// `Hold` never interpolates (render steps it), so it passes `t`
    /// through; the continuous eases warp it. Monotonic with warp(0)=0,
    /// warp(1)=1 for the bounded `Exp` tension and any valid `Bez`.
    pub fn warp(self, t: f64) -> f64 {
        match self {
            Ease::Lin | Ease::Hold => t,
            Ease::Smooth => t * t * (3.0 - 2.0 * t),
            Ease::Exp(k) => ((k * t).exp() - 1.0) / (k.exp() - 1.0),
            Ease::Bez(x1, y1, x2, y2) => bezier_warp(x1, y1, x2, y2, t),
        }
    }

    /// Whether this ease carries decimal-canonical, in-range parameters.
    /// `Exp(k)` needs a nonzero, bounded, on-grid tension; `Bez` needs four
    /// on-grid params with `x1`/`x2` in `[0, 1]`. A malformed ease must be
    /// rejected by both validation boundaries before it reaches the renderer.
    pub fn is_canonical(self) -> bool {
        match self {
            Ease::Exp(k) => value_is_canonical(k) && k != 0.0 && k.abs() <= EXP_TENSION_MAX,
            Ease::Bez(x1, y1, x2, y2) => {
                [x1, y1, x2, y2].iter().all(|v| value_is_canonical(*v))
                    && (0.0..=1.0).contains(&x1)
                    && (0.0..=1.0).contains(&x2)
            }
            _ => true,
        }
    }
}

/// A cubic Bézier ease: `y` at time-fraction `t`. Control points `(x1,y1)`,
/// `(x2,y2)` between `(0,0)` and `(1,1)`. Because the curve is
/// parameterized by `s`, not `t`, solve `x(s) = t` (Newton, bisection
/// fallback — cheap and robust since `x1`,`x2 ∈ [0,1]` make `x(s)`
/// monotonic) then return `y(s)`. Endpoints are exact: `warp(0)=0`,
/// `warp(1)=1`.
fn bezier_warp(x1: f64, y1: f64, x2: f64, y2: f64, t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    if t >= 1.0 {
        return 1.0;
    }
    // Polynomial coefficients of the cubic through (0,0),(x1,y1),(x2,y2),(1,1).
    let (cx, cy) = (3.0 * x1, 3.0 * y1);
    let (bx, by) = (3.0 * (x2 - x1) - cx, 3.0 * (y2 - y1) - cy);
    let (ax, ay) = (1.0 - cx - bx, 1.0 - cy - by);
    let sample_x = |s: f64| ((ax * s + bx) * s + cx) * s;
    let sample_y = |s: f64| ((ay * s + by) * s + cy) * s;
    let dxds = |s: f64| (3.0 * ax * s + 2.0 * bx) * s + cx;
    // Newton-Raphson from t, then bisection if a derivative stalls.
    let mut s = t;
    for _ in 0..8 {
        let err = sample_x(s) - t;
        if err.abs() < 1e-7 {
            return sample_y(s);
        }
        let d = dxds(s);
        if d.abs() < 1e-7 {
            break;
        }
        s -= err / d;
    }
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    let mut s = t.clamp(0.0, 1.0);
    for _ in 0..24 {
        let x = sample_x(s);
        if (x - t).abs() < 1e-7 {
            break;
        }
        if x < t {
            lo = s;
        } else {
            hi = s;
        }
        s = 0.5 * (lo + hi);
    }
    sample_y(s)
}

/// A resolved automation lane on a track: a concrete [`Target`], the bind's
/// optional `[min..max]` value `domain`, and absolute-tick keyframes
/// `(position, value, ease-to-next)`. `resolve` expands each pattern-local
/// `@name` lane through the arrangement and binds the name to its target.
/// Values are decimals ([`value_text`]) in the domain's units (or the
/// target's wire units when there is no domain); positions are exact ticks.
#[derive(Debug, Clone, PartialEq)]
pub struct QAuto {
    pub target: Target,
    /// The bind's `[min..max]` value domain; `None` = values are wire units.
    pub domain: Option<(f64, f64)>,
    pub keys: Vec<(MusicalTime, f64, Ease)>,
}

#[derive(Debug, Clone)]
pub struct QTrack {
    pub name: String,
    pub program: u8,
    pub is_drums: bool,
    /// Sorted by (onset, pitch).
    pub notes: Vec<QNote>,
    /// Resolved automation lanes (empty for compiled-from-audio songs).
    pub autos: Vec<QAuto>,
}

/// Swing feel, applied at render time. `percent` is where the swung
/// subdivision lands inside its parent beat: 50 = straight, 66 = triplet
/// shuffle. Authoring-only for now (never produced by quantization).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Swing {
    /// Which layer swings: 8 = offbeat 8ths, 16 = offbeat 16ths.
    pub level: u8,
    /// 50..=75.
    pub percent: u8,
}

#[derive(Debug, Clone)]
pub struct QSong {
    pub name: String,
    pub bpm: f64,
    /// (numerator, denominator): declared by the source when available,
    /// else detected (4/4, 3/4, 6/8 templates). With meter overrides
    /// this is the *default* (header) meter; see [`QSong::bar_meters`].
    pub meter: (u32, u32),
    /// Per-bar meters when the song changes meter: empty = every bar is
    /// `meter` (the uniform case, and what quantization produces);
    /// otherwise exactly `n_bars` entries.
    pub bar_meters: Vec<(u32, u32)>,
    /// Estimated key (header + spelling); `None` = unknown, spell sharps.
    pub key: Option<crate::key::Key>,
    /// Swing feel (authoring; see [`Swing`]).
    pub swing: Option<Swing>,
    pub n_bars: u32,
    pub tracks: Vec<QTrack>,
}

/// Bar length in ticks for one meter (exact for /4 and /8 denominators).
pub fn meter_ticks(meter: (u32, u32)) -> MusicalTime {
    MusicalTime(meter.0 as i64 * TICKS_PER_BEAT * 4 / meter.1 as i64)
}

/// Bar length in 16th cells for one meter.
pub fn meter_cells(meter: (u32, u32)) -> u32 {
    meter.0 * CELLS_PER_BEAT * 4 / meter.1
}

impl QSong {
    /// Bar length in the text format's unit (16th cells) — the *default*
    /// meter's; per-bar values come from [`QSong::bar_meter`].
    pub fn cells_per_bar(&self) -> u32 {
        meter_cells(self.meter)
    }

    /// Bar length in ticks — the *default* meter's.
    pub fn bar_ticks(&self) -> MusicalTime {
        meter_ticks(self.meter)
    }

    /// The meter of one bar (the default unless overridden).
    pub fn bar_meter(&self, bar: u32) -> (u32, u32) {
        self.bar_meters.get(bar as usize).copied().unwrap_or(self.meter)
    }

    /// Bar start ticks, one per bar plus the song end: `starts[i]` is
    /// where bar i begins, `starts[n_bars]` is the total length.
    pub fn bar_starts(&self) -> Vec<MusicalTime> {
        let mut starts = Vec::with_capacity(self.n_bars as usize + 1);
        let mut t = MusicalTime::ZERO;
        for bar in 0..self.n_bars {
            starts.push(t);
            t += meter_ticks(self.bar_meter(bar));
        }
        starts.push(t);
        starts
    }

    /// Total song length in ticks (per-bar aware).
    pub fn total_ticks(&self) -> MusicalTime {
        if self.bar_meters.is_empty() {
            self.bar_ticks() * self.n_bars as i64
        } else {
            self.bar_meters.iter().map(|m| meter_ticks(*m)).fold(MusicalTime::ZERO, |a, b| a + b)
        }
    }

    pub fn cell_sec(&self) -> f64 {
        60.0 / (self.bpm * CELLS_PER_BEAT as f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TempoSource {
    /// Constant tempo declared by the source file, grid anchored at t=0.
    Declared,
    /// Estimated from onsets (MuScriptor streams, or `--infer-tempo`).
    Inferred,
    /// User-supplied BPM; phase/downbeat still estimated from onsets.
    Override,
    /// The file declared a tempo, but onsets fit an inferred grid much
    /// better (live takes against a default click), so it was replaced.
    AutoInferred { declared_bpm: f64, declared_mean_ms: f64 },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QuantizeOptions {
    /// Force this BPM instead of declaring/inferring.
    pub bpm_override: Option<f64>,
    /// Infer tempo from onsets even if the source declares one.
    pub infer_tempo: bool,
    /// Trust a declared tempo unconditionally (disables the auto-switch).
    pub no_infer: bool,
}

/// Declared grids with residuals above this are suspects for auto-switch.
const AUTO_INFER_TRIGGER_MS: f64 = 25.0;
/// The inferred grid must be at least this much tighter to win.
const AUTO_INFER_RATIO: f64 = 0.6;

/// Mean |onset − nearest grid point| in ms for a grid hypothesis.
fn mean_residual_ms(song: &RawSong, bpm: f64, origin: f64) -> f64 {
    let cell = 60.0 / (bpm * CELLS_PER_BEAT as f64);
    let (mut sum, mut n) = (0.0, 0usize);
    for t in &song.tracks {
        for note in &t.notes {
            let k = ((note.onset - origin) / cell).round();
            sum += ((note.onset - origin) - k * cell).abs() * 1e3;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

#[derive(Debug, Clone, Copy)]
pub struct QuantizeReport {
    pub bpm: f64,
    pub tempo_source: TempoSource,
    /// Seconds of bar 0, cell 0 in the source timeline.
    pub origin: f64,
    pub note_count: usize,
    /// Onset µtiming discarded by snapping, milliseconds.
    pub mean_abs_residual_ms: f64,
    pub max_abs_residual_ms: f64,
}

pub fn quantize(song: &RawSong, opts: &QuantizeOptions) -> (QSong, QuantizeReport) {
    let declared_meter = song.source_meter.unwrap_or((4, 4));
    let (bpm, origin, meter, tempo_source) =
        match (opts.bpm_override, opts.infer_tempo, song.source_bpm) {
            (Some(bpm), _, _) => {
                let (origin, meter) = tempo::align_known_bpm(song, bpm);
                let meter = song.source_meter.unwrap_or(meter);
                (bpm, origin, meter, TempoSource::Override)
            }
            (None, false, Some(declared)) => {
                let declared_mean = mean_residual_ms(song, declared, 0.0);
                let mut choice = (declared, 0.0, declared_meter, TempoSource::Declared);
                if !opts.no_infer && declared_mean > AUTO_INFER_TRIGGER_MS {
                    let est = tempo::estimate(song);
                    let inferred_mean = mean_residual_ms(song, est.bpm, est.origin);
                    if inferred_mean < AUTO_INFER_RATIO * declared_mean {
                        // The declared meter stays authoritative if present:
                        // a lying tempo doesn't make the meter a lie too.
                        let meter = song.source_meter.unwrap_or(est.meter);
                        choice = (
                            est.bpm,
                            est.origin,
                            meter,
                            TempoSource::AutoInferred {
                                declared_bpm: declared,
                                declared_mean_ms: declared_mean,
                            },
                        );
                    }
                }
                choice
            }
            _ => {
                let est = tempo::estimate(song);
                let meter = song.source_meter.unwrap_or(est.meter);
                (est.bpm, est.origin, meter, TempoSource::Inferred)
            }
        };

    let cell_sec = 60.0 / (bpm * CELLS_PER_BEAT as f64);
    let snap = |t: f64| ((t - origin) / cell_sec).round() as i64;

    let mut residual_sum = 0.0f64;
    let mut residual_max = 0.0f64;
    let mut note_count = 0usize;
    let mut max_end = MusicalTime::ZERO;

    let tracks = song
        .tracks
        .iter()
        .map(|t| {
            let mut notes: Vec<QNote> = t
                .notes
                .iter()
                .map(|n: &RawNote| {
                    let cell_i = snap(n.onset);
                    let residual = ((n.onset - origin) - cell_i as f64 * cell_sec).abs() * 1e3;
                    residual_sum += residual;
                    residual_max = residual_max.max(residual);
                    note_count += 1;
                    let cell = cell_i.max(0) as u32;
                    let end = snap(n.end()).max(0) as u32;
                    // Drum hits are one-shots; their length carries no
                    // information (MuScriptor emits a fixed minimum anyway).
                    let dur_cells = if t.is_drums { 1 } else { end.saturating_sub(cell).max(1) };
                    let q = QNote::from_cells(n.pitch, cell, dur_cells, n.vel);
                    max_end = max_end.max(q.onset + q.dur);
                    q
                })
                .collect();
            notes.sort_by(|a: &QNote, b: &QNote| a.onset.cmp(&b.onset).then(a.pitch.cmp(&b.pitch)));
            QTrack {
                name: t.name.clone(),
                program: t.program,
                is_drums: t.is_drums,
                notes,
                autos: Vec::new(),
            }
        })
        .collect();

    let mut qsong = QSong {
        name: song.name.clone(),
        bpm,
        meter,
        bar_meters: Vec::new(),
        key: None,
        swing: None,
        n_bars: 0,
        tracks,
    };
    qsong.n_bars = max_end.spans_ceil(qsong.bar_ticks());
    qsong.key = crate::key::detect(&qsong);

    let report = QuantizeReport {
        bpm,
        tempo_source,
        origin,
        note_count,
        mean_abs_residual_ms: if note_count > 0 { residual_sum / note_count as f64 } else { 0.0 },
        max_abs_residual_ms: residual_max,
    };
    (qsong, report)
}

#[cfg(test)]
mod value_tests {
    use super::*;

    #[test]
    fn value_spelling_is_canonical_and_idempotent() {
        for &(v, want) in
            &[(2000.5, "2000.5"), (0.25, "0.25"), (-1.5, "-1.5"), (440.0, "440"), (0.0, "0")]
        {
            assert_eq!(value_text(v), want);
            assert!(value_is_canonical(v));
            let reparsed: f64 = value_text(v).parse().unwrap();
            assert_eq!(value_text(reparsed), want, "spelling is a fixpoint");
        }
        // Negative zero normalizes to `0`.
        assert_eq!(value_text(-0.0), "0");
        // Sub-precision input is non-canonical; snapping to the grid makes
        // it canonical (parse rejects the raw value with this repair).
        assert!(!value_is_canonical(0.123456));
        let snapped: f64 = value_text(0.123456).parse().unwrap();
        assert!(value_is_canonical(snapped));
    }

    #[test]
    fn position_spelling_is_tick_exact_rational() {
        // Whole cells are integers; sub-cell positions are lowest-terms
        // fractions (the note-duration grid), and 0 is `0`.
        assert_eq!(pos_text(MusicalTime::ZERO), "0");
        assert_eq!(pos_text(MusicalTime::from_sixteenths(8)), "8");
        assert_eq!(pos_text(MusicalTime(120)), "1/2"); // half a cell (a 32nd)
        assert_eq!(pos_text(MusicalTime(2040)), "17/2"); // 8.5 cells
        assert_eq!(pos_text(MusicalTime(320)), "4/3"); // a triplet member's edge
        assert_eq!(pos_text(MusicalTime(137)), "137/240"); // one tick shy of a septuplet edge
    }

    #[test]
    fn ease_warp_is_monotonic_and_pinned() {
        for ease in [
            Ease::Lin,
            Ease::Smooth,
            Ease::Exp(3.0),
            Ease::Exp(-8.0),
            Ease::Exp(16.0),
            Ease::Bez(0.42, 0.0, 0.58, 1.0), // ease-in-out
            Ease::Bez(0.25, 0.1, 0.25, 1.0), // CSS "ease"
        ] {
            assert!((ease.warp(0.0) - 0.0).abs() < 1e-9, "{ease:?} warp(0)");
            assert!((ease.warp(1.0) - 1.0).abs() < 1e-9, "{ease:?} warp(1)");
            let mut prev = f64::NEG_INFINITY;
            for i in 0..=20 {
                let v = ease.warp(i as f64 / 20.0);
                assert!(v.is_finite(), "{ease:?} nonfinite");
                assert!(v >= prev - 1e-9, "{ease:?} not monotonic");
                prev = v;
            }
        }
        // A bezier whose x-solve is exercised: the CSS ease-in-out is
        // symmetric, so its midpoint value sits at 0.5.
        assert!((Ease::Bez(0.42, 0.0, 0.58, 1.0).warp(0.5) - 0.5).abs() < 1e-6);
        // An ease-in (slow start) sits below the diagonal early.
        assert!(Ease::Bez(0.42, 0.0, 1.0, 1.0).warp(0.25) < 0.25);
    }

    #[test]
    fn ease_canonicality() {
        assert!(Ease::Exp(2.0).is_canonical());
        assert!(Ease::Exp(-1.5).is_canonical());
        assert!(!Ease::Exp(0.0).is_canonical(), "zero tension is Lin");
        assert!(!Ease::Exp(0.12345).is_canonical(), "sub-decimal tension");
        assert!(!Ease::Exp(20.0).is_canonical(), "beyond the tension bound");
        assert!(
            Ease::Lin.is_canonical() && Ease::Hold.is_canonical() && Ease::Smooth.is_canonical()
        );
        // Bezier: on-grid params, x controls in [0,1]; y may overshoot.
        assert!(Ease::Bez(0.42, 0.0, 0.58, 1.0).is_canonical());
        assert!(Ease::Bez(0.3, -0.5, 0.7, 1.5).is_canonical(), "y overshoot allowed");
        assert!(!Ease::Bez(1.5, 0.0, 0.58, 1.0).is_canonical(), "x1 out of [0,1]");
        assert!(!Ease::Bez(0.42, 0.0, 0.58, 1.00001).is_canonical(), "y2 off the decimal grid");
    }

    #[test]
    fn domain_canonicality() {
        assert!(domain_is_canonical(None));
        assert!(domain_is_canonical(Some((0.0, 1.0))));
        assert!(domain_is_canonical(Some((-2.0, 2.0))));
        assert!(!domain_is_canonical(Some((1.0, 0.0))), "inverted");
        assert!(!domain_is_canonical(Some((0.0, 0.0))), "degenerate");
        assert!(!domain_is_canonical(Some((0.0, 1.00001))), "off the decimal grid");
    }
}
