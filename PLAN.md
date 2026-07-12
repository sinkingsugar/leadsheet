# PLAN — leadsheet as the LLM entry point

Working plan for Claude Code. Discussed with Gio, July 2026.

## Scope charter

leadsheet is the **compiler and semantic IR** for music-as-text. It is the
entry point for LLMs — not the DAW, not the collab platform, not the
audio engine.

```
                 ┌─────────────────────────────────────┐
                 │  hosts (future, NOT this crate):    │
                 │  editors · CRDT collab · players    │
                 │  agents · Edge Talk embeds          │
                 └──────────────┬──────────────────────┘
                                │ uses
   .ls text  ←→  Document (AST)  ←→  QSong (compiled)  ←→  MIDI
   [emit/parse]   [this crate]      [this crate]        [render/ingest]
```

**Terminology:** `.ls` is the public **source language**; `Document`
and `QSong` are the **IRs**; MIDI is a compiled backend. Tuplet
spelling etc. is language surface, not an implementation detail — hence
the bake-off governance below.

**In scope:** the format, parse/emit, document model, quantization &
inference, render, metrics/oracles, semantic diff, diagnostics, wasm
buildability, derived analysis views.

**Out of scope, permanently:** CRDTs & multi-user state, UI, plugin
hosting, audio engine, project/asset management. Hosts do that; this
crate hands them a trustworthy AST and canonical text.

**Invariants (unchanged, from CLAUDE.md):** roundtrip oracle green;
canonical emission; never lose data to a wrong name; format changes go
through Gio.

---

## Phase 1 — Bulletproof the contract

Make the invariants machine-enforced before anything moves.

**Status: DONE 2026-07-12** (tests/props.rs, tests/robustness.rs,
tests/diagnostics.rs, tests/corpus.rs + corpus/, CLI check/fmt). The
property suite immediately caught three canonicality bugs in parse tie
tracking and emit dynamic-base derivation; fixed and merged
(`fix/canonicality`, reviewed by Gio) — ties are a multiset of pending
continuations matched by end cell, and the `@dyn` base derives from
parse-observable token-group votes. All properties run un-ignored.
Tagged `phase-1`.

- [x] **Property tests** (`proptest` dev-dep in leadsheet-core):
      arbitrary valid `QSong` generator → `emit → parse → emit`
      byte-identical — THE primary invariant. Structural
      `parse(emit(q)) == q` only holds when generators are constrained
      to bucketed velocities / quantized values: constrain the
      generator, don't weaken the assertion, and never "fix" production
      to satisfy an unconstrained generator. Plus
      `parse → render → ingest → quantize` note-F1 == 1.0 on
      already-quantized input.
- [x] **Parser robustness**: `&str` is valid UTF-8 by construction, so
      the real properties are: arbitrary Unicode strings into `parse()`
      never panic (clean `Err`, bounded time/memory on pathological
      input); arbitrary *bytes* at the CLI file boundary produce a clean
      encoding error, not a crash.
- [x] **Golden corpus**: `corpus/` with paired `.mid` + `.ls` fixtures
      (synthetic band pieces + at least one real MuScriptor transcription,
      trimmed). Regression test: compress output byte-identical to the
      committed `.ls`.
- [x] **Structured diagnostics**: `Diagnostic { code, line/col, message,
      suggestion }` in `error.rs`; parse errors carry them. No new deps
      (no miette/ariadne), keep human `Display`. Goal: an LLM can
      self-repair a bad `.ls` from the diagnostic alone — test the common
      authoring mistakes (bad bar length, unknown lane, hold across
      barline, bad chord symbol, dangling `~P` reference).
- [x] **Agent-loop CLI**: `leadsheet check song.ls [--json]` (parse +
      validate, print diagnostics) and `leadsheet fmt song.ls` (parse +
      canonical emit — trivial by construction, never reinterprets).

Acceptance: `cargo test` green incl. new suites; no format changes.

## Phase 2 — Expose the document model (the big one)

Today `parse()` flattens patterns/arrangement into `QSong`, discarding
source structure. Hosts (and the eventual CRDT layer) need the structure.

