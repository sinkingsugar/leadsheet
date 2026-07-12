//! M4 acceptance: legibility becomes real — chord symbols, key detection,
//! drum lanes, key-aware spelling — while the roundtrip stays green.

use leadsheet_core::grid::{QNote, QSong, QTrack, QuantizeOptions};
use leadsheet_core::{emit, ingest, key, metrics, parse, render};

fn n(pitch: u8, cell: u32, dur: u32) -> QNote {
    QNote::from_cells(pitch, cell, dur, 96)
}

/// A little band: piano comping canonical triads (chord mode), a spread
/// voicing bar (must stay melodic), drums, and a melody.
fn band_song(key_name: Option<&str>) -> QSong {
    let mut piano = Vec::new();
    // Bar 1: Am, held whole bar (canonical root position at octave 3).
    for p in [57, 60, 64] {
        piano.push(n(p, 0, 16));
    }
    // Bar 2: F/A for 2 beats (first inversion), then G for 2 beats.
    for p in [57, 60, 65] {
        piano.push(n(p, 16, 8));
    }
    for p in [55, 59, 62] {
        piano.push(n(p, 24, 8));
    }
    // Bar 3: spread Am voicing (A2 C4 E4) — NOT nameable, stays melodic.
    for p in [45, 60, 64] {
        piano.push(n(p, 32, 16));
    }
    // Bar 4: G7 on beat 1, rest of the bar silent.
    for p in [55, 59, 62, 65] {
        piano.push(n(p, 48, 4));
    }

    let mut drums = Vec::new();
    for bar in 0..4u32 {
        let base = bar * 16;
        for c in [0, 8] {
            drums.push(n(36, base + c, 1));
        }
        for c in [4, 12] {
            drums.push(n(38, base + c, 1));
        }
        for i in 0..8 {
            drums.push(n(42, base + i * 2, 1));
        }
    }

    let melody =
        vec![n(69, 0, 4), n(72, 4, 4), n(76, 8, 8), n(77, 16, 8), n(76, 24, 8), n(69, 32, 16)];

    QSong {
        name: "band".into(),
        bpm: 100.0,
        meter: (4, 4),
        key: key_name.map(|k| key::Key::parse(k).unwrap()),
        swing: None,
        n_bars: 4,
        tracks: vec![
            QTrack { name: "piano".into(), program: 0, is_drums: false, notes: piano },
            QTrack { name: "drums".into(), program: 0, is_drums: true, notes: drums },
            QTrack { name: "lead".into(), program: 81, is_drums: false, notes: melody },
        ],
    }
}

type Structural =
    Vec<(String, Vec<(u8, leadsheet_core::grid::MusicalTime, leadsheet_core::grid::MusicalTime)>)>;

fn structural(q: &QSong) -> Structural {
    q.tracks
        .iter()
        .map(|t| {
            let mut ns: Vec<_> = t.notes.iter().map(|x| (x.pitch, x.onset, x.dur)).collect();
            ns.sort_unstable();
            (t.name.clone(), ns)
        })
        .collect()
}

#[test]
fn chords_drums_and_key_in_emitted_text() {
    let q = band_song(Some("Am"));
    let text = emit::emit(&q);
    assert!(text.contains("key: Am"), "{text}");
    assert!(text.contains("piano* | Am . . . |"), "bar 1 chord mode:\n{text}");
    assert!(text.contains("piano* | F/A . G . |"), "inversion + change:\n{text}");
    assert!(text.contains("piano* | G7 z z z |"), "one-beat hit then rests:\n{text}");
    // Spread voicing must stay melodic (a bracket tuple, no chord name).
    assert!(text.contains("[A,,CE]16"), "spread voicing stays as pitches:\n{text}");
    // Drum lanes.
    assert!(text.contains("K |x... .... x... ....|"), "kick lane:\n{text}");
    assert!(text.contains("S |.... x... .... x...|"), "snare lane:\n{text}");
    assert!(text.contains("h |x.x. x.x. x.x. x.x.|"), "hat lane:\n{text}");
}

