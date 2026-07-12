# REVIEW-TRIAGE — consolidated findings, 2026-07-12

**Temporary working doc.** Consolidates the four post-Phase-1/3 reviews
(J2 = Johnny/Mentat, F5 = Fable 5, Sol = GPT-5.6 Sol, G3 = Gemini 3.1 Pro).
Every claim below was re-verified against source before inclusion; where a
reviewer's severity was off, the corrected reading is noted. Delete this
file once every item is fixed, blessed, or moved into PLAN.md.

Shared verdict (all four): Phase 1 + 3 landed as documented. Clock
migration clean, property suite doing real work, diagnostics are a
product surface. Proceed to Phase 2 **after** section A below.

---

## A — Correctness bugs (fix before Phase 2)

### A1. Fractional tuplet spans emit truncated `[Sol P1, verified w/ correction]`
`notation.rs:497` — `emit_token_spelled` spells the tuplet span with
integer division:

```rust
s.push_str(&(span.ticks() / TICKS_PER_SIXTEENTH).to_string());
```

`parse_tokens` legally accepts fractional spans (`(3 C D E)3/2` = 360
ticks, 120/member; `(3 C D E)/2` = 120 ticks, 40/member). Direct token
emission truncates these to `(3 C D E)1` (wrong music) and `(3 C D E)0`
(unparseable).

**Corrected severity:** *latent* today — the QSong pipeline can't reach
it, because `detect_tuplets` only constructs whole-cell spans (span =
member-dur × den, always a multiple of 240), so `fmt` currently respells
these as fractions (`C/2 D/2 E/2`), semantically intact. But it IS
reachable now via diagnostics (`emit_token(&tok)` in bar-overflow errors
spells a lying token, and `.at(spelled)` then fails to locate the column)
— and it becomes load-bearing the moment Phase 2 emits `Tok`s from the
Document directly.

**Fix:** spell the span with `dur_text(*span)` like every other duration.
One line.
**Tests:** token-level roundtrip for `(3 C D E)3/2`, `(3 C D E)/2`,
`(3 C D E)2-`; property: `parse_token(emit_token(t)) == t` for arbitrary
valid `Tok::Tuplet` including fractional spans.

### A2. Swing collapses short fractional notes to one MIDI tick `[Sol P1, verified live]`
`render.rs:70,78` — note-on is displaced by `swing_delta`, melodic
note-off stays at the straight-grid position, then `off.max(on + 1)`
clamps. At `swing: 66%` the displacement is 153 ticks; a 32nd (120
ticks) starting on the swung offbeat becomes a 1-tick note. Live,
audible, today.

**Fix (proposed):** displace melodic note-off by the same delta as its
note-on — swing shifts the note, notated duration is preserved. Drums
are unaffected (stroke offs already derive from the displaced start).
**Policy note for Gio (see B4):** preserving duration means a swung note
can now overlap the next straight onset by up to `delta` ticks. That's
what a human player does; if we'd rather gate it, that's an articulation
policy and needs its own line in FORMAT.md. Either way, the 1-tick
collapse must die.
**Tests:** `/2`, `/4`, and tuplet members at swung 8th and 16th
positions × both swing levels; assert rendered duration == notated.

### A3. Melodic channel wrap collides with the drum channel `[F5 #3, verified]`
`render.rs:56-63` — the skip-9 logic fires only when the counter is
*exactly* 9; after wrapping, melodic track #25 gets `25 % 16 == 9`, the
GM percussion channel.

**Fix:** allocate from 15 usable channels: `let c = ch % 15; if c >= 9
{ c + 1 } else { c }`.
**Test:** 26 melodic tracks → assert no melodic event on channel 9.

### A4. Silent swallowing in header and arrangement rows `[J2 #1, verified]`
- `parse.rs::parse_header_line` — unknown `key: value` pairs on the
  `# song:` line go into `fields_map` and are silently ignored. `metre:
  3/4` (typo) yields 4/4 with no complaint. For a format whose thesis is
  "the LLM self-repairs from diagnostics," a silent default is the one
  forbidden failure mode.