- [ ] New `doc` module: `Document { header, patterns: Vec<PatternDef>,
      arrangement: Vec<Row>, direct_bars }` — the faithful AST of a `.ls`
      file, all pattern kinds (melodic/chordal/drums, variants, dynamics).
- [ ] Split the pipeline: `parse(text) -> Document`,
      `Document::resolve() -> QSong` (compilation),
      `emit(&Document) -> text` and `Document::from_qsong(q)` (what the
      compressor builds). Existing `parse`/`emit` signatures stay as thin
      wrappers so nothing downstream breaks.
- [ ] Canonicality oracle extends to the Document layer:
      `emit(parse(text)) == text` for canonical text, byte-for-byte,
      same as today.
- [ ] **Semantic diff**: `leadsheet diff a.ls b.ls` over Documents, not
      lines. Reports at the right granularity: header changes, pattern
      added/removed/modified (per-lane for drums, per-bar for melodic),
      arrangement row changes. Plain-text output a human or LLM reads
      directly; `--json` only when a real consumer exists.

Acceptance: all existing tests + corpus green; emitted text unchanged
byte-for-byte vs. Phase 1.

## Phase 3 — Clock refactor + rhythmic depth

Prerequisite for melodic 32nds, triplets, grace notes — do it once.

**3a (clock internals) DONE 2026-07-12** — see DESIGN-960.md. Zero format
change, corpus byte-identical without regeneration, Matrix.mid compress
output byte-identical, roundtrip F1 unchanged through the 960 PPQ render.
Remaining items below are 3b (syntax, gated on the bake-off).

- [x] Internal time base moves from 16th-cells to **ticks: 960 per beat**
      — the industry-converged resolution (Ableton Live, Pro Tools,
      modern Logic). (`const TICKS_PER_BEAT`, wrapped in a `MusicalTime`
      newtype — the number appears in ONE place, is never serialized, and
      never reaches `.ls` text.) 960 divides evenly for 16ths (240),
      32nds (120), 64ths (60), 128ths (30), 8th/16th/32nd/64th triplets
      (320/160/80/40), and quintuplets (192) — everything short of
      septuplets, a one-line ×7 bump if ever needed. Rendered MIDI is
      written at 960 PPQ: 1 internal tick = 1 MIDI tick, no conversion,
      no rounding, ever. Integer `i64` math only; no floats in semantic
      positions. [DECIDED with Gio: 960, industry alignment]
- [x] **Feel never spends resolution**: ticks encode *notated* positions
      only. Swing stays a render-time property; µtiming stays sidecar
      material. This keeps the tick base about what's writable, not
      what's playable.
- [x] Fix the `dur_cells` overload: drums get an explicit
      `strokes: u8` (subdivision count) separate from duration; melodic
      duration becomes ticks. Emission of existing files byte-identical.
- [ ] Tuplets live in the IR as **exact semantic objects** (`played: N,
      in_time_of: M, members`) — never as pre-rounded durations. Tick
      placement happens only at compile time, in one function:
      boundaries `round(i·960/n)`, span always closes exactly. A
      septuplet stays "a septuplet" in the source and the Document even
      though its tick placement rounds.
- [ ] Melodic 32nds — `/` fraction spelling enters the syntax bake-off
      below. [EVAL: bake-off]
- [ ] Melodic tuplets — candidates: `(3 C D E)4` (ABC-flavored group,
      generalizes to `(5 ...)4`, optional internal weights `(3 C2 D)4`),
      vs. at least one alternative spelling. **Decided by measurement,
      not taste**: parse-only throwaway implementations per candidate,
      then an LLM bake-off — zero-shot comprehension (no FORMAT.md),
      spec-in-context writing validity, edit-task constraint pass rate.
      Winner gets canonical emission; losers die in the branch.
      [EVAL: bake-off]
- [ ] Per-track swing override (drums shuffle / pads straight).
      [GIO: floated, still undecided — skip unless blessed]
- [ ] Design note only (no implementation): the tick model must not
      hard-assume constant tempo, so a future tempo map doesn't force a
      third clock migration.

Acceptance: full oracle + corpus green; old files emit byte-identical;
new rhythms roundtrip.

## Phase 4 — Prove LLM editability (lean version)

