# leadsheet

**MIDI ↔ compact, semantically meaningful text that an LLM can read,
critique, edit, and compose in.** A lead sheet, not a zip file.

The loop, both directions, working today:

```
audio → MuScriptor → .mid/.jsonl → leadsheet compress → text
text → Claude reads / writes / edits → leadsheet render → .mid → synth → ears
```

## Status (July 2026)

Core pipeline complete (original milestones M0–M5) plus the first
authoring-expression layer: dynamics, drum stroke subdivision, swing.
Battle-tested on a real 3.5k-note MuScriptor transcription (roundtrip
F1 0.9997) and on pieces composed directly as text.

## Map

| Where | What |
|---|---|
| `FORMAT.md` | **The format spec.** Paste it alongside a `.ls` when prompting an LLM. |
| `leadsheet-core/` | Library. `ingest` (.mid + MuScriptor jsonl) → `tempo`/`grid` (beat inference, quantization) → `chord`/`key`/`drums`/`notation` (semantics) → `pattern`/`emit` (text out) and `parse`/`render` (text in → MIDI). `metrics` is the oracle. |
| `leadsheet-cli/` | `leadsheet compress | render | roundtrip | inspect` |

```
leadsheet compress  in.mid  -o out.ls    # or MuScriptor .jsonl (streamable)
leadsheet render    out.ls  -o back.mid
leadsheet roundtrip in.mid               # F1 + ratio; exits nonzero < 0.95
fluidsynth -ni -F out.wav -r 44100 <soundfont.sf2> back.mid
```

Good soundfont: `~/Downloads/Arachno.sf2`. Fallback: MuseScore_General
from the MuScriptor HF cache.

## Invariants — do not break

1. **The roundtrip oracle stays green.** `cargo test` end to end;
   `metrics::roundtrip` is the correctness definition (note F1 on
   instrument/pitch/onset, ±1 cell).
2. **Emission is canonical**: `emit(parse(emit(q))) == emit(q)` byte-for-byte.
   Anything derived (section labels, variants, dynamics marks) must
   re-derive deterministically.
3. **Never lose data to a wrong name.** Chord symbols only when the canonical
   voicing reconstructs exactly; everything else stays explicit pitches.
4. **Format changes get discussed first.** The format is the product —
   and its users are LLMs: new syntax is chosen by measured model
   performance (bake-off process in PLAN.md), not taste. Gio arbitrates
   scope and invariants, not spelling.

## Working notes

- Tempo declarations lie (live takes on a default click, transcriber
  output): the auto-switch in `grid::quantize` handles it; `--no-infer-tempo`
  trusts the file.
- MuScriptor emits no velocity (all 100) and a fake tempo — its `.ls`
  output legitimately has no dynamics marks.
- Composing for playback: keep the kit at one base dynamic and shape with
  `o`/`x`/`X` inside bars (pattern-level jumps sound like fader moves);
  use `4` (64th buzzes) sparingly at speed; swing ~56% is a lilt,
  66% is a full shuffle.

## Next / deferred

- Melodic 32nds (`/` duration fractions): needs the internal clock moved
  from 16th-cells to 32nd-units first. Drum subdivision shipped without it
  (drum `dur_cells` is a stroke count).
- Per-track swing override (drums shuffle, pads straight) — floated,
  undecided.
- Analysis-grade chord view (roman numerals over real comping), BPE motif
  discovery, µtiming sidecar.
- Velocity re-estimation from audio lives in the MuScriptor candle port,
  not here; the format slot (`@dyn`, `>`/`~`, `X`/`o`) is ready.

## Upstream (optional)

leadsheet has no dependency on [MuScriptor](https://github.com/muscriptor/muscriptor)
(Kyutai/Mirelo) — it just understands its jsonl output as one ingest
format, verified against `main.py::_event_to_dict`
(`{"type":"start"|"end", ..., "start_event_index"}`), streamed per 5 s
chunk — the hook for a future live mode. Any MIDI source works.