#[test]
fn m4_format_is_lossless_and_canonical() {
    let q = band_song(Some("Am"));
    let text = emit::emit(&q);
    let q2 = parse::parse(&text).unwrap();
    assert_eq!(structural(&q2), structural(&q), "text:\n{text}");
    assert_eq!(q2.key, q.key);
    assert_eq!(emit::emit(&q2), text, "second generation byte-identical");
}

#[test]
fn full_roundtrip_stays_green_with_chords_and_drums() {
    let q = band_song(None);
    let midi = render::render(&q);
    let song = ingest::ingest_midi(&midi, "band").unwrap();
    let report = metrics::roundtrip(&song, &QuantizeOptions::default()).unwrap();
    assert_eq!(report.f1.f1(), 1.0, "text:\n{}", report.text);
}

#[test]
fn key_detection_finds_am_and_eb() {
    // Quantize path: key comes back from the pitch content.
    let q = band_song(None);
    let midi = render::render(&q);
    let song = ingest::ingest_midi(&midi, "band").unwrap();
    let (qsong, _) = leadsheet_core::grid::quantize(&song, &QuantizeOptions::default());
    let detected = qsong.key.expect("key detected");
    assert!(
        detected == key::Key::parse("Am").unwrap() || detected == key::Key::parse("C").unwrap(),
        "A-minor material read as {}",
        detected.name()
    );

    // Eb major scale content spells with flats.
    let notes: Vec<QNote> = [63u8, 65, 67, 68, 70, 72, 74, 75]
        .iter()
        .enumerate()
        .map(|(i, &p)| n(p, i as u32 * 2, 2))
        .collect();
    let q = QSong {
        name: "flat".into(),
        bpm: 90.0,
        meter: (4, 4),
        key: None,
        swing: None,
        n_bars: 1,
        tracks: vec![QTrack { name: "p".into(), program: 0, is_drums: false, notes }],
    };
    let midi = render::render(&q);
    let song = ingest::ingest_midi(&midi, "flat").unwrap();
    let (qsong, _) = leadsheet_core::grid::quantize(&song, &QuantizeOptions::default());
    assert_eq!(qsong.key.unwrap().name(), "Eb");
    let text = emit::emit(&qsong);
    assert!(text.contains("_E"), "flat spelling in melodic tokens:\n{text}");
    assert!(!text.contains("^D"), "no sharp spelling in Eb:\n{text}");
}

#[test]
fn handwritten_chord_and_drum_text_renders() {
    // The Claude-authoring path: chord symbols + drum lanes from scratch.
    let text = "\
# song: sketch  tempo: 90.00  meter: 4/4  key: C  grid: 1/16
# instruments: piano:0 drums:kit

P1 piano* | C . Am7 . |
P2 drums
  K |x... .... x.x. ....|
  S |.... x... .... x..x|

arrangement:
  [P1+P2] x2
";
    let q = parse::parse(text).unwrap();
    assert_eq!(q.n_bars, 2);
    let piano = &q.tracks[0];
    // C(3) = C3 E3 G3? No: bass octave 3 → 48, 52, 55; Am7 → A3 C4 E4 G4.
    let mut pitches: Vec<u8> = piano.notes.iter().map(|x| x.pitch).collect();
    pitches.sort_unstable();
    assert_eq!(pitches.len(), 14, "3+4 pitches per bar × 2 bars");
    assert!(pitches.contains(&48) && pitches.contains(&57));
    let drums = &q.tracks[1];
    assert_eq!(drums.notes.len(), (3 + 3) * 2);
    // And it renders to valid MIDI.
    let midi = render::render(&q);
    assert!(ingest::ingest_midi(&midi, "x").is_ok());
}

