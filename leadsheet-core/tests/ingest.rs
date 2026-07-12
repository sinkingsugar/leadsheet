//! M0 acceptance: .mid and MuScriptor jsonl land in the same event model
//! with correct absolute-seconds timing.

use leadsheet_core::ingest::{ingest_jsonl, ingest_midi};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

fn ev(delta: u32, kind: TrackEventKind<'_>) -> TrackEvent<'_> {
    TrackEvent { delta: u28::new(delta), kind }
}

fn note_on(ch: u8, key: u8, vel: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: u4::new(ch),
        message: MidiMessage::NoteOn { key: u7::new(key), vel: u7::new(vel) },
    }
}

fn note_off(ch: u8, key: u8) -> TrackEventKind<'static> {
    TrackEventKind::Midi {
        channel: u4::new(ch),
        message: MidiMessage::NoteOff { key: u7::new(key), vel: u7::new(0) },
    }
}

fn smf_bytes(smf: &Smf) -> Vec<u8> {
    let mut out = Vec::new();
    smf.write_std(&mut out).unwrap();
    out
}

#[test]
fn midi_constant_tempo() {
    // 120 BPM, ppq 480: one beat = 480 ticks = 0.5 s.
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(480))));
    smf.tracks.push(vec![
        ev(0, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000)))),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    smf.tracks.push(vec![
        ev(0, TrackEventKind::Meta(MetaMessage::TrackName(b"Bass"))),
        ev(
            0,
            TrackEventKind::Midi {
                channel: u4::new(0),
                message: MidiMessage::ProgramChange { program: u7::new(33) },
            },
        ),
        ev(0, note_on(0, 45, 100)),
        ev(480, note_off(0, 45)),
        ev(480, note_on(0, 43, 100)), // starts at tick 960 = 1.0 s
        ev(240, note_off(0, 43)),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);

    let song = ingest_midi(&smf_bytes(&smf), "t").unwrap();
    assert_eq!(song.source_bpm, Some(120.0));
    assert_eq!(song.tracks.len(), 1);
    let t = &song.tracks[0];
    assert_eq!(t.name, "bass");
    assert_eq!(t.program, 33);
    assert!(!t.is_drums);
    assert_eq!(t.notes.len(), 2);
    assert_eq!(t.notes[0].pitch, 45);
    assert!((t.notes[0].onset - 0.0).abs() < 1e-9);
    assert!((t.notes[0].dur - 0.5).abs() < 1e-9);
    assert_eq!(t.notes[1].pitch, 43);
    assert!((t.notes[1].onset - 1.0).abs() < 1e-9, "onset {}", t.notes[1].onset);
    assert!((t.notes[1].dur - 0.25).abs() < 1e-9);
}

#[test]
fn midi_tempo_change() {
    // 120 BPM for 2 beats, then 60 BPM. Note at tick 1440 → 1.0 s + 1.0 s = 2.0 s.
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(480))));
    smf.tracks.push(vec![
        ev(0, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(500_000)))),
        ev(960, TrackEventKind::Meta(MetaMessage::Tempo(u24::new(1_000_000)))),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    smf.tracks.push(vec![
        ev(1440, note_on(0, 60, 90)),
        ev(480, note_off(0, 60)),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);

    let song = ingest_midi(&smf_bytes(&smf), "t").unwrap();
    assert_eq!(song.source_bpm, None, "two tempi → not constant");
    let n = &song.tracks[0].notes[0];
    assert!((n.onset - 2.0).abs() < 1e-9, "onset {}", n.onset);
    assert!((n.dur - 1.0).abs() < 1e-9, "dur {}", n.dur);
}

#[test]
fn midi_channel_10_is_drums() {
    let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))));
    smf.tracks.push(vec![
        ev(0, note_on(9, 36, 100)),
        ev(120, note_off(9, 36)),
        ev(0, note_on(0, 60, 100)), // same track, melodic channel
        ev(480, note_off(0, 60)),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);

    let song = ingest_midi(&smf_bytes(&smf), "t").unwrap();
    assert_eq!(song.tracks.len(), 2, "format-0 track splits per channel");
    let drums = song.tracks.iter().find(|t| t.is_drums).expect("drum track");
    assert_eq!(drums.notes[0].pitch, 36);
    let melodic = song.tracks.iter().find(|t| !t.is_drums).unwrap();
    assert_eq!(melodic.notes[0].pitch, 60);
}

