//! Golden corpus: paired `.mid` + `.ls` fixtures in `corpus/`. The
//! regression contract is byte-identity — `compress` (default options) of
//! each `.mid` must reproduce the committed `.ls` exactly. Any diff means
//! canonical output changed, which per invariant 4 is a discussion, not a
//! commit.
//!
//! Fixtures:
//! - `pop.mid` / `waltz.mid` / `dynamics.mid` — synthetic band pieces
//!   rendered from the builders below (declared, honest tempo).
//! - `matrix-excerpt.mid` — the first ~45 s of a real MuScriptor
//!   transcription (Gio's playing), timing preserved: the file declares
//!   120 BPM while the take is ~125, so it exercises the auto-infer path.
//!
//! Regeneration (maintainer action): `cargo test --test corpus -- --ignored`
//! rewrites the synthetic `.mid`s from the builders, re-trims the Matrix
//! excerpt when the source file is present, and refreshes every `.ls`.

use leadsheet_core::grid::{QNote, QSong, QTrack, QuantizeOptions};
use leadsheet_core::model::RawSong;
use leadsheet_core::{emit, grid, ingest, render};
use std::path::{Path, PathBuf};

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("corpus")
}

/// The compress pipeline exactly as the CLI runs it (default options).
fn compress(midi: &[u8], name: &str) -> String {
    let song = ingest::ingest_midi(midi, name).expect("fixture must ingest");
    let (qsong, _) = grid::quantize(&song, &QuantizeOptions::default());
    emit::emit(&qsong)
}

#[test]
fn corpus_is_byte_stable() {
    let dir = corpus_dir();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus/ exists") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("mid") {
            continue;
        }
        let ls_path = path.with_extension("ls");
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let midi = std::fs::read(&path).unwrap();
        let expected = std::fs::read_to_string(&ls_path)
            .unwrap_or_else(|_| panic!("missing {}", ls_path.display()));
        let got = compress(&midi, &name);
        assert_eq!(
            got, expected,
            "canonical output changed for corpus/{name}.mid — if intentional, regenerate with \
             `cargo test --test corpus -- --ignored` and review the diff"
        );
        checked += 1;
    }
    assert!(checked >= 4, "expected at least 4 corpus fixtures, found {checked}");
}

/// Every corpus `.ls` must itself be canonical (fmt is a no-op on it).
#[test]
fn corpus_ls_is_canonical() {
    for entry in std::fs::read_dir(corpus_dir()).expect("corpus/ exists") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("ls") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let q = leadsheet_core::parse::parse(&text)
            .unwrap_or_else(|e| panic!("{} must parse: {e}", path.display()));
        assert_eq!(emit::emit(&q), text, "{} is not canonical", path.display());
    }
}

// ---------------------------------------------------------------------------
// Builders (regeneration only — deterministic, no RNG).

fn n(pitch: u8, cell: u32, dur: u32) -> QNote {
    QNote::from_cells(pitch, cell, dur, 96)
}

fn nv(pitch: u8, cell: u32, dur: u32, vel: u8) -> QNote {
    QNote::from_cells(pitch, cell, dur, vel)
}

fn song(name: &str, bpm: f64, meter: (u32, u32), n_bars: u32, tracks: Vec<QTrack>) -> QSong {
    QSong {
        name: name.into(),
        bpm,
        meter,
        bar_meters: Vec::new(),
        key: None,
        swing: None,
        n_bars,
        tracks,
    }
}

fn track(name: &str, program: u8, is_drums: bool, mut notes: Vec<QNote>) -> QTrack {
    notes.sort_by(|a, b| a.onset.cmp(&b.onset).then(a.pitch.cmp(&b.pitch)));
    QTrack { name: name.into(), program, is_drums, notes }
}