#[test]
fn drum_variants_emit_as_lane_diffs() {
    // Two nearly-identical drum bars: the second must emit as `~P` with
    // only the changed lane, and survive the roundtrip.
    let mut drums = Vec::new();
    for bar in 0..2u32 {
        let base = bar * 16;
        for c in [0, 8] {
            drums.push(n(36, base + c, 1));
        }
        for c in [4, 12] {
            drums.push(n(38, base + c, 1));
        }
        for i in 0..8 {
            // Bar 2 opens the hat pattern on the last beat.
            if bar == 1 && i >= 6 {
                drums.push(n(46, base + i * 2, 1));
            } else {
                drums.push(n(42, base + i * 2, 1));
            }
        }
    }
    let q = QSong {
        name: "var".into(),
        bpm: 100.0,
        meter: (4, 4),
        key: None,
        swing: None,
        n_bars: 2,
        tracks: vec![QTrack { name: "drums".into(), program: 0, is_drums: true, notes: drums }],
    };
    let text = emit::emit(&q);
    assert!(text.contains("drums ~P1"), "variant header:\n{text}");
    // The diff must contain h and O lanes but inherit K and S.
    let variant_block: Vec<&str> = text
        .lines()
        .skip_while(|l| !l.contains("~P1"))
        .skip(1)
        .take_while(|l| l.starts_with("  "))
        .collect();
    let labels: Vec<&str> =
        variant_block.iter().map(|l| l.split_whitespace().next().unwrap()).collect();
    assert_eq!(labels, ["h", "O"], "only changed lanes in diff:\n{text}");
    let q2 = parse::parse(&text).unwrap();
    assert_eq!(structural(&q2), structural(&q), "text:\n{text}");
    assert_eq!(emit::emit(&q2), text, "canonical");
}

#[test]
fn melodic_kinship_is_informational() {
    let mk = |cells: &[(u8, u32)], bar: u32| -> Vec<QNote> {
        cells.iter().map(|&(p, c)| n(p, bar * 16 + c, 2)).collect()
    };
    let mut notes =
        mk(&[(60, 0), (64, 2), (67, 4), (64, 6), (60, 8), (64, 10), (67, 12), (64, 14)], 0);
    // Bar 2: same figure, one note changed.
    notes.extend(mk(
        &[(60, 0), (64, 2), (67, 4), (64, 6), (60, 8), (64, 10), (69, 12), (64, 14)],
        1,
    ));
    let q = QSong {
        name: "kin".into(),
        bpm: 100.0,
        meter: (4, 4),
        key: None,
        swing: None,
        n_bars: 2,
        tracks: vec![QTrack { name: "p".into(), program: 0, is_drums: false, notes }],
    };
    let text = emit::emit(&q);
    assert!(text.contains("p ~P1 |"), "kinship marker:\n{text}");
    let q2 = parse::parse(&text).unwrap();
    assert_eq!(structural(&q2), structural(&q));
    assert_eq!(emit::emit(&q2), text, "canonical");
}

