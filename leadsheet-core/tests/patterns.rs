//! M3 acceptance: pattern dedup is lossless by construction and makes
//! compression real (plan target: ≥10x vs naive event list on typical
//! pop/rock structure).

use leadsheet_core::grid::{QNote, QSong, QTrack, QuantizeOptions};
use leadsheet_core::{emit, ingest, metrics, parse, render};

/// 68 bars of verse/chorus pop structure: intro (bass+drums), verse x16,
/// chorus x16 (adds lead), verse, chorus, 4 silent bars, outro chord.
fn structured_song() -> QSong {
    let n = |pitch: u8, cell: u32, dur: u32| QNote { pitch, cell, dur_cells: dur, vel: 96 };
    let mut bass = Vec::new();
    let mut drums = Vec::new();
    let mut lead = Vec::new();

    let sections: &[(u32, u32, &str)] = &[
        (0, 4, "intro"),
        (4, 20, "verse"),
        (20, 36, "chorus"),
        (36, 52, "verse"),
        (52, 60, "chorus"),
        // bars 60..64 silent
        (64, 68, "outro"),
    ];
    for &(start, end, kind) in sections {
        for bar in start..end {
            let base = bar * 16;
            // Bass: verse walks A minor, chorus pumps roots, intro/outro drone.
            match kind {
                "intro" | "outro" => bass.push(n(33, base, 16)),
                "verse" => {
                    let root = [45, 43, 41, 40][(bar % 4) as usize];
                    bass.push(n(root, base, 8));
                    bass.push(n(root - 12, base + 8, 8));
                }
                _ => {
                    for beat in 0..4 {
                        bass.push(n(45, base + beat * 4, 4));
                    }
                }
            }
            // Drums everywhere except the outro (one-shots: dur 1).
            if kind != "outro" {
                for (c, p) in [(0u32, 36u8), (8, 36), (4, 38), (12, 38)] {
                    drums.push(n(p, base + c, 1));
                }
                for i in 0..8 {
                    drums.push(n(42, base + i * 2, 1));
                }
            }
            // Lead only in the chorus, one 2-bar phrase.
            if kind == "chorus" {
                const CELLS: [u32; 6] = [0, 4, 6, 8, 12, 14];
                let phrase: [u8; 6] =
                    if bar % 2 == 0 { [69, 72, 74, 76, 74, 72] } else { [71, 74, 76, 77, 76, 74] };
                for (c, p) in CELLS.iter().zip(phrase) {
                    lead.push(n(p, base + c, 2));
                }
            }
        }
    }

    QSong {
        name: "structured".into(),
        bpm: 120.0,
        meter: (4, 4),
        key: None,
        n_bars: 68,
        tracks: vec![
            QTrack { name: "bass".into(), program: 33, is_drums: false, notes: bass },
            QTrack { name: "drums".into(), program: 0, is_drums: true, notes: drums },
            QTrack { name: "lead".into(), program: 81, is_drums: false, notes: lead },
        ],
    }
}

fn structural(q: &QSong) -> Vec<(String, Vec<(u8, u32, u32)>)> {
    q.tracks
        .iter()
        .map(|t| {
            let mut ns: Vec<_> = t.notes.iter().map(|n| (n.pitch, n.cell, n.dur_cells)).collect();
            ns.sort_unstable();
            (t.name.clone(), ns)
        })
        .collect()
}

#[test]
fn dedup_is_lossless_and_canonical() {
    let q = structured_song();
    let text = emit::emit(&q);
    assert!(text.contains("\narrangement:\n"), "text:\n{text}");
    assert!(text.contains("] x"), "RLE rows expected:\n{text}");
    let q2 = parse::parse(&text).unwrap();
    assert_eq!(structural(&q2), structural(&q));
    assert_eq!(q2.n_bars, q.n_bars, "silent bars preserved via [z] rows");
    assert_eq!(emit::emit(&q2), text, "second generation must be byte-identical");
}

#[test]
fn pattern_count_stays_small() {
    // 68 bars × 3 instruments collapse to a handful of distinct bars.
    let text = emit::emit(&structured_song());
    let n_patterns = text.lines().filter(|l| l.starts_with('P')).count();
    assert!(
        (5..=14).contains(&n_patterns),
        "expected ~9 distinct patterns, got {n_patterns}:\n{text}"
    );
}

#[test]
fn compression_hits_plan_target() {
    // Plan test #2: ≥10x vs the naive event list on pop/rock structure,
    // measured through the *real* pipeline (render → ingest → roundtrip).
    let q = structured_song();
    let midi = render::render(&q);
    let song = ingest::ingest_midi(&midi, "structured").unwrap();
    let report = metrics::roundtrip(&song, &QuantizeOptions::default()).unwrap();
    assert_eq!(report.f1.f1(), 1.0, "roundtrip stays green:\n{}", report.text);
    assert!(
        report.ratio_vs_naive() >= 10.0,
        "ratio {:.1}x < 10x ({} bytes vs {} naive)",
        report.ratio_vs_naive(),
        report.ls_bytes(),
        report.naive_bytes
    );
}

#[test]
fn arrangement_rows_parse_labels_and_reps() {
    let text = "\
# song: manual  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: bass:33
P1 bass | C,16 |
P7 bass | D,8 E,8 |
arrangement:
  intro: [P1] x2
  [z] x2
  A: [P7]
";
    let q = parse::parse(text).unwrap();
    assert_eq!(q.n_bars, 5);
    let notes = &q.tracks[0].notes;
    assert_eq!(notes.len(), 4);
    assert_eq!((notes[0].pitch, notes[0].cell, notes[0].dur_cells), (48, 0, 16));
    assert_eq!(notes[1].cell, 16);
    // Silent bars 3-4, then P7 in bar 5.
    assert_eq!((notes[2].pitch, notes[2].cell), (50, 64));
    assert_eq!((notes[3].pitch, notes[3].cell), (52, 72));
}

#[test]
fn arrangement_rejects_unknown_pattern_and_duplicates() {
    let head = "# song: x  tempo: 100  meter: 4/4\n# instruments: p:0\n";
    let unknown = format!("{head}arrangement:\n  [P3]\n");
    assert!(parse::parse(&unknown).is_err());
    let dup = format!("{head}P1 p | C16 |\nP1 p | D16 |\n");
    assert!(parse::parse(&dup).is_err());
    let bad_reps = format!("{head}P1 p | C16 |\narrangement:\n  [P1] x0\n");
    assert!(parse::parse(&bad_reps).is_err());
}

#[test]
fn tie_survives_pattern_instantiation() {
    // A pad note held across repeated pattern instances must come back as
    // one long note, not per-bar fragments.
    let text = "\
# song: pad  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: pad:89
P1 pad | C16- |
P2 pad | C16 |
arrangement:
  [P1] x3
  [P2]
";
    let q = parse::parse(text).unwrap();
    let notes = &q.tracks[0].notes;
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(notes[0].dur_cells, 64);
}