/// 40 bars of verse/chorus pop: intro (bass+drums), verses add keys
/// comping Am F C G, choruses add a lead with accented peaks and an
/// open-hat drum variant, quiet outro. Exercises patterns, RLE, drum lane
/// diffs, chord mode, section labels, and minority accent/ghost marks.
fn pop() -> QSong {
    let mut bass = Vec::new();
    let mut drums = Vec::new();
    let mut keys = Vec::new();
    let mut lead = Vec::new();

    // (start, end, kind)
    let sections: &[(u32, u32, &str)] = &[
        (0, 4, "intro"),
        (4, 12, "verse"),
        (12, 20, "chorus"),
        (20, 28, "verse"),
        (28, 36, "chorus"),
        (36, 40, "outro"),
    ];
    for &(start, end, kind) in sections {
        for bar in start..end {
            let base = bar * 16;
            let root: u8 = [45, 41, 36, 43][(bar % 4) as usize]; // A F C G
            match kind {
                "outro" => bass.push(n(33, base, 16)),
                "intro" => {
                    bass.push(n(root - 12, base, 8));
                    bass.push(n(root - 12, base + 8, 8));
                }
                _ => {
                    bass.push(n(root - 12, base, 12));
                    bass.push(n(root - 5, base + 12, 4));
                }
            }
            if kind != "outro" {
                // Kick/snare with a ghost before the backbeat; 8th hats.
                for c in [0u32, 8] {
                    drums.push(nv(36, base + c, 1, 96));
                }
                drums.push(nv(38, base + 3, 1, 72)); // ghost
                for c in [4u32, 12] {
                    drums.push(nv(38, base + c, 1, 96));
                }
                for i in 0..8 {
                    // Chorus opens the hat on the last two 8ths.
                    let open = kind == "chorus" && i >= 6;
                    drums.push(nv(if open { 46 } else { 42 }, base + i * 2, 1, 96));
                }
            }
            if kind == "verse" || kind == "chorus" {
                // Canonical triads, one per bar, held: Am F C G.
                let sym = ["Am", "F", "C", "G"][(bar % 4) as usize];
                let parsed = leadsheet_core::chord::parse_symbol(sym).unwrap();
                for p in leadsheet_core::chord::voicing(&parsed).unwrap() {
                    keys.push(n(p, base, 16));
                }
            }
            if kind == "chorus" {
                const CELLS: [u32; 6] = [0, 4, 6, 8, 12, 14];
                let phrase: [u8; 6] =
                    if bar % 2 == 0 { [69, 72, 74, 76, 74, 72] } else { [71, 74, 76, 77, 76, 74] };
                for (i, (&c, p)) in CELLS.iter().zip(phrase).enumerate() {
                    // Accent the phrase peak only (minority mark).
                    let vel = if i == 3 { 112 } else { 96 };
                    lead.push(nv(p, base + c, 2, vel));
                }
            }
            if kind == "outro" {
                for p in [57u8, 60, 64] {
                    keys.push(nv(p, base, 16, 64));
                }
                drums.push(nv(51, base, 1, 64)); // soft ride taps
                drums.push(nv(51, base + 8, 1, 64));
            }
        }
    }
    song(
        "pop",
        118.0,
        (4, 4),
        40,
        vec![
            track("bass", 33, false, bass),
            track("drums", 0, true, drums),
            track("keys", 0, false, keys),
            track("lead", 81, false, lead),
        ],
    )
}

/// 24 bars of 3/4: bass on the downbeat, Eb-major piano stabs on 2 and 3
/// (chord mode in a waltz meter), brushes kit. Exercises 3/4 barring and
/// flat-side key spelling.
fn waltz() -> QSong {
    let mut bass = Vec::new();
    let mut piano = Vec::new();
    let mut drums = Vec::new();
    for bar in 0..24u32 {
        let base = bar * 12;
        let (root, sym) = [(39u8, "Eb"), (44, "Ab"), (46, "Bb"), (39, "Eb")][(bar % 4) as usize];
        bass.push(n(root - 12, base, 12));
        let parsed = leadsheet_core::chord::parse_symbol(sym).unwrap();
        let voicing = leadsheet_core::chord::voicing(&parsed).unwrap();
        for beat in [4u32, 8] {
            for &p in &voicing {
                piano.push(n(p, base + beat, 4));
            }
        }
        drums.push(nv(36, base, 1, 96));
        for beat in [4u32, 8] {
            drums.push(nv(40, base + beat, 1, 72));
        }
    }
    song(
        "waltz",
        140.0,
        (3, 4),
        24,
        vec![
            track("bass", 32, false, bass),
            track("piano", 0, false, piano),
            track("drums", 0, true, drums),
        ],
    )
}

/// 8 bars of a single line stepping through the dynamic buckets, with one
/// accent and one ghost inside otherwise-flat bars. Exercises `@dyn`
/// emission and the mark thresholds.
fn dynamics() -> QSong {
    let buckets = [32u8, 48, 64, 80, 96, 112, 96, 64];
    let mut lead = Vec::new();
    for (bar, &b) in buckets.iter().enumerate() {
        let base = bar as u32 * 16;
        let scale = [69u8, 71, 72, 74];
        for (i, &p) in scale.iter().enumerate() {
            let vel = match (bar, i) {
                (4, 1) => 112, // accent in the f bar
                (5, 2) => 88,  // ghost in the ff bar
                _ => b,
            };
            lead.push(nv(p, base + i as u32 * 4, 4, vel));
        }
    }
    song("dynamics", 100.0, (4, 4), 8, vec![track("lead", 73, false, lead)])
}