#[test]
fn dynamics_emit_and_roundtrip() {
    let nv = |pitch: u8, cell: u32, dur: u32, vel: u8| QNote::from_cells(pitch, cell, dur, vel);
    let q = QSong {
        name: "dyn".into(),
        bpm: 100.0,
        meter: (4, 4),
        key: None,
        swing: None,
        n_bars: 2,
        tracks: vec![
            QTrack {
                name: "lead".into(),
                program: 81,
                is_drums: false,
                // Bar 1 is quiet (mp) with one accent and one ghost;
                // bar 2 sits at the default f — no marks expected.
                notes: vec![
                    nv(69, 0, 4, 64),
                    nv(72, 4, 4, 80), // +16 over mp → accent
                    nv(74, 8, 4, 64),
                    nv(76, 12, 4, 40), // −24 → ghost
                    nv(69, 16, 8, 96),
                    nv(72, 24, 8, 96),
                ],
            },
            QTrack {
                name: "drums".into(),
                program: 0,
                is_drums: true,
                notes: vec![
                    nv(36, 0, 1, 96),
                    nv(38, 4, 1, 112), // accent
                    nv(42, 8, 1, 96),
                    nv(38, 12, 1, 72), // ghost
                ],
            },
        ],
    };
    let text = emit::emit(&q);
    assert!(text.contains("lead@mp"), "{text}");
    assert!(text.contains(">c4"), "accent mark:\n{text}");
    assert!(text.contains("~e4"), "ghost mark:\n{text}");
    assert!(!text.contains("lead@f"), "default dynamic stays unmarked:\n{text}");
    assert!(text.contains("X..."), "drum accent:\n{text}");
    assert!(text.contains("o..."), "drum ghost:\n{text}");
    // Parse reconstructs bucketed velocities and stays canonical.
    let q2 = parse::parse(&text).unwrap();
    assert_eq!(emit::emit(&q2), text, "canonical");
    let lead = &q2.tracks[0].notes;
    assert_eq!(lead[0].vel, 64);
    assert_eq!(lead[1].vel, 80);
    assert_eq!(lead[3].vel, 40);
    let drums = &q2.tracks[1].notes;
    assert_eq!(drums[1].vel, 112);
    assert_eq!(drums[3].vel, 72);
    // And the rendered MIDI carries them.
    let midi = render::render(&q2);
    let back = ingest::ingest_midi(&midi, "x").unwrap();
    let lead_back = back.tracks.iter().find(|t| !t.is_drums).unwrap();
    assert!(lead_back.notes.iter().any(|n| n.vel == 40));
}

#[test]
fn drum_stroke_density_roundtrips() {
    // `2`/`3`/`4` lane cells: drags, triplet strokes, buzzes.
    let text = "\
# song: rolls  tempo: 110.00  meter: 4/4  grid: 1/16
# instruments: drums:kit

P1 drums
  K |x... .... x... ....|
  S |.... 2.2. .... 3344|

arrangement:
  [P1]
";
    let q = parse::parse(text).unwrap();
    let drums = &q.tracks[0];
    let strokes: Vec<(u32, u32)> = drums
        .notes
        .iter()
        .filter(|n| n.pitch == 38)
        .map(|n| (n.cell(), n.strokes as u32))
        .collect();
    assert_eq!(strokes, [(4, 2), (6, 2), (12, 3), (13, 3), (14, 4), (15, 4)]);
    assert_eq!(q.n_bars, 1, "stroke counts are not extents");
    // Canonical emit reproduces the digits.
    let text2 = emit::emit(&q);
    assert!(text2.contains("2.2."), "{text2}");
    assert!(text2.contains("3344"), "{text2}");
    assert_eq!(emit::emit(&parse::parse(&text2).unwrap()), text2);
    // Render subdivides: 2+2+3+3+4+4 = 18 snare hits + 2 kicks.
    let midi = render::render(&q);
    let back = ingest::ingest_midi(&midi, "x").unwrap();
    assert_eq!(back.note_count(), 18 + 2);
}