- `parse.rs::parse_row` — trailing junk after the repeat is dropped:
  `[P1] x4 garbage` parses clean.

**Fix:** hard error (`unknown-header-field`, with the known-keys list as
hint) on unrecognized keys in the `# song:` line — other `#` lines stay
comments; error on trailing row tokens. Corpus is unaffected (emitter
never writes junk).
**Tests:** diagnostics tests for both, asserting code + hint quality.

---

## B — Decisions needed (Gio blesses, before Phase 2 types are written)

### B1. Two competing definitions of "canonical" `[J2, design]`
`fmt` today reinterprets: parse → QSong → compressor emit. Hand-authored
structure (multi-bar patterns, author numbering, direct bars) is
rewritten into compressor form. Phase 2's `emit(parse(text)) == text`
promise is at the *Document* level — which creates two canonicals:
**compressor-canonical** (QSong-derived) and **author-canonical**
(Document-preserving).

**Recommendation (J2, F5 concurs implicitly):** `fmt` becomes
Document-canonical; compressor output is just one particular Document.
`leadsheet compress` stays the only thing that invents structure.
**Decide before writing Document types** — the diff tool inherits
whichever answer this gets.

### B2. Phase 2 API transition is impossible as worded `[Sol P2, verified]`
PLAN.md says both `parse(text) -> Document` and "existing parse/emit
signatures stay as thin wrappers." Same name can't do both.

**Fix (PLAN wording):** adopt Sol's split —
`parse_document(&str) -> Result<Document>`, `Document::resolve() ->
Result<QSong>`, `emit_document(&Document) -> String`; existing
`parse`/`emit` remain compatibility wrappers. Update PLAN.md Phase 2
bullets to name the real signatures.

### B3. Public validation boundary before hosts `[Sol P2, design]`
`QSong`/`QNote` fields are public; parser/quantizer construct valid
values but Phase 5 hosts and wasm callers will construct them directly.
Concrete example: parser accepts any finite BPM > 0, but MIDI tempo is
24-bit µs/quarter — a tiny BPM overflows `u24` in `render()`.

**Proposal:** `Document::validate()` / `QSong::validate()` preflight
(no new deps, no format change), called by CLI entry points; render
clamps or errors on unrepresentable tempo. Slot the work into Phase 2
while the types are open.

### B4. Swing overlap policy `[follows from A2]`
Preserve-duration (player-like, may overlap next onset) vs. gate-to-grid
(clean, needs articulation semantics). A2 implements preserve-duration
unless Gio says otherwise; whichever wins gets one sentence in FORMAT.md
under `swing:`.

---

## C — Cleanup (small, do in one commit; none block Phase 2 conceptually)

- **C1. Duplicated lane-code tables** `[F5 #1]` — `parse.rs:403-409`
  duplicates `emit.rs:245-251` (`LANE_EMPTY..LANE_D4`), comment claims
  "shared vocabulary". Move to `drums.rs`, import from both.
- **C2. Garbled diagnostic** `[F5 #2, J2 #2]` — `notation.rs:306`:
  `"(want (2..(24)"` → `"(want 2..=24)"`. This string is part of the
  LLM repair interface; it's a bug, not a nit.
- **C3. `as_sixteenths` hardening** `[Sol P3]` — doc comment still says
  sub-16th syntax "cannot be spelled yet" (stale since 3b); release
  builds silently truncate (debug_assert only). Split into
  `try_as_sixteenths() -> Option<u32>` and an asserting
  `as_sixteenths_exact()`; audit the call sites (drum lane indexing is
  provably on-grid; the rest should say why, or use `try_`).
- **C4. `validate_melodic` / `apply_melodic` duplicate bar-sum logic**
  `[F5 #5]` — with slightly different error text. Known debt; Phase 2's
  parse → Document → resolve split erases it. Do NOT patch divergently
  meanwhile — if one changes, change both.
