# The leadsheet text format (`.ls`)

Compact, semantically meaningful music text. Designed to be read *and
written* by an LLM, and to round-trip losslessly: `leadsheet render`
turns any valid `.ls` back into MIDI.

`.ls` aims *above* the DAW: it replaces the human editing surface — the
piano roll, the automation lanes, the drag-and-nudge — with structure an
agent authors directly in text, and leaves the audio engine downstream
(render → MIDI → synth). It is not made for humans, and legibility is
not a design constraint: when a choice is between representing more or
representing less, it represents more.

## Example

```
song: demo  tempo: 96.00  meter: 4/4  key: Am  grid: 1/16
instruments: bass:33 drums:kit piano:0 lead:81

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

- `song: NAME  tempo: BPM  meter: N/D  key: K  swing: S  grid: 1/16` —
  `key` is optional (spelling hint only, never changes pitch semantics);
  only `grid: 1/16` exists today. Meter denominator 4 or 8. `tempo` is
  spelled to hundredths of a BPM (`96.00`) — that is the format's full
  tempo precision, and finer values are rejected rather than silently
  rounded.
- `swing:` (optional): shuffle feel, applied at render time. `swing: 66%`
  pushes every offbeat 8th to 66% through its beat (50 = straight,
  66 = triplet shuffle); `swing: 16th 58%` swings the offbeat 16ths
  instead. Range 50–75%. Notation stays on the straight grid — the feel
  is a render property, like a drummer interpreting the chart. A swung
  note keeps its notated duration (the whole note shifts, as a player
  would), so it may overlap the next straight onset by the swing amount.
- `instruments: name:PROGRAM ...` — GM program number, or `kit` for the
  drum channel. Declaration order is track order. Names use letters,
  digits, `_` and `-` only.

`song:`, `instruments:`, and `bind` (see [Automation](#automation)) are
the **directives** — bare `word:` / `word ` lines that configure the
song. A line beginning with `//` is a **comment**: a durable annotation.
Comments never affect the music, but they survive formatting — each one
attaches to the next thing in the file (a directive, a pattern, an
automation lane, an arrangement row, a direct bar) and is re-emitted on
its own line immediately above it; comments after the last construct
stay at the end. Use them as margin notes: intent, reminders,
observations for the next editing pass. Two caveats: a comment written
between drum lanes floats below the block on the first reformat (lanes
can't carry comments), and comments only live in the text — rendering
to MIDI discards them, and `leadsheet fmt --strip-comments` removes
them all deliberately. Everything else is music.

## Time

Everything is measured in **grid cells** = 16th notes, with sub-cell
resolution via fractions and tuplets (below). A 4/4 bar has 16
cells (3/4 → 12, 6/8 → 12); a beat is 4 cells. The compressor emits
one-bar patterns; hand-written patterns may span several bars:
`P2 piano* | Am . . . | F . C . |` (chord holds don't cross the internal
bar lines — restate the chord).

**Meter overrides.** The header meter is the default; a pattern or
direct bar may claim another with a meter token after the instrument:
`P5 drums 3/4`, `P7 keys* 3/4 | Am . . |`, `b12 lead 6/8 | ... |`. All
bars of that pattern are in its meter, its bodies size accordingly
(a 3/4 melodic bar sums to 12 cells, a 3/4 chord bar has 3 columns),
and every pattern stacked in one arrangement row must agree on the
meter. A `[z]` row claims no meter (a direct at that bar may supply
one); unclaimed bars default to the header. Two claims on one bar must
match, or it's an error. Rendered MIDI carries a time-signature event
at every change.

## Pattern bodies — three kinds

**Melodic** — `P1 bass | A,,4 ^F2 z2 [CEG]4 |`

- Pitch: ABC convention. `C` = middle C (C4), `c` = C5, `,` = octave down,
  `'` = octave up. Accidentals are **always explicit** on the note
  (`^` sharp, `_` flat, `^^`/`__` double, `=` natural) — unlike real ABC
  there is *no* key-signature inference and nothing is sticky across a
  bar. Every token reads in isolation.
- Duration: cell count suffix, `1` implied (`e2` = an 8th, `c4` = a beat).
  Fractions of a cell as in real ABC: `e/2` = a 32nd, `e/4` = a 64th,
  `C3/2` = a dotted 16th. Lowest terms is the canonical spelling; the
  denominator must land on the underlying tick grid (2, 3, 4, 5, 6, 8, …
  work; `/7` of a single cell doesn't).
- Tuplets: `(3 C D E)4` = three notes evenly dividing 4 cells (an 8th
  triplet over the beat); `(3 e f g)2` = a 16th triplet; `(5 …)4` = a
  quintuplet; `(7 …)4` = a septuplet. Members are bare pitches, `[..]`
  chords or `z` rests — marks allowed, no durations, no nesting; the
  arity (2–24) must match the member count. The group is the semantic
  object: divisions that don't fall on whole ticks place their members
  at `round(i·span/n)` and the span always closes exactly. Tie the whole
  group with `-` after the span: `(3 C E G)4-`. Runs of equal
  triplet-length notes written as fractions (`C4/3 D4/3 E4/3`)
  canonicalize to the group form; conversely a group with trailing rests
  may respell as fractions (the rest merges with whatever silence
  follows) — timing is unchanged.
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

## Dynamics

Pattern-level base dynamic + per-note deviations:

```
P5 lead@mf | z4 d4 >f4 e2 ~d2 |
P3 drums@f
  K |X... .... x.X. ....|
  S |.... x... .... o...|
```

- `@dyn` after the instrument name: `pp`=32 `p`=48 `mp`=64 `mf`=80 `f`=96
  `ff`=112 (MIDI velocity buckets). **Unmarked = `f` (96)**, the historical
  default — MuScriptor transcriptions carry no velocity.
- Melodic/chordal notes: `>` accent (+16), `~` ghost (−24). Drum lanes:
  `X` accent, `x` normal, `o` ghost.
- Drum subdivision below the 16th grid, per cell: `2` = two 32nd strokes
  (drag/roll), `3` = triplet strokes, `4` = four 64ths (buzz). A 32nd-note
  snare roll is `S |2222 2222 2222 2222|`. Strokes play at the pattern's
  base dynamic. (Melodic sub-16th values are `/` fractions and tuplet
  groups — see Melodic above.)
- Drum tuplet groups, in a lane: `(n:span strokes)` = n strokes evenly
  dividing `span` whole cells (the ratio reads like 3:4, three against
  four). Strokes are `x` hit, `X` accent, `o` ghost, `.` silent slot —
  exactly n of them, at least one sounding; no nested digits. The group
  counts `span` cells toward the bar: an 8th-note triplet over a beat is
  `S |(3:4xxx) x... .... ....|`. Placement follows the same
  `round(i·span/n)` boundary rule as melodic groups. A one-cell group at
  uniform dynamic is the digit spelling (`(3:1xxx)` canonicalizes to
  `3`); digits get played rudiment shading (drag/ruff/buzz), wider
  groups render as written rhythm at their marked dynamics.
- Compression is lossy by bucket: each bar's base is the bucketed median
  velocity; notes ≥12 above it emit `>`, ≥16 below emit `~`. A file with
  constant velocity emits no marks at all.

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

## Automation

Continuous parameters — filter cutoff, a pitch bend, a plugin macro — are
authored as **keyframes**, the way games and animation solve millions of
events per second: name a destination, then place value points over time
and let the renderer interpolate between them. This is the surface that
replaces a DAW's automation lanes.

```
bind cutoff = cc74 [0..1]
bind lead.wobble = bend [-2..2]

P1 lead | c4 e4 g4 c4 |
  @cutoff { 0:0.2 8:1 smooth 16:0.5 }
  @wobble { 0:0 8:1 bez:0.7,0,1,1 16:0 }
```

**Binds** map a name to a destination. Song-level (`bind cutoff = cc74`)
applies to any track; instrument-scoped (`bind lead.cutoff = cc74`)
applies only on that instrument and shadows a same-named song bind
(innermost wins), so one name can mean different things on different
tracks. Targets:

- `cc0`…`cc127` — a MIDI control change (wire 0–127)
- `bend` — pitch bend, signed 14-bit −8192…8191 (0 = center)
- `at` — channel aftertouch (wire 0–127)
- `poly<note>` — polyphonic aftertouch on a MIDI note, e.g. `poly60` (wire 0–127)
- `nrpn0`…`nrpn16383` / `rpn0`…`rpn16383` — a (non-)registered parameter (wire 0–16383)
- `prog` — program (patch) change 0–127; *discrete*, so it emits at the
  keyframes only (the ease is ignored)
- `vst3:<path>` / `clap:<path>` / `osc:<path>` / `host:<path>` — an
  opaque, beyond-MIDI destination carried as intent. It has no Standard
  MIDI File form, so `render` **skips** it (an agent may rewrite the lane
  onto a MIDI target if it wants it to sound); a host that speaks the
  protocol honors it directly.

A bind may carry a `[min..max]` **value domain** (`bind cutoff = cc74
[0..1]`): lane values in that range map linearly onto the target's wire
range at render, so you can author in normalized (`0..1`) or musical
(`-2..2` semitones) units. Without a domain, values are already in wire
units. `min < max`, both on the decimal grid.

**Lanes** attach to a pattern or direct bar on the line(s) below it:
`@name { pos:value ease ... }`, in pattern-local time.

- **Position** is a grid cell — a whole 16th (`8`) or a lowest-terms
  fraction of one (`1/2`, `17/2`), exactly like a note duration. Time is
  rational and tick-exact; decimal positions (`8.5`) are rejected.
- **Value** is a decimal in the domain's units (or the target's wire
  units with no domain), snapped to 4 places. Values are analog —
  decimals, not the integer/rational grid that time lives on.
- **Ease** carries a keyframe to the next one: `lin` (straight, the
  default and omitted), `hold` (step), `smooth` (an ease-in-out S-curve),
  `exp:k` (exponential tension, `k` a nonzero decimal in ±16; `k>0` starts
  slow and accelerates, `k<0` the reverse), or `bez:x1,y1,x2,y2` (a cubic
  Bézier, CSS `cubic-bezier`; control points between `(0,0)` and `(1,1)`,
  `x`-controls in `[0,1]`, `y` may overshoot). The last keyframe eases
  nowhere and carries none.

Keyframes may sit anywhere in the body (sub-cell included) and must
strictly increase in position. At render, MIDI targets sample the eased
curve at 1/64-note resolution onto the track's channel; an NRPN selects
its parameter once, then streams 14-bit data.

## CLI

```
leadsheet compress  in.mid  -o out.ls    # or MuScriptor .jsonl
leadsheet render    out.ls  -o back.mid
leadsheet roundtrip in.mid               # F1 + compression report
leadsheet inspect   in.mid               # what would the compressor see
leadsheet check     out.ls [--json]      # validate; diagnostics carry a code,
                                         # line:col, message, and a suggestion
leadsheet fmt       out.ls               # canonical form, in place (-o / `-`);
                                         # Document-canonical: your structure
                                         # (ids, direct bars, labels) survives
leadsheet diff      a.ls b.ls            # semantic diff (per bar / per lane)
leadsheet eval      eval/                # check saved model outputs against
                                         # musical constraints
leadsheet inspect   in.mid --harmony     # derived roman-numeral view
```

Tempo handling: a declared tempo whose grid fits the onsets poorly
(live takes recorded against a default click) is auto-replaced by the
inferred one, with a notice. `--no-infer-tempo` trusts the declaration,
`--infer-tempo` forces inference, `--bpm N` forces a value. Meter is
taken from the file's time signature when present, else detected
(4/4, 3/4, 6/8).