#[test]
fn swing_header_shifts_offbeats() {
    let text = "\
# song: shuffle  tempo: 120.00  meter: 4/4  swing: 66%  grid: 1/16
# instruments: p:0
b1 p | C4 D4 E4 F4 & z2 c2 z2 d2 z2 e2 z2 f2 |
";
    let q = parse::parse(text).unwrap();
    assert_eq!(q.swing, Some(leadsheet_core::grid::Swing { level: 8, percent: 66 }));
    // Canonical emit keeps the header field.
    let text2 = emit::emit(&q);
    assert!(text2.contains("swing: 66%"), "{text2}");
    assert_eq!(emit::emit(&parse::parse(&text2).unwrap()), text2);
    // Render: at 120 BPM a beat is 0.5 s. Downbeat notes stay put; the
    // offbeat-8th notes (cells 2,6,10,14) land at 66% through their beat.
    let midi = render::render(&q);
    let back = ingest::ingest_midi(&midi, "x").unwrap();
    let t = &back.tracks[0];
    let on_of = |pitch: u8| t.notes.iter().find(|n| n.pitch == pitch).unwrap().onset;
    assert!((on_of(60) - 0.0).abs() < 1e-3, "C4 on the beat");
    assert!((on_of(62) - 0.5).abs() < 1e-3, "D4 on beat 2");
    assert!((on_of(72) - 0.33).abs() < 0.01, "c (offbeat 8th) swung: {}", on_of(72));
    assert!((on_of(74) - 0.83).abs() < 0.01, "d swung into beat 2");

    // 16th swing form parses too.
    let q = parse::parse(
        "# song: x  tempo: 120  meter: 4/4  swing: 16th 58%  grid: 1/16\n# instruments: p:0\nb1 p | C16 |\n",
    )
    .unwrap();
    assert_eq!(q.swing, Some(leadsheet_core::grid::Swing { level: 16, percent: 58 }));
    // Out-of-range rejected.
    assert!(
        parse::parse("# song: x  tempo: 120  swing: 90%\n# instruments: p:0\nb1 p | C16 |\n")
            .is_err()
    );
}

#[test]
fn chord_holds_accumulate_duration() {
    let text = "\
# song: hold  tempo: 90.00  meter: 4/4  grid: 1/16
# instruments: p:0
b1 p* | Dm7 . . . |
b2 p* | . z G7 . |
";
    // `.` at bar 2 start with no chord before it in that bar is an error.
    assert!(parse::parse(text).is_err(), "hold cannot cross a bar line");

    let text = "\
# song: hold  tempo: 90.00  meter: 4/4  grid: 1/16
# instruments: p:0
b1 p* | Dm7 . . . |
";
    let q = parse::parse(text).unwrap();
    assert!(q.tracks[0].notes.iter().all(|x| x.dur_cells() == 16));
}

#[test]
fn fractions_and_tuplets_author_roundtrip() {
    // The 3b authoring surface: 32nds, a dotted figure, a triplet, and a
    // tied tuplet — written by hand, parsed, rendered, canonicalized.
    let text = "\
# song: frac  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: lead:81
b1 lead | C/2 D/2 E F2 (3 G A B)4 c8 |
b2 lead | C3/2 D/2 E2 (3 [CE] z c)4- c4 z4 |
";
    let q = parse::parse(text).unwrap();
    let notes = &q.tracks[0].notes;
    // Bar 1: C,D 32nds; E 16th; F 8th; triplet G A B; c half.
    let t = |cell_ticks: i64| leadsheet_core::grid::MusicalTime(cell_ticks);
    let find = |onset: i64| notes.iter().find(|n| n.onset == t(onset)).unwrap();
    assert_eq!(find(0).dur, t(120), "32nd");
    assert_eq!(find(120).dur, t(120));
    assert_eq!(find(960).dur, t(320), "triplet member");
    assert_eq!(find(1280).dur, t(320));
    // Bar 2: the tied tuplet's last member joins the following c4.
    let tied = notes.iter().find(|n| n.onset == t(3840 + 960 + 640)).unwrap();
    assert_eq!(tied.dur, t(320 + 960), "tuplet tie joins across the group");
    // Canonical emission is a fixpoint and keeps the group spelling.
    let text2 = emit::emit(&q);
    assert!(text2.contains("(3 G A B)4"), "{text2}");
    assert!(text2.contains("C/2"), "{text2}");
    assert!(text2.contains("C3/2"), "{text2}");
    assert_eq!(emit::emit(&parse::parse(&text2).unwrap()), text2, "canonical");
    // And it renders to valid MIDI with every note.
    let midi = render::render(&q);
    let back = ingest::ingest_midi(&midi, "frac").unwrap();
    assert_eq!(back.note_count(), notes.len());
}