Not a framework: a directory of task fixtures + constraint checks built
on what already exists (`metrics`, semantic diff, `check`). Lands after
the format settles (post-Phase 3).

- [ ] `eval/` fixtures: (input `.ls`, instruction, expected constraints).
      Starter tasks: transpose w/o rhythm change; edit drums w/o touching
      other tracks; extend section by 4 bars; reharmonize preserving top
      line; repair deliberately-broken `.ls` from diagnostics.
- [ ] One CLI entry (`leadsheet eval <dir>`) that checks saved model
      outputs against constraints and prints a pass/fail table. No API
      calls, no model deps in the crate.

## Phase 5 — Host enablement

- [ ] **wasm32 target**: `leadsheet-core` compiles to
      `wasm32-unknown-unknown` (deps look clean: midly/serde/thiserror);
      CI check so it stays true. Opens web playgrounds / Edge Talk embeds.
- [ ] **Analysis view** (derived, never authoritative): roman-numeral /
      chord-function annotation over real comping via
      `leadsheet inspect --harmony`, and optionally as `#`-comment lines
      in emitted text (ignored by parser, re-derived deterministically —
      invariant 2 compliant). Makes real transcriptions *legible* even
      when voicings stay as honest `[...]` tuples.
- [ ] README gets the scope charter (one paragraph: compiler, not DAW).

## Deferred / explicitly elsewhere

- CRDT collab layer: separate project, consumes `Document` + semantic
  diff. Stable object IDs live THERE (identity mapping over canonical
  text), not in `.ls`.
- CC/automation lanes, tempo & meter maps: format decisions for later
  Gio sessions; Phase 3's design note keeps the door open.
- µtiming/velocity sidecar: format slot exists; estimation lives in the
  MuScriptor candle port.
- Audio rendering, VST/WebAudio bridges: hosts.

## Rejected (KISS — re-argue only with evidence)

- **Three-layer model** (Document → SemanticSong → QSong): two layers
  suffice. Document is source truth, QSong is compiled output.
- **Structured patch/operation protocol** (`set_drum_cell`-style ops):
  LLMs edit the *text* — that's the whole thesis. `check` + `diff` close
  the loop. Revisit only if real agent traces show text editing failing.
- **Format version header**: no users yet; canonical emission + corpus
  is the compatibility story for now. One-line parser tolerance can be
  added the day it's needed.
- **Source spans on every AST node**: line/col in diagnostics only.
- **Comments as first-class AST nodes / whitespace preservation**:
  canonical form is the product; `fmt` is the answer.
- **Inline analysis strings in `.ls`** (`"ii7"` before a tuple): derived
  analysis stays out of source truth (Phase 5 `inspect --harmony`,
  re-derivable per invariant 2). Syntax decision deferred to Gio if ever.
- **Error-reporting deps** (miette/ariadne): dep budget stays at 4.

## Syntax governance (amends invariant 4)

The format's users are LLMs, not humans — so **syntax is decided by
measured model performance, not anyone's taste**. Every format change:
candidate spellings → parse-only throwaway impls → LLM bake-off
(zero-shot comprehension, spec-in-context writing validity, edit-task
constraint pass rate) → winner productionized. Gio's role: veto on
scope and invariants (no data loss, canonicality, crate boundary) —
not spelling. A minimal bake-off harness is therefore pulled ahead of
Phase 3's syntax work (it's a subset of Phase 4's eval fixtures).

## Order & rationale

**1 → 3a (clock internals, zero format changes) → syntax bake-off →
3b (winning tuplet/32nd syntax) → 2 → 4 → 5.** Safety nets first (1) —
proptest + golden corpus prove the clock refactor (3a) lands with
byte-identical emission on all existing files. The 960-tick internals
need no syntax decisions, so they never block on the bake-off. Then the
Document model + diff (2), full eval fixtures (4), host enablement (5).
Per Sol's process note: 3a starts with a short written design note
(current meanings of `cell`/`dur_cells`, modules assuming 4 cells/beat,
deterministic tuplet-boundary rounding `round(i·960/n)` closing the span
exactly, MIDI out at 960 PPQ, test vectors) — reviewed once, then
implemented in one coherent migration.
