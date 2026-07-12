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

**Status: DONE 2026-07-12.** `doc::Document` is the faithful AST
(author ids, variant lane diffs as written, multi-bar bodies, labeled
rows and direct bars on one source-ordered timeline — order is
semantic for tie joining, pinned by tests). Pipeline:
`parse_document → Document → resolve() → QSong` and
`from_qsong → Document → emit_document`, with `parse`/`emit` as
wrappers; corpus and Matrix.mid output stayed byte-identical.
`validate()` on both layers; `leadsheet diff` ships. Tuplets are
semantic objects — inexact divisions (septuplets) parse and place by
the DESIGN-960 boundary rule.

Decisions adopted from the 2026-07-12 review triage (all implemented):

- **One canonical, the Document's** (B1): `fmt` becomes
  Document-canonical — hand-authored structure (multi-bar patterns,
  author numbering, direct bars) survives it. Compressor output is just
  one particular Document; `leadsheet compress` stays the only thing
  that invents structure. The diff tool inherits this identity.
- **Real signatures** (B2): `parse_document(&str) -> Result<Document>`,
  `Document::resolve() -> Result<QSong>`,
  `emit_document(&Document) -> String`,
  `Document::from_qsong(&QSong) -> Document` (what the compressor
  builds). Existing `parse`/`emit` remain as compatibility wrappers
  (`parse_document(...)?.resolve()` / `emit_document(&from_qsong(q))`).
- **Validation boundary** (B3): `Document::validate()` /
  `QSong::validate()` preflight — hosts and wasm callers construct
  these types directly, and today only parser/quantizer discipline
  keeps fields sane. CLI entry points call validate; no new deps, no
  format change. (Closed by triage-2: unrepresentable tempo is a parse
  diagnostic, off-grid drum onsets are a validate error, and
  `document_canonicality` in tests/doc_props.rs proves the boundary.)

- [x] New `doc` module — the faithful AST (timeline replaces separate
      arrangement/direct vecs: source order is tie-semantic), tuplets as
      semantic objects (inexact divisions place by `round(i·S/n)`).
- [x] Pipeline split per B2 (wrappers kept).
- [x] Document-layer canonicality: emission is a fixpoint and Documents
      survive the loop equal (tests/phase2.rs).
- [x] `Document::validate()` / `QSong::validate()` per B3.
- [x] **Semantic diff** `leadsheet diff` (plain text; per bar / per lane).
- [x] Mixed direct-bar + arrangement semantics pinned (E6).

Acceptance: all existing tests + corpus green; compressor-emitted text
unchanged byte-for-byte vs. Phase 1.

## Phase 3 — Clock refactor + rhythmic depth

Prerequisite for melodic 32nds, triplets, grace notes — do it once.

**3a (clock internals) DONE 2026-07-12** — see DESIGN-960.md. Zero format
change, corpus byte-identical without regeneration, Matrix.mid compress
output byte-identical, roundtrip F1 unchanged through the 960 PPQ render.
**3b (fractions + tuplet syntax) DONE 2026-07-12** — same day, on the
delegated spelling decision; tuplets became true semantic objects with
Phase 2 (inexact divisions parse and place by the boundary rule). Still
open below: per-track swing (unblessed).

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
- [x] Tuplets live in the IR as **exact semantic objects**
      (`Tok::Tuplet { n, members, span }` on the Document) — never as
      pre-rounded durations. Tick placement happens only at compile
      time, in one function (`notation::tuplet_boundary`): boundaries
      `round(i·S/n)`, span always closes exactly. A septuplet stays "a
      septuplet" in the source and the Document even though its tick
      placement rounds.
- [x] Melodic 32nds — `/` fraction spelling (exactly ABC's prior:
      `C/2` halves the unit). Shipped 2026-07-12.
- [x] Melodic tuplets — `(3 C D E)4` shipped 2026-07-12 (equal members,
      marks allowed, group tie, no nesting; inexact divisions —
      septuplets — were rejected at first and became semantic objects
      on the Document the same day, placing by the boundary rule).
      Spelling decided by the resident LLM user on
      Gio's delegation — prior-alignment with ABC on both features —
      rather than a multi-model bake-off; the Phase 4 eval harness can
      re-measure and overturn the spelling if data disagrees. Internal
      weights (`(3 C2 D)4`) deliberately left out for now.
