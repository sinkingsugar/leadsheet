# The leadsheet text format (`.ls`)

Compact, semantically meaningful music text. Designed to be read *and
written* by an LLM, and to round-trip losslessly: `leadsheet render`
turns any valid `.ls` back into MIDI.

## Example

```
# song: demo  tempo: 96.00  meter: 4/4  key: Am  grid: 1/16
# instruments: bass:33 drums:kit piano:0 lead:81

P1 bass   | A,,4 A,,4 G,,4 E,,4 |
P2 drums
  K |x... .... x.x. ....|
  S |.... x... .... x...|
  h |x.x. x.x. x.x. x.x.|
P3 piano* | Am . F G7 |
P4 lead   | e2 c2 d2 B2 c4 A4 |

arrangement:
  intro: [P1+P2] x2
  A:     [P1+P2+P3+P4] x4
```

## Header

- `# song: NAME  tempo: BPM  meter: N/D  key: K  grid: 1/16` — `key` is
  optional (spelling hint only, never changes pitch semantics); only
  `grid: 1/16` exists today. Meter denominator 4 or 8.
- `# instruments: name:PROGRAM ...` — GM program number, or `kit` for the
  drum channel. Declaration order is track order.
- Any other `#` line is a comment.

## Time

Everything is measured in **grid cells** = 16th notes. A 4/4 bar has 16
cells (3/4 → 12, 6/8 → 12); a beat is 4 cells. The compressor emits
one-bar patterns; hand-written patterns may span several bars:
`P2 piano* | Am . . . | F . C . |` (chord holds don't cross the internal
bar lines — restate the chord).

## Pattern bodies — three kinds

**Melodic** — `P1 bass | A,,4 ^F2 z2 [CEG]4 |`

- Pitch: ABC convention. `C` = middle C (C4), `c` = C5, `,` = octave down,
  `'` = octave up. Accidentals are **always explicit** on the note
  (`^` sharp, `_` flat, `^^`/`__` double, `=` natural) — unlike real ABC
  there is *no* key-signature inference and nothing is sticky across a
  bar. Every token reads in isolation.
- Duration: cell count suffix, `1` implied (`e2` = an 8th, `c4` = a beat).
- `z2` = rest. `[CEG]4` = simultaneous notes (exact pitches, any content).
- `-` = tie into the next bar: `C8-` … next bar `C4` continues the note.
- ` & ` separates overlapping voices within a bar (sustained note under a
  moving line). Each voice must sum to a full bar.

**Chordal** — `P3 piano* | Am . F G7 |` (the `*` marks chord mode)

- One column per beat. `.` = hold previous chord, `z` = rest.
- Symbols: `C Am G7 Fmaj7 Dm7 Bdim Caug Dsus4 Esus2 Am7b5 Cdim7 C6 Am6`,
  slash bass `F/A`, octave hint `Am(2)` = bass note octave (default 3).
- A symbol means its **canonical voicing**: chord tones stacked closely
  upward from the bass note. `Am` = A3 C4 E4; `F/A` = A3 C4 F4.
  The compressor only names a voicing when this reconstruction is exact —
  anything else (doublings, spread voicings) stays melodic as `[...]`
  tuples, so no data is ever lost to a wrong chord name.

**Drums** — a `P2 drums` line, then one lane per voice:

```
P2 drums
  K |x... .... x.x. ....|
  S |.... x... .... x...|
```

- `x` hit, `.` empty; spaces inside lanes are cosmetic (beat grouping).
- Lane labels: `K` kick, `K2`, `S` snare, `S2`, `st` sidestick, `cp` clap,
  `h` closed hat, `hp` pedal hat, `O` open hat, `C`/`C2` crash, `Cs`
  splash, `Cn` china, `R`/`R2` ride, `rb` ride bell, `T1..T6` toms
  (low→high), `tm` tambourine, `cb` cowbell, `vs` vibraslap, `B1 B2`
  bongos, `cg1 cg2 cg3` congas, `dNN` = any other GM key NN.
- Drum hits are one-shots; durations are not represented.

## Variants

A pattern can declare itself a variant of an earlier one with `~P<n>`:

```
P8 drums ~P3
  h |x.x. x.x. ..x. x...|
```

For **drums** this is real inheritance: only the lanes that differ from
the base are listed (an all-dots lane silences an inherited one). For
melodic/chordal patterns `~P7` is informational kinship — the body is
still complete.

## Arrangement

```
arrangement:
  intro: [P1+P2] x2
  A: [P1+P2+P3] x8
  [z] x4
```

Each row is one bar-stack: the listed patterns play together, repeated
`xN` times (`x1` implied). With multi-bar patterns in the stack, the row
unit is the longest pattern; 1-bar patterns repeat each bar of the unit.
`[z]` = a silent bar. Labels (`intro:`, `A:`) are emitted from
self-similarity analysis (reprises reuse their letter) and ignored by the
parser — purely for reading. Bars run consecutively row by row.

## Direct bars (hand-authoring shortcut)

`b3 lead | ... |` (or `b3 drums` + lanes, `b3 piano* | ... |`) places
content directly into bar 3, no pattern/arrangement needed. Mixable with
the pattern form.

## CLI

```
leadsheet compress  in.mid  -o out.ls    # or MuScriptor .jsonl
leadsheet render    out.ls  -o back.mid
leadsheet roundtrip in.mid               # F1 + compression report
leadsheet inspect   in.mid               # what would the compressor see
```

Tempo handling: a declared tempo whose grid fits the onsets poorly
(live takes recorded against a default click) is auto-replaced by the
inferred one, with a notice. `--no-infer-tempo` trusts the declaration,
`--infer-tempo` forces inference, `--bpm N` forces a value. Meter is
taken from the file's time signature when present, else detected
(4/4, 3/4, 6/8).