#[test]
fn midi_overlapping_same_pitch() {
    // Two overlapping middle Cs: LIFO pairing must not panic or drop notes.
    let mut smf = Smf::new(Header::new(Format::SingleTrack, Timing::Metrical(u15::new(480))));
    smf.tracks.push(vec![
        ev(0, note_on(0, 60, 100)),
        ev(240, note_on(0, 60, 100)),
        ev(240, note_off(0, 60)),
        ev(240, note_off(0, 60)),
        ev(0, TrackEventKind::Meta(MetaMessage::EndOfTrack)),
    ]);
    let song = ingest_midi(&smf_bytes(&smf), "t").unwrap();
    assert_eq!(song.tracks[0].notes.len(), 2);
}

#[test]
fn jsonl_start_end_events() {
    let text = r#"
{"type": "note_start", "pitch": 45, "start_time": 0.0, "index": 0, "instrument": "bass"}
{"type": "note_start", "pitch": 36, "start_time": 0.0, "index": 1, "instrument": "drums"}
{"type": "note_end", "end_time": 0.5, "start_event": 0}
{"type": "note_end", "end_time": 0.1, "start_event": 1}
{"type": "note_start", "pitch": 47, "start_time": 0.5, "index": 2, "instrument": "bass"}
{"type": "note_end", "end_time": 1.0, "start_event": 2}
"#;
    let song = ingest_jsonl(text, "t").unwrap();
    assert_eq!(song.tracks.len(), 2);
    assert_eq!(song.source_bpm, None);
    let bass = song.tracks.iter().find(|t| t.name == "bass").unwrap();
    assert_eq!(bass.program, 33);
    assert_eq!(bass.notes.len(), 2);
    assert!((bass.notes[1].onset - 0.5).abs() < 1e-9);
    assert!((bass.notes[1].dur - 0.5).abs() < 1e-9);
    let drums = song.tracks.iter().find(|t| t.name == "drums").unwrap();
    assert!(drums.is_drums);
}

#[test]
fn jsonl_canonical_muscriptor_schema() {
    // Exactly what `muscriptor transcribe --format jsonl` writes
    // (main.py::_event_to_dict): tag "start"/"end", end refers back via
    // start_event_index. Drums arrive as instrument "drums" with synthetic
    // minimum-duration ends.
    let text = r#"
{"type": "start", "pitch": 45, "start_time": 0.32, "index": 0, "instrument": "bass"}
{"type": "start", "pitch": 36, "start_time": 0.5, "index": 1, "instrument": "drums"}
{"type": "end", "end_time": 0.62, "start_event_index": 1}
{"type": "end", "end_time": 0.87, "start_event_index": 0}
"#;
    let song = ingest_jsonl(text, "t").unwrap();
    assert_eq!(song.tracks.len(), 2);
    let bass = song.tracks.iter().find(|t| t.name == "bass").unwrap();
    assert_eq!(bass.notes.len(), 1);
    assert!((bass.notes[0].onset - 0.32).abs() < 1e-9);
    assert!((bass.notes[0].dur - 0.55).abs() < 1e-9);
    assert!(song.tracks.iter().find(|t| t.name == "drums").unwrap().is_drums);
}

#[test]
fn jsonl_enum_wrapped_events() {
    let text = r#"
{"NoteStart": {"pitch": 60, "start_time": 1.0, "index": 7, "instrument": "piano"}}
{"NoteEnd": {"end_time": 1.5, "start_event": 7}}
"#;
    let song = ingest_jsonl(text, "t").unwrap();
    assert_eq!(song.tracks.len(), 1);
    let n = &song.tracks[0].notes[0];
    assert_eq!((n.pitch, n.onset, n.dur), (60, 1.0, 0.5));
}

#[test]
fn jsonl_flat_notes_and_orphans() {
    let text = r#"
{"pitch": 60, "start": 0.0, "end": 0.5, "instrument": "piano"}
{"pitch": 64, "start": 0.5, "duration": 0.25, "instrument": "piano"}
{"type": "note_start", "pitch": 67, "start_time": 0.75, "index": 3, "instrument": "piano"}
"#;
    let song = ingest_jsonl(text, "t").unwrap();
    let t = &song.tracks[0];
    assert_eq!(t.notes.len(), 3, "orphan start is kept, not dropped");
    assert!((t.notes[1].dur - 0.25).abs() < 1e-9);
    assert!(t.notes[2].dur > 0.0);
}
