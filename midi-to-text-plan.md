# MIDI → Text Compressor ("LeadSheet")

**Goal:** compress MIDI (from MuScriptor transcription) into compact, *semantically meaningful* text that an LLM (Claude) can genuinely read, reason about, and write back. Round-trippable: text → MIDI → audio.

**Working name:** `leadsheet` (rename freely)

**Principle:** semantic compression over maximal compression. A lead sheet, not a zip file. The reader is an LLM — optimize for vocabulary the model has deep priors on (chord symbols, ABC notation, song structure) rather than raw token count.

---

## Context

- Upstream: [MuScriptor](https://github.com/muscriptor/muscriptor) (Kyutai/Mirelo) — multi-instrument audio→MIDI, decoder-only transformer. Native candle port + Metal PR in progress (separate track).
- MuScriptor output: stream of `NoteStartEvent {pitch, start_time (float sec), index, instrument}` / `NoteEndEvent {end_time, start_event}`. **No velocity. No tempo/bars — timing is absolute seconds.**
- Downstream: Claude reads the text, critiques/composes; ABC or the same format can be decompressed back to MIDI and auralized (fluidsynth / SpessaSynth).

## Language & deps

Rust. Suggested crates:

- `midly` — MIDI file read/write
- `serde` + `serde_json` — event stream ingest (MuScriptor `--format jsonl`)
- No FFT needed: tempo inference from note onsets (IOI analysis), not audio. (Optional later: audio-assisted beat tracking for hard cases.)
- CLI via `clap`. Library-first design (`leadsheet-core`), thin CLI on top — so it can later embed in the plugin or a service.

---

## Pipeline (4 layers)

### Layer 1 — Beat inference & quantization  ⚠️ the hidden hard part

Everything downstream depends on this. Input: note onsets in float seconds. Output: tempo map + grid-aligned events.

1. **Tempo estimation:** inter-onset-interval (IOI) histogram + autocorrelation of the onset train. Search 60–200 BPM, prefer the peak with strongest metrical support (subdivision agreement at 2x/4x).
2. **Downbeat/phase estimation:** slide beat phase, score by onset density on strong beats (bass/drum onsets weighted higher — MuScriptor gives instrument labels, use them: kick ≈ downbeat prior).
3. **Tempo drift:** support a piecewise-constant tempo map (human recordings drift). Start with global tempo; add windowed re-estimation when residual error is high.
4. **Quantize** onsets/offsets to a 16th grid (configurable; triplet grid detection as stretch goal). Keep the residual (µtiming) — discard in text, optionally preserve in a sidecar for lossless-ish round trip.
5. **Meter:** assume 4/4 first, detect 3/4 / 6/8 by downbeat scoring later.

**Acceptance:** on a MIDI file rendered at known BPM and re-transcribed by MuScriptor, recovered tempo within ±1 BPM and >95% of onsets snap to the correct grid cell.

### Layer 2 — Chordify & voice separation

- Group per-instrument. Within harmonic instruments, collapse simultaneous/overlapping notes into **chord symbols** (`Am`, `F/A`, `G7`, `Csus4`). Template matching over pitch-class sets; keep the actual voicing octave range as a hint (`Am(3)` = around octave 3) only when it matters.
- Unmatched clusters fall back to explicit pitch tuples `[A2 E3 A3]` — never lose data to a wrong chord name.
- Monophonic lines (melody, bass) stay as note sequences.
- **Key estimation** (Krumhansl or simple pitch-class histogram vs profiles) → header; enables roman-numeral output as an *optional* view (`ii-V-I` speaks LLM fluently).
- Drums: map GM percussion to a step-grid alphabet (`K`=kick, `S`=snare, `H`=hat, `O`=open hat, …), one line per bar: `K.S. K.S. K.SS K.S.`

### Layer 3 — Structure: tracker-style pattern dedup

MOD files solved this in 1987: patterns + order list.

1. Slice quantized events into **bars, per instrument**.
2. Canonical hash of each bar (instrument, grid-relative onsets, pitches/chords, durations) → identical bars share a **pattern ID** (`P1`, `P2`, …).
3. **Arrangement line:** ordered list of bar-stacks with run-length encoding: `[P1+P2]x4 [P1+P2+P3]x8`.
4. **Near-match (v1.5):** pattern + diff, e.g. `P3'{b4: walk-up}` when a bar differs from a known pattern by ≤N events.
5. **BPE over the event stream (v2):** byte-pair encoding on the quantized token sequence discovers motifs automatically (recurring riffs → single tokens), catching repetition that isn't bar-aligned.
6. **Section labels (v2):** cluster the arrangement into sections (intro/verse/chorus by self-similarity matrix) — labels are what the LLM reasons with best.

### Layer 4 — Emission: the text format

Header + patterns + arrangement. Melodic patterns in **ABC notation** (LLMs have strong ABC priors); harmonic patterns as chord symbols per beat; drums as step grids.

```
# song: <name>  tempo: 128  meter: 4/4  key: Am  grid: 1/16
# instruments: bass, drums, acoustic_piano, lead

P1 bass   | A,,2 A,,2 G,,2 E,,2 |
P2 drums  | K.S. K.S. K.SS K.S. |
P3 piano  | Am . . . | F . C . | G . . . | Am . . . |   (chords, 1 col = 1 beat)
P4 lead   | e2 c2 d2 B2 | c4 A4 |                        (ABC, octave-relative to key)

arrangement:
  intro:  [P1+P2] x4
  A:      [P1+P2+P3] x8
  B:      [P2+P4] x4
  A':     [P1+P2+P3'{b4: last beat walks up}] x8
```

**Decompressor** (`leadsheet render`): text → MIDI. This closes the loop: Claude writes/edits the text, you hear it. The decompressor is also the correctness oracle for the compressor.

---

## CLI sketch

```
leadsheet compress  in.mid            -o out.ls     # or ingest muscriptor jsonl directly
leadsheet compress  events.jsonl      -o out.ls
leadsheet render    out.ls            -o back.mid
leadsheet roundtrip in.mid                          # compress→render→diff, print F1 + ratio
```

## Test strategy

1. **Round-trip note F1** (onset/pitch/instrument, ±1 grid-cell tolerance): compress → render → compare against input MIDI. Target ≥0.95 on quantized-source material.
2. **Compression ratio** vs naive event-list text and vs raw MIDI. Target ≥10x vs event list on typical pop/rock structure.
3. **Corpus:** Lakh MIDI subset + own MuScriptor transcriptions (incl. Giovanni's old recordings — the real acceptance test 🎸).
4. **LLM legibility eval:** give Claude compressed text, ask structural questions (key? chorus chords? where does the bridge start?) — score answers. This is the metric that actually matters.
5. Property tests: quantizer never drops notes; decompressor output is always valid MIDI; pattern dedup is lossless by construction.

## Milestones

- **M0** — Ingest: MuScriptor jsonl + .mid → internal event model. Tests green.
- **M1** — Layer 1: tempo/downbeat/quantize on known-BPM material. (Hardest milestone — budget accordingly.)
- **M2** — Layer 4 minimal: emit bars as explicit note text, no dedup, + working `render`. **Round trip closes here.**
- **M3** — Layer 3: bar-hash pattern dedup + RLE arrangement. Compression becomes real.
- **M4** — Layer 2: chordify, key detection, drum step-grid, ABC melodies. Legibility becomes real.
- **M5** — Polish: near-match diffs, tempo drift, meter detection, section labeling.
- **M6 (v2)** — BPE motif discovery; roman-numeral view; velocity re-estimation from audio (spectral energy at onsets) feeding back into the format.

## Open questions

- Grid residuals: sidecar file for µtiming, or accept lossy timing? (Lean: lossy in text, sidecar optional.)
- Pitch spelling (F# vs Gb) — key-aware spelling needed for readable ABC.
- Overlapping/legato notes across bar boundaries — tie notation (ABC has ties; format must too).
- How much MuScriptor noise (spurious short notes) to filter before compression — dedicated cleanup pass?
- License note: MuScriptor **weights are CC BY-NC 4.0** (code MIT) — fine for personal/research; talk to Kyutai/Mirelo before any commercial plugin ships.

## North star

Audio → MuScriptor → `leadsheet` → Claude reads, critiques, edits, composes → `leadsheet render` → MIDI → synth → audio. Ears and a voice.