// ---------------------------------------------------------------------------
// Regeneration.

/// Rebuild an SMF from a seconds-domain song, preserving its declared
/// (possibly lying) tempo: ticks at 480 PPQ against `declared_bpm`.
fn write_raw_midi(song: &RawSong, declared_bpm: f64) -> Vec<u8> {
    use midly::num::{u4, u7, u15, u24, u28};
    use midly::{
        Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind,
    };

    let ppq = 480.0;
    let tick_of = |sec: f64| (sec * declared_bpm / 60.0 * ppq).round() as u32;
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(480))));
    let us_per_qn = (60e6 / declared_bpm).round() as u32;
    smf.tracks.push(vec![
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_qn))),
        },
        TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) },
    ]);
    for t in &song.tracks {
        let channel = u4::new(if t.is_drums { 9 } else { 0 });
        let mut events: Vec<(u32, u8, MidiMessage)> = Vec::new();
        for note in &t.notes {
            let on = tick_of(note.onset);
            let off = tick_of(note.onset + note.dur).max(on + 1);
            events.push((
                on,
                1,
                MidiMessage::NoteOn {
                    key: u7::new(note.pitch),
                    vel: u7::new(note.vel.clamp(1, 127)),
                },
            ));
            events.push((
                off,
                0,
                MidiMessage::NoteOff { key: u7::new(note.pitch), vel: u7::new(0) },
            ));
        }
        events.sort_by_key(|(tick, order, msg)| {
            let key = match msg {
                MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => key.as_int(),
                _ => 0,
            };
            (*tick, *order, key)
        });
        let mut ev_out = vec![
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Meta(MetaMessage::TrackName(t.name.as_bytes())),
            },
            TrackEvent {
                delta: u28::new(0),
                kind: TrackEventKind::Midi {
                    channel,
                    message: MidiMessage::ProgramChange { program: u7::new(t.program) },
                },
            },
        ];
        let mut last = 0u32;
        for (tick, _, message) in events {
            ev_out.push(TrackEvent {
                delta: u28::new(tick - last),
                kind: TrackEventKind::Midi { channel, message },
            });
            last = tick;
        }
        ev_out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(ev_out);
    }
    let mut out = Vec::new();
    smf.write_std(&mut out).unwrap();
    out
}

/// Where the untrimmed Matrix transcription lives (Gio's machine).
const MATRIX_SOURCE: &str = "/Users/sugar/Library/Mobile Documents/com~apple~CloudDocs/Matrix.mid";
const MATRIX_TRIM_SECS: f64 = 45.0;

#[test]
#[ignore = "regenerates corpus fixtures in place — run deliberately, then review the diff"]
fn regenerate() {
    let dir = corpus_dir();
    std::fs::create_dir_all(&dir).unwrap();
    for q in [pop(), waltz(), dynamics()] {
        let midi = render::render(&q);
        let name = q.name.clone();
        std::fs::write(dir.join(format!("{name}.mid")), &midi).unwrap();
        let ls = compress(&midi, &name);
        std::fs::write(dir.join(format!("{name}.ls")), ls).unwrap();
        eprintln!("regenerated {name}");
    }
    // The real-transcription excerpt: first MATRIX_TRIM_SECS seconds,
    // original (lying) 120 BPM declaration preserved.
    if let Ok(bytes) = std::fs::read(MATRIX_SOURCE) {
        let full = ingest::ingest_midi(&bytes, "matrix-excerpt").unwrap();
        let declared = full.source_bpm.expect("Matrix declares a (wrong) tempo");
        let mut trimmed = full;
        for t in &mut trimmed.tracks {
            t.notes.retain(|n| n.onset < MATRIX_TRIM_SECS);
            for n in &mut t.notes {
                n.dur = n.dur.min(MATRIX_TRIM_SECS + 5.0 - n.onset);
            }
        }
        trimmed.tracks.retain(|t| !t.notes.is_empty());
        let midi = write_raw_midi(&trimmed, declared);
        std::fs::write(dir.join("matrix-excerpt.mid"), &midi).unwrap();
        let ls = compress(&midi, "matrix-excerpt");
        std::fs::write(dir.join("matrix-excerpt.ls"), ls).unwrap();
        eprintln!("regenerated matrix-excerpt");
    } else {
        eprintln!("matrix source not found at {MATRIX_SOURCE}; excerpt left as committed");
    }
}
