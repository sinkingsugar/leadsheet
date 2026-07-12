# REVIEW-TRIAGE-2 — consolidated findings, 2026-07-12 (evening round)

**Temporary working doc, round 2.** Consolidates the four post-Phase-2/4/5
reviews (J2 = Johnny/Mentat, F5 = Fable 5, Sol = GPT-5.6 Sol, G3 =
Gemini 3.1 Pro). Every claim re-verified against source; corrected
severities are noted inline. Delete when every item is fixed, blessed,
or moved into PLAN.md.

Shared verdict (all four): triage round 1 fully closed, Phase 2 landed
structurally (one emitter, one canonical, timeline-as-written), Phases
4-harness and 5 done. **Compiler correctness is no longer the main
uncertainty — model behavior is.** Everything below is smaller than
what shipped today; then run the eval.

Fact-check corrections applied during consolidation:

- Sol's P1 "parser accepts degenerate tuplet spans" — **wrong at the
  parse level**: `parse_tokens` rejects `span < n` ticks, and for
  accepted spans the round-half-up boundaries give every member ≥ 1
  tick. The gap is host-built-Document-only (see B2). Demoted.
- F5's "instrument named `#x` parses as comment" — **could not
  reproduce** by trace (emitted pattern lines don't start with `#`).
  Logged unconfirmed in B3; the A4 property settles it empirically.
- Sol's diff/timeline finding — **confirmed, the only real P1.**

---

## A — Contract bugs (fix before treating Phase 2 as closed)

### A1. Semantic diff ignores timeline ordering `[Sol P1, verified]`
`diff.rs:79,94` — rows and directs are extracted into two independent
sequences and compared positionally. But `Document::timeline` order is
**tie-semantic** (E6 proves the joined/split pair compiles to different
QSongs), so two Documents that differ only in row↔direct interleaving
diff **empty**. The diff contract is "empty = semantically identical";
today that's false.

**Fix:** diff `timeline` as the ordered sequence it is — match items
positionally by (kind, content), report kind transitions
(`timeline item 2: row [P1] -> direct b2`), keep the per-row/per-lane
detail once kinds match.
**Tests:** the E6 joined/split pair as a diff regression (must be
nonempty); reorder-only cases; identical timelines stay empty.

### A2. `resolve()` advertises host safety it doesn't have `[Sol P2, verified]`
`doc.rs::resolve` doc says "the checks here guard hand-built Documents"
but never calls `validate()`. Host-built `meter = (4, 0)` panics in
`cells_per_bar()` (division by zero) before any structured error; other
malformed states reach `unwrap()`s.

**Fix:** `resolve()` calls `self.validate()?` first (parse-produced
Documents pay a negligible double-check; correctness of the public
contract wins). Optionally a crate-private `resolve_unchecked` fast
path for `parse()` if profiles ever care.
**Test:** malformed host Documents (zero denominator, bad track index)
get `Err`, never panic.

### A3. Degenerate tuplets: the host door is open `[Sol P1 → demoted; F5 #4 same finding]`
Parse-level is sound (verified: `span < n` rejected, accepted spans
give all-positive members). But `validate_tok` doesn't require
`span ≥ n`, and `place_tok` silently `continue`s zero-width members —
a validated host Document can lose notes (and a group-final tie) at
compile time. Silent data loss, our one forbidden thing.

**Fix:** `validate_tok` requires `span.ticks() >= n`; with that
enforced, the `place_tok` skip becomes unreachable — turn it into a
`debug_assert` or an `Err`.
**Tests:** `span == n` accepted (1 tick/member); host-built `span < n`
rejected by validate; tie on a group whose last member would have been
degenerate.

### A4. THE structural fix — `arb_document` property `[J2 + F5 + Sol, unanimous]`
All of B below are instances of one theorem that currently isn't
enforced: **`d.validate() == Ok` must imply
`parse_document(emit_document(d))` succeeds and equals `d` (and
emission is a fixpoint).** Document canonicality today rests on one
hand-written fixture; the QSong layer has three properties.

**Fix:** `arb_document()` generator (headers, instruments incl. hostile
names, melodic/chordal/drums bodies, variants, kin, labels, rows +
directs interleaved, tuplets incl. inexact) + two properties:
round-trip equality and fixpoint, gated on `validate()`. This is the
Phase-2 sibling of `emission_is_canonical` and would have caught every
B-item below the way the Phase-1 props caught the tie bugs.

---

## B — Validation boundary gaps (each also a test case for A4)

All host-only (parse output is valid by construction), all verified
except where noted. The durable fix is A4; these are the concrete
holes to close while writing it:

- **B1. `base_vel` off-bucket mutates music** `[J2 + Sol]` —
  `base_vel: 70` emits `@mp`, reparses as 64: silent velocity shift
  through a fmt-equivalent loop. Require `base_vel ∈ DYNAMICS` buckets
  in `Document::validate` (and `DirectItem`).
- **B2. Tuplet member shape unchecked** `[Sol + J2]` — host-built
  member ties are silently dropped by emit/resolve; member durs can
  disagree with `tuplet_boundary`; a tied group ending in a rest is
  emitter-unrepresentable. Enforce parse-canonical member shape in
  `validate_tok` (no member tie, durs match boundary rule, no rest-tie,
  span ≥ n per A3).
- **B3. Emission-breaking text fields: blacklist has holes** `[F5 + J2]`
  — verified: row label `a|b` (only `\n [ ]` are blocked) and
  instrument `a[b` (before-pipe `[` routes the line as an arrangement
  row) both pass validate and produce unparseable/misrouted emission.
  Unconfirmed: F5's `#x` instrument (could not reproduce; property
  will tell). Consider inverting to a **whitelist** (e.g.
  `[A-Za-z0-9_-]+` for instrument names, label charset minus
  structural chars) instead of chasing the blacklist.
