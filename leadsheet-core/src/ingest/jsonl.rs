//! MuScriptor jsonl → RawSong.
//!
//! The canonical schema (verified against muscriptor's `main.py::_event_to_dict`):
//!
//! ```text
//! {"type": "start", "pitch": 45, "start_time": 0.32, "index": 0, "instrument": "bass"}
//! {"type": "end", "end_time": 0.87, "start_event_index": 0}
//! ```
//!
//! One event per line, flushed as transcription progresses (5-second chunks),
//! so the stream can be consumed live. This parser additionally tolerates
//! nearby dialects — enum-wrapped events (`{"NoteStart": {...}}`), flat
//! pre-paired notes (`{"pitch", "start", "end", "instrument"}`), and common
//! field aliases — since the candle port may not serialize identically.
//! No velocity upstream → vel defaults to 96.

use crate::error::Error;
use crate::gm;
use crate::model::{RawNote, RawSong, RawTrack, finalize_tracks, sanitize_name};
use serde_json::Value;
use std::collections::HashMap;

const DEFAULT_VEL: u8 = 96;
/// Duration for a start event that never sees its end event.
const ORPHAN_DUR: f64 = 0.25;

pub fn ingest_jsonl(text: &str, song_name: &str) -> Result<RawSong, Error> {
    let mut pending: HashMap<u64, (u8, f64, String)> = HashMap::new(); // index -> (pitch, onset, instrument)
    let mut notes: Vec<(String, RawNote)> = Vec::new();
    let mut last_time = 0.0f64;

    for (lineno, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line)
            .map_err(|e| Error::Jsonl(format!("line {}: {e}", lineno + 1)))?;
        let (kind, body) = classify(&v);
        match kind {
            EventKind::Start => {
                let pitch = get_u8(body, &["pitch", "note", "key"])
                    .ok_or_else(|| Error::Jsonl(format!("line {}: start event without pitch", lineno + 1)))?;
                let onset = get_f64(body, &["start_time", "start", "onset", "time"])
                    .ok_or_else(|| Error::Jsonl(format!("line {}: start event without time", lineno + 1)))?;
                let inst = get_str(body, &["instrument", "inst", "track"]).unwrap_or("unknown");
                last_time = last_time.max(onset);
                match get_u64(body, &["index", "id"]) {
                    Some(idx) => {
                        pending.insert(idx, (pitch, onset, inst.to_string()));
                    }
                    None => notes.push((
                        inst.to_string(),
                        RawNote { pitch, onset, dur: ORPHAN_DUR, vel: DEFAULT_VEL },
                    )),
                }
            }
            EventKind::End => {
                let end = get_f64(body, &["end_time", "end", "time"])
                    .ok_or_else(|| Error::Jsonl(format!("line {}: end event without time", lineno + 1)))?;
                last_time = last_time.max(end);
                let idx =
                    get_u64(body, &["start_event_index", "start_event", "start_index", "index", "id"]);
                if let Some((pitch, onset, inst)) = idx.and_then(|i| pending.remove(&i)) {
                    notes.push((
                        inst,
                        RawNote { pitch, onset, dur: (end - onset).max(1e-3), vel: DEFAULT_VEL },
                    ));
                }
                // An end we can't pair carries no pitch — nothing to salvage.
            }
            EventKind::FlatNote => {
                let pitch = get_u8(body, &["pitch", "note", "key"])
                    .ok_or_else(|| Error::Jsonl(format!("line {}: note without pitch", lineno + 1)))?;
                let onset = get_f64(body, &["start_time", "start", "onset", "time"])
                    .ok_or_else(|| Error::Jsonl(format!("line {}: note without start", lineno + 1)))?;
                let dur = get_f64(body, &["duration", "dur"])
                    .or_else(|| get_f64(body, &["end_time", "end"]).map(|e| e - onset))
                    .unwrap_or(ORPHAN_DUR)
                    .max(1e-3);
                let inst = get_str(body, &["instrument", "inst", "track"]).unwrap_or("unknown");
                last_time = last_time.max(onset + dur);
                notes.push((inst.to_string(), RawNote { pitch, onset, dur, vel: DEFAULT_VEL }));
            }
            EventKind::Other => {} // headers/metadata lines: skip
        }
    }

    // Starts that never ended: close them with a nominal duration.
    for (_, (pitch, onset, inst)) in pending {
        // Close at the last seen timestamp, capped so a lost end event
        // doesn't produce a drone; floor at the nominal orphan duration.
        let dur = (last_time - onset).clamp(ORPHAN_DUR, 2.0);
        notes.push((inst, RawNote { pitch, onset, dur, vel: DEFAULT_VEL }));
    }

    // Group by instrument label.
    let mut by_inst: HashMap<String, Vec<RawNote>> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    for (inst, note) in notes {
        by_inst
            .entry(inst.clone())
            .or_insert_with(|| {
                order.push(inst);
                Vec::new()
            })
            .push(note);
    }
    let tracks = order
        .into_iter()
        .map(|label| {
            let notes = by_inst.remove(&label).unwrap();
            match gm::program_for_label(&label) {
                None => RawTrack { name: sanitize_name(&label), program: 0, is_drums: true, notes },
                Some(program) => {
                    RawTrack { name: sanitize_name(&label), program, is_drums: false, notes }
                }
            }
        })
        .collect();

    Ok(RawSong { name: song_name.into(), tracks: finalize_tracks(tracks), source_bpm: None })
}