- [ ] Per-track swing override (drums shuffle / pads straight).
      [GIO: floated, still undecided — skip unless blessed]
- [x] Design note only (no implementation): the tick model must not
      hard-assume constant tempo, so a future tempo map doesn't force a
      third clock migration. Satisfied by DESIGN-960.md — `MusicalTime`
      is tempo-agnostic ticks; tempo lives only in the header field.

Acceptance: full oracle + corpus green; old files emit byte-identical;
new rhythms roundtrip.

## Phase 4 — Prove LLM editability (lean version)

**Harness + fixtures DONE 2026-07-12** (`eval/`, `leadsheet eval`);
what remains is *running* it: an external runner producing `output.ls`
per model, including the retroactive 3b spelling bake-off. No API
calls, no model deps in the crate.

- [x] `eval/` fixtures: the five starter tasks (transpose w/o rhythm
      change; drum edit w/o touching other tracks; extend by 4 bars;
      reharmonize preserving the top line; repair from diagnostics),
      each with committed known-good sample outputs.
- [ ] **Duplet duality** (G1, triage-2, Gio decides): `(2 C D)4` and
      `C2 D2` are the same music with two stable canonical spellings —
      the format's first one-music-two-canonicals case (any power-of-two
      arity: `detect_tuplets` never creates them, `fmt` preserves an
      authored one). Options: bless it in FORMAT.md, have `fmt` demote
      trivial tuplets, or fold it into the bake-off below.
- [ ] **Retroactive spelling bake-off** (governance debt from 3b): measure
      `/` fractions and `(n …)S` tuplets across models — zero-shot
      comprehension, spec-in-context writing validity, edit-task pass
      rate — and report; the data may overturn the delegated spellings.
- [x] **Expected-behavior fixture** (`eval/transcription-grid`):
      quantization snaps to 16ths; sub-16th authoring content
      deliberately does not survive MIDI → compress.
- [x] One CLI entry (`leadsheet eval <dir>`), pass/fail table, exit 1 on
      any FAIL; self-tests in CI via the sample outputs.

## Phase 5 — Host enablement

**Status: DONE 2026-07-12** (comment-line emission deliberately not
done — syntax decision stays deferred per the Rejected list).

- [x] **wasm32 target**: `leadsheet-core` builds on
      `wasm32-unknown-unknown` out of the box; GitHub Actions CI checks
      fmt, clippy -D warnings, tests, and the wasm build on every push.
- [x] **Analysis view**: `leadsheet inspect --harmony` — duration-
      weighted pitch-class scoring per bar against loose chord templates,
      spelled as symbols + roman numerals in the detected key (derived,
      lossy, never authoritative; on the Matrix transcription it reads
      the Bb-Lydian I ↔ IImaj7 oscillation straight off the comping).
- [x] README carries the scope charter.

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
measured model performance, not anyone's taste**. The default process
for a format change: candidate spellings → parse-only throwaway impls →
LLM bake-off (zero-shot comprehension, spec-in-context writing validity,
edit-task constraint pass rate) → winner productionized. Gio's role:
veto on scope and invariants (no data loss, canonicality, crate
boundary) — not spelling.

**Recorded exception (2026-07-12):** the `/`-fraction and `(n …)S`
tuplet spellings shipped on Gio's explicit delegation to the resident
LLM user, on prior-alignment grounds (both are real ABC's own
conventions), without a multi-model bake-off. Phase 4's eval harness
therefore **retroactively measures both spellings** as one of its first
jobs, and the data may overturn them. One governance story: measured by
default, this one delegated and to be back-measured.

## Order & rationale (as it actually happened)

**1 → 3a → 3b → 2 → 4 → 5.** Safety nets first (1) — proptest + golden
corpus proved the clock refactor (3a) landed with byte-identical
emission on all existing files (DESIGN-960.md was the reviewed design
note). 3b followed immediately under the delegated-spelling exception
above. Next: the Document model + diff (2, decisions B1–B3 recorded at
the top of Phase 2), then eval fixtures (4) including the retroactive
spelling measurements and an expected-behavior fixture pinning that
quantization snaps to 16ths (authoring is finer than transcription —
fractional/tuplet content deliberately does not survive a MIDI →
compress trip), then host enablement (5).
