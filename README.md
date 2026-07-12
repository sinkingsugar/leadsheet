# leadsheet

**Music as text an LLM can actually read, edit, and write.**

`leadsheet` compresses MIDI into a compact, semantically meaningful text
format — chord symbols, drum tabs, ABC-style melodies, tracker-style
patterns — and renders that text back to MIDI. A lead sheet, not a zip
file: the design goal is vocabulary a language model has deep priors on,
so it can answer "where does the chorus start?", fix bar 9, or write the
whole song.

```
# song: demo  tempo: 96.00  meter: 4/4  key: Am  swing: 56%  grid: 1/16
# instruments: bass:33 drums:kit piano:0 lead:81

P1 bass   | A,,4 A,,4 G,,4 E,,4 |
P2 drums@mf
  K |X... .... x.x. ....|
  S |.... X... .... X..o|
  h |x.o. x.o. x.o. x.o.|
P3 piano* | Am . F G7 |
P4 lead@f | e2 c2 >d2 B2 c4 A4 |

arrangement:
  intro: [P1+P2] x2
  A: [P1+P2+P3+P4] x4
```

That's a complete, renderable song: chord mode (`piano*`, one column per
beat), drum lanes with accents/ghosts (`X`/`x`/`o`) and sub-16th strokes
(`2`/`3`/`4` = drags, triplets, buzzes), melodies with explicit
accidentals and dynamics, patterns deduplicated into an arrangement with
section labels, shuffle applied at render time.

## The loop

```
audio → transcription → .mid / .jsonl ─┐
                                       ├─ leadsheet compress → .ls text
        LLM reads, critiques, edits ───┤
                                       └─ leadsheet render → .mid → synth → ears
```

## Quickstart

```sh
cargo build --release

leadsheet compress  song.mid  -o song.ls   # MIDI (or MuScriptor jsonl) → text
leadsheet render    song.ls   -o back.mid  # text → MIDI
leadsheet roundtrip song.mid               # the correctness oracle: note F1 + ratio
leadsheet inspect   song.mid               # what the compressor sees (tempo, key, meter)

fluidsynth -ni -F out.wav -r 44100 your_soundfont.sf2 back.mid
```

## What it does

- **Beat inference that doesn't trust liars.** Live takes and transcriber
  output carry fake tempo declarations; `compress` detects a poorly
  fitting grid and re-estimates tempo, phase, and downbeat from onsets
  (autocorrelation + octave disambiguation with a snare-backbeat prior),
  plus meter (4/4, 3/4, 6/8) and key (Krumhansl). On a real 3,463-note
  transcription: declared 120 BPM exposed as 124.97, roundtrip **F1 0.9997**.
- **Lossless-by-construction structure.** Identical bars share pattern
  IDs; near-identical drum bars are lane diffs (`P8 drums ~P3`); the
  arrangement is run-length-encoded bar stacks with self-similarity
  section labels. ≥10× smaller than a naive event list on structured
  material.
- **Honest chord names.** A voicing is only written as `Am`, `F/A`, `G7`
  when the canonical stacked voicing reconstructs the exact pitches;
  anything else stays explicit. Data is never lost to a wrong name.
- **An expression layer for composing**: dynamics buckets (`@pp`…`@ff`),
  accents/ghosts, drum stroke subdivision, swing, fraction durations
  (`e/2` = a 32nd, `C3/2` = a dotted 16th) and true tuplet groups
  (`(3 C D E)4`, septuplet-class divisions included) — all
  round-trippable, all documented in [FORMAT.md](FORMAT.md).

The whole text format is specified in **[FORMAT.md](FORMAT.md)** — short
enough to paste into an LLM prompt next to a song, which is the intended
use.

## Scope

leadsheet is the **compiler and semantic IR** for music-as-text: the
format, parse/emit, the Document AST, quantization and inference,
render, metrics, diagnostics, semantic diff, derived analysis views.
It is deliberately *not* a DAW, a collaboration platform, a plugin
host, or an audio engine — hosts do that; this crate hands them a
trustworthy AST and canonical text.

## Status

Working end to end; format may still evolve. Rust workspace:
`leadsheet-core` (library) + `leadsheet` CLI. `cargo test` runs the full
acceptance suite, including synthetic-band tempo/meter recovery and the
roundtrip oracle. See [CLAUDE.md](CLAUDE.md) for architecture notes,
invariants, and the roadmap.

## License

[BSD-3-Clause](LICENSE).