- **C5. `Tok::Tuplet` member-duration inconsistency** `[F5 #6]` —
  parse-side members carry 240 ticks, `detect_tuplets` members keep real
  durations (e.g. 320); emission ignores member durs so nothing
  observable breaks, but `Tok` equality across the two paths lies.
  Normalize when tuplets become semantic objects in the Document AST
  (pick one convention; document it on the type).
- **C6. Header swing tokenizer** `[F5 design note]` — the
  `swing2`/pending stash dance in `parse_header_line` is the hackiest
  code in parse.rs (the comment admits it). Phase 2 touches the header
  for Document anyway: give it a real tokenizer then.
- **C7. Stale docs sweep** `[Sol P3]` — `grid.rs::as_sixteenths` comment
  (see C3); `tests/props.rs` wording that drum `dur_ticks` "doubles as
  stroke count" (true only of the generator's local `Ev` — say so);
  DESIGN-960.md places still calling tuplet syntax "future" (add one
  line at top: 3b shipped, kept as historical reference). Stale comments
  are implementation instructions to future agents here — treat as real.
- **C8. PLAN order section contradicts recorded history** `[Sol P3]` —
  bottom of PLAN.md still prescribes `3a → syntax bake-off → 3b` as the
  active order. Rewrite as the actual `1 → 3a → 3b → 2 → 4 → 5`, and
  state explicitly: Phase 4's eval harness retrospectively measures the
  `/`-fraction and tuplet spellings and may overturn them with data.
  The delegated-spelling exception was honest; two conflicting
  governance stories is not.

---

## D — Recorded, no action (behavior is intentional; write it down so nobody "fixes" it)

- **D1. Trailing-rest tuplets don't survive re-emission** `[F5 #4]` —
  `(3 C D z)4` canonicalizes to fraction spelling because the group's
  final rest merges with whatever gap follows. Fixpoint holds; only
  legibility degrades. One line in FORMAT.md ("groups with trailing
  rests may respell as fractions"), and it dies naturally when tuplets
  become Document-level semantic objects.
- **D2. 1-tick durations are legal** `[J2 #3]` — `C1/240` parses and
  roundtrips canonically. Only a hostile generator writes it. The floor
  of the duration lattice is open; known and accepted.
- **D3. Authoring surface is finer than transcription surface**
  `[F5 record-note]` — quantize snaps to 16ths, so fractional/tuplet
  content does not survive a MIDI → compress trip; the COMPILED prop
  generator correctly excludes it. Deliberate (DESIGN-960: "ticks buy
  authoring resolution, not transcription resolution"). **Action:**
  encode as an *expected-behavior* fixture in Phase 4 eval so it's
  protected, not rediscovered.

---

## E — Test additions (Sol's interaction list + A-item regressions)

Compositional gaps, in priority order — new bugs now live in feature
*interactions*, not isolated syntax:

1. Fractional tuplets × `fmt` (A1 regressions).
2. Fractions × swing × MIDI render (A2 regressions).
3. Tuplets × ties × bar boundaries (`(3 C E G)4-` into next bar; tie
   joins at exact tick).
4. Fractional durations in overlapping `&` voices.
5. Extreme-but-parser-valid tempo/meter × render (B3: u24 tempo
   overflow, 64/8 meters, MAX_BARS-scale output).
6. Direct bars mixed with arrangement rows under the future Document
   resolver (pin current semantics before Phase 2 changes the plumbing).
7. \>16 melodic tracks × channel allocation (A3 regression).

---

## Suggested commit order

1. **Cleanup commit:** C1 + C2 + A3 (20 minutes, zero risk).
2. **Correctness commit:** A1 + A2 (+ B4 decision) + E1/E2/E7 tests.
3. **Strictness commit:** A4 + diagnostics tests.
4. **Docs commit:** C3 (code) + C7 + C8 + B2 wording in PLAN.md.
5. **Gio session:** B1 (fmt canonical identity) + B3 (validation
   boundary) + B4 (swing policy) — then Phase 2 Document types.
