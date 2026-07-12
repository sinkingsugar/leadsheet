# DESIGN-960 — the clock refactor (PLAN Phase 3a)

**Implemented 2026-07-12** (same session, on Gio's go), and 3b's syntax
(`/` fractions, `(n …)S` tuplet groups) shipped the same day — mentions
of tuplet syntax as "future"/"3b" below are historical. Decision already
made with Gio: **960 ticks per beat**, integer math, `i64`, one constant,
never serialized. This note was the audit of what moved and the rules
that kept emission byte-identical — retained as the clock's reference.

## Today's clock, audited

The unit is the **16th cell** (`grid::CELLS_PER_BEAT = 4`). Everything
below assumes it:

| Where | Assumption |
|---|---|
| `grid.rs:9,16` | `QNote.cell` / `dur_cells` are global 16th indices; `CELLS_PER_BEAT = 4` |
| `grid.rs:56-61` | `cells_per_bar = meter.0 * 4 * 4 / meter.1`; `cell_sec = 60/(bpm*4)` |
| `grid.rs:158-185` | quantize snaps seconds → cell indices; drums forced to `dur_cells = 1`… |
| `grid.rs:183` + `parse.rs:317` | …except drum `dur_cells` is **overloaded** as a stroke count (1–4) by the lane digits |
| `notation.rs` (`Tok::dur`) | token duration suffix = cell count (this is **text surface**, not internal) |
| `parse.rs` (`cpb` everywhere) | bar cursor arithmetic, lane length, chord columns (`cpb/4` beats) in cells |
| `emit.rs:76-110` | `split_bars` clips notes at `cells_per_bar` boundaries |
| `emit.rs:201,212` | chord mode: beat = 4 cells, onsets/durs `% 4` |
| `emit.rs:260-273` | drum lanes indexed by cell; beat grouping every 4 chars |
| `render.rs:8-9` | PPQ 480, `TICKS_PER_CELL = 120` |
| `render.rs:14-18` | swing keys off cell parity: offbeat 8th = `cell % 4 == 2`, offbeat 16th = `cell % 2 == 1` |
| `render.rs:65-73` | drum strokes subdivide one cell: `step = 120 / strokes` |
| `metrics.rs:126` | match tolerance = one cell, in seconds |
| `tempo.rs:271,291` | `cell = 15/bpm` (a 16th) — **seconds-domain analysis**, stays as is |
| `key.rs:53` | `dur_cells` as histogram weight — unit-agnostic, only relative |

## Target model

- `const TICKS_PER_BEAT: i64 = 960;` — appears in **one** place, wrapped
  in a `MusicalTime(i64)` newtype (positions *and* durations). Never
  serialized: no tick number ever reaches `.ls` text, JSON, or docs.
  A 16th = 240 ticks.
- `QNote { onset: MusicalTime, dur: MusicalTime, strokes: u8, … }` —
  the drum overload dies: `strokes` is its own field (1 for melodic and
  plain hits), `dur` is always a duration.
- **Text units don't change** (zero format change in 3a): a duration
  suffix `N` still means N 16ths. parse multiplies by 240 at note
  creation; emit divides by 240 when spelling tokens. Until 3b syntax
  lands, parse and quantize can only produce multiples of 240, so the
  division is exact by construction (`debug_assert!`).
- **Quantization resolution is unchanged**: inference still snaps onsets
  to the 16th grid (cell k → `onset = k * 240`). Ticks buy authoring
  resolution (3b), not transcription resolution — µtiming stays sidecar
  material. Feel never spends resolution: swing remains a render-time
  displacement, notated positions stay straight.
- **MIDI out at 960 PPQ**: 1 internal tick = 1 MIDI tick. No conversion,
  no rounding, ever. (Render is currently the only tick consumer; after
  the migration it does no time math beyond swing.)
- Bar length: `bar_ticks = meter.0 * 960 * 4 / meter.1` — exact for /4
  and /8 (960·4/8 = 480), the only denominators the format admits.
- Swing in ticks: offbeat 8th = `onset % 960 == 480`, offbeat 16th =
  `onset % 480 == 240` — same set of notes as today (only exact grid
  positions swing; future 32nds won't match and won't swing).
  Displacement keeps today's integer-division semantics:
  `pct * 960 / 100 - 480` (8th), `pct * 480 / 100 - 240` (16th) — it's
  feel, ±1 tick of truncation is inaudible and deterministic.
- Chord mode: beat-aligned = `onset % 960 == 0 && dur % 960 == 0`.
- Metrics tolerance: one 16th = 240 ticks, converted to seconds exactly
  as today.
- **Tempo-map safety** (design constraint only): `MusicalTime` is score
  position, not wall time. The only tick→seconds conversions live in
  quantize (in) and render (out), each through a single function, so a
  future piecewise tempo map replaces those two spots without a third
  clock migration.

## Why 960 (recap of the decision)

Divides exactly: 16ths 240, 32nds 120, 64ths 60, 128ths 30; triplets of
8th/16th/32nd/64th = 320/160/80/40; quintuplets 192. Septuplets round
(rule below). Industry-converged (Live, Pro Tools, modern Logic).

## Tuplet placement rule (contract now, syntax in 3b)

A tuplet is an exact semantic object (`played: n, in_time_of: m,
members`) in the IR — never pre-rounded durations. Ticks are assigned
only at compile time, in one function: member *i* of an n-tuplet over
span S starts at `round(i * S / n)`; the span always closes at exactly
S. A septuplet over a beat: 0, 137, 274, 411, 549, 686, 823 — end 960.

## Migration order (one PR-sized change)

1. `grid.rs`: `MusicalTime`, `TICKS_PER_BEAT`, new `QNote` shape
   (`onset`/`dur` ticks + `strokes`), `bar_ticks()`.
2. Mechanical call-site sweep: parse (×240 at creation), emit (÷240 at
   spelling; lane index = `onset / 240`), render (PPQ 960, tick
   passthrough, swing/stroke rules above), metrics (240-tick tolerance),
   key (weight by dur ticks — only relative, no change in result).
3. `tempo.rs` untouched (seconds-domain); quantize emits `cell * 240`.
4. Gate: **entire suite green with zero corpus regeneration** — corpus
   byte-identity is the proof the refactor is invisible. Round-trip
   properties and Matrix.mid byte-comparison re-run as in Phase 1.

## Test vectors

| Thing | Ticks |
|---|---|
| 16th / 8th / quarter | 240 / 480 / 960 |
| dotted 8th | 720 |
| bar of 4/4, 3/4, 6/8, 12/8 | 3840 / 2880 / 2880 / 5760 |
| 8th-triplet member | 320 |
| 16th-triplet member | 160 |
| quintuplet-over-beat member | 192 |
| septuplet boundaries (beat) | 0 137 274 411 549 686 823 → 960 |
| 4 buzz strokes in a 16th | step 60, note-off at +30 |
| swing 66% (8th), displacement | 66·960/100 − 480 = 153 |
| swing 58% (16th), displacement | 58·480/100 − 240 = 38 |