- **B4. Pitches > 127** `[J2]` — `pitch_to_abc_spelled(200)` emits a
  spelling `parse_pitch` rejects (out of MIDI range): validated
  Document, unparseable emission. Check in `validate_tok` and
  `QSong::validate`.
- **B5. `QSong::validate` skips name/uniqueness checks entirely**
  `[Sol]` — duplicate track names, whitespace, `:@*|~` all pass; the
  emitted `# instruments:` line is then invalid or ambiguous. Share
  one name-validation helper with `Document::validate` so the two
  boundaries can't drift.
- **B6. Variant/kin integrity** `[Sol]` — `Document::validate` checks
  kin exists *earlier* but not same-track/same-kind (parse is
  stricter); drum `variant_base` same-instrument check likewise;
  duplicate lane pitches within one `DrumsBody` are unchecked (and
  possibly also parseable via two `K` lines — test both layers).

---

## C — Eval harness

### C1. `matches` is not note-exact `[Sol P2 + J2 #4]`
Checks every *target* track exists in the output, never the reverse:
a model answer with all target tracks **plus an invented extra track
passes**. False passes in the measurement instrument poison Phase 4's
data before it's collected.

**Fix:** compare canonical `BTreeMap<name, (program, is_drums, notes)>`
in both directions + `n_bars` (already there). Decide whether header
tempo/meter/key belong in `matches` or in a separate constraint.
**Test:** extra-track output must FAIL; also a renamed-track case.

---

## D — Diff quality (after A1)

- **D1. Spelling noise across key changes** `[F5 + Sol]` — bars are
  compared as `spell_melodic_bar(x, fa)` vs `(y, fb)`: change only the
  key Am→Dm and every accidental-bearing bar reports "changed"
  (`^A` vs `_B`) with identical pitches. Compare Toks structurally;
  spell only for display (pick one convention for the arrow's two
  sides).
- **D2. `kin` changes are invisible** `[Sol, verified]` —
  `diff_pattern` compares track/dynamic/body, not `kin`. `~P2 → ~P5`
  with an unchanged body diffs empty. It's source-semantic Document
  structure; report it.
- **D3. Positional row matching is a known limit** `[J2]` — inserting
  a row at the top reports the whole tail changed. Acceptable for
  lean; record it (one doc line) so nobody builds CRDT intuitions on
  it. A1's item-matching pass is the natural place to improve it later.

## E — Analysis view

- **E1. Minor roman table: offsets 10 and 11 both spell `VII`**
  `[F5 + Sol, verified]` — the raised leading tone in minor is
  indistinguishable from the natural seventh (G vs G# in Am). Spell
  offset 11 `#VII` (or adopt `vii°` convention — pick one and pin it
  with a chromatic-degree test across both tables).

## F — Docs drift + housekeeping

- **F1.** PLAN Phase 2 parenthetical "(off-grid drum onsets still
  panic in lane emission)" and emit.rs's "until QSong::validate() (B3)"
  comment — both stale: `QSong::validate` exists and checks it. `[F5]`
- **F2.** PLAN Phase 3 bullet still says inexact tuplets "are rejected
  until the Document layer can hold them" — that checkbox shipped;
  reword as history. The tempo-map design-note checkbox is arguably
  satisfied by DESIGN-960 — tick it with a pointer. `[Sol]`
- **F3.** README expression-layer bullet omits fractions and semantic
  tuplets — the biggest authoring additions. One-line fix. `[Sol]`
- **F4.** Cosmetics `[J2]`: `# instruments: ` trailing space when
  instruments are empty (fixpoint holds; still ugly — or make empty
  instruments a validate error, which B5 suggests anyway);
  `diff_pattern` takes unused `_a`/`_b`.
- **F5.** CI claim (fmt/clippy/tests/wasm on push) taken on faith by
  reviewers — confirm `.github/workflows` is committed and green.
  `[F5]`

## G — Format question (Gio / Phase 4 data)

- **G1. Duplet duality** `[J2]` — `(2 C D)4` and `C2 D2` are the same
  music with two stable canonical spellings (`detect_tuplets` never
  creates duplets; `fmt` preserves an authored one). Defensible under
  "author structure survives," but it's the format's first
  one-music-two-canonicals case. Options: FORMAT.md line blessing it,
  `fmt` demoting trivial tuplets, or a bake-off question. **Gio
  decides; data can inform.** Same applies to `(4 C D E F)4` etc. —
  any power-of-two arity.

---

## Then: the actual next thing (unanimous)

**Run Phase 4.** External eval runs producing `output.ls` per model,
including the retroactive `/`-fraction + `(n …)S` spelling bake-off
(the recorded governance debt). Compiler correctness is no longer the
bottleneck — empirical model behavior is, and C1 must land first so
the instrument doesn't lie.

## Suggested commit order

1. **Diff commit:** A1 + D2 (+ D3 doc line) + E6-pair regression.
2. **Boundary commit:** A2 + A3 + B1–B6 closed, then **A4's
   `arb_document` properties** proving the set (write the generator
   first, let it find what the list missed).
3. **Harness commit:** C1 + extra-track regression.
4. **Diff-quality commit:** D1 (structural compare, display spelling).
5. **Sweep commit:** E1 + F1–F5.
6. **Gio session:** G1 (duplet canonicality), then eval runs.