enum EventKind {
    Start,
    End,
    FlatNote,
    Other,
}

/// Figure out what a line is, unwrapping `{"NoteStart": {...}}` style tags.
fn classify(v: &Value) -> (EventKind, &Value) {
    if let Some(obj) = v.as_object() {
        // Enum-style single-key wrapper.
        if obj.len() == 1 {
            let (k, inner) = obj.iter().next().unwrap();
            if inner.is_object() {
                match norm_tag(k).as_str() {
                    "notestart" | "notestartevent" | "start" | "noteon" => {
                        return (EventKind::Start, inner);
                    }
                    "noteend" | "noteendevent" | "end" | "noteoff" => {
                        return (EventKind::End, inner);
                    }
                    "note" => return (EventKind::FlatNote, inner),
                    _ => {}
                }
            }
        }
        // Tagged with a type field.
        if let Some(tag) = obj.get("type").or_else(|| obj.get("event")).and_then(Value::as_str) {
            return match norm_tag(tag).as_str() {
                "notestart" | "notestartevent" | "start" | "noteon" => (EventKind::Start, v),
                "noteend" | "noteendevent" | "end" | "noteoff" => (EventKind::End, v),
                "note" => (EventKind::FlatNote, v),
                _ => (EventKind::Other, v),
            };
        }
        // Untagged: infer from fields.
        let has = |k: &str| obj.contains_key(k);
        if has("pitch") || has("note") || has("key") {
            if (has("end_time") || has("end") || has("duration") || has("dur"))
                && (has("start_time") || has("start") || has("onset") || has("time"))
            {
                return (EventKind::FlatNote, v);
            }
            if has("start_time") || has("start") || has("onset") {
                return (EventKind::Start, v);
            }
        }
        if has("end_time") && (has("start_event") || has("start_index")) {
            return (EventKind::End, v);
        }
    }
    (EventKind::Other, v)
}

fn norm_tag(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

fn get_f64(v: &Value, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|k| v.get(k)).and_then(Value::as_f64)
}

fn get_u64(v: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|k| v.get(k)).and_then(Value::as_u64)
}

fn get_u8(v: &Value, keys: &[&str]) -> Option<u8> {
    get_u64(v, keys).and_then(|n| u8::try_from(n).ok()).filter(|&p| p <= 127)
}

fn get_str<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(k)).and_then(Value::as_str)
}
