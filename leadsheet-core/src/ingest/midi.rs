//! .mid → RawSong. Converts tick timing to absolute seconds via the file's
//! tempo map; notes are grouped per (track, channel) so format-0 files split
//! correctly.

use crate::error::Error;
use crate::gm;
use crate::model::{RawNote, RawSong, RawTrack, finalize_tracks, sanitize_name};
use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};
use std::collections::HashMap;

const DEFAULT_US_PER_QN: u32 = 500_000; // 120 BPM, per SMF spec

/// Piecewise-constant tempo map: (absolute tick, µs per quarter note),
/// sorted by tick, always starting at tick 0.
struct TempoMap {
    changes: Vec<(u64, u32)>,
    ppq: f64,
}

impl TempoMap {
    fn new(mut changes: Vec<(u64, u32)>, ppq: u16) -> Self {
        changes.sort_by_key(|c| c.0); // stable: same-tick events keep file order
        let mut dedup: Vec<(u64, u32)> = Vec::with_capacity(changes.len());
        for c in changes {
            match dedup.last_mut() {
                Some(last) if last.0 == c.0 => *last = c, // last change at a tick wins
                _ => dedup.push(c),
            }
        }
        if dedup.first().is_none_or(|c| c.0 != 0) {
            dedup.insert(0, (0, DEFAULT_US_PER_QN));
        }
        TempoMap { changes: dedup, ppq: ppq as f64 }
    }

    fn tick_to_sec(&self, tick: u64) -> f64 {
        let mut sec = 0.0;
        let mut prev_tick = 0u64;
        let mut us_per_qn = self.changes[0].1;
        for &(t, us) in &self.changes {
            if t >= tick {
                break;
            }
            sec += (t - prev_tick) as f64 * us_per_qn as f64 / (self.ppq * 1e6);
            prev_tick = t;
            us_per_qn = us;
        }
        sec + (tick - prev_tick) as f64 * us_per_qn as f64 / (self.ppq * 1e6)
    }

    /// The single BPM if the file only ever states one tempo value.
    fn constant_bpm(&self) -> Option<f64> {
        let first = self.changes[0].1;
        self.changes
            .iter()
            .all(|&(_, us)| us == first)
            .then(|| 60e6 / first as f64)
    }
}

pub fn ingest_midi(bytes: &[u8], song_name: &str) -> Result<RawSong, Error> {
    let smf = Smf::parse(bytes).map_err(|e| Error::Midi(e.to_string()))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int(),
        Timing::Timecode(fps, sub) => {
            // Rare; fold SMPTE timing into a fake tempo map: 1 tick = 1/(fps*sub) sec.
            return ingest_timecode(&smf, fps.as_f32() as f64 * sub as f64, song_name);
        }
    };

    // Pass 1: tempo map + declared time signature across all tracks.
    let mut tempo_changes = Vec::new();
    let mut source_meter: Option<(u32, u32)> = None;
    for track in &smf.tracks {
        let mut tick = 0u64;
        for ev in track {
            tick += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::Tempo(us)) => {
                    tempo_changes.push((tick, us.as_int()));
                }
                TrackEventKind::Meta(MetaMessage::TimeSignature(num, denom_log2, _, _)) => {
                    let denom = 1u32 << denom_log2;
                    if source_meter.is_none() && num > 0 && (denom == 4 || denom == 8) {
                        source_meter = Some((num as u32, denom));
                    }
                }
                _ => {}
            }
        }
    }
    let tempo = TempoMap::new(tempo_changes, ppq);

    // Pass 2: notes.
    let mut builders: HashMap<GroupKey, GroupBuilder> = HashMap::new();
    for (track_idx, track) in smf.tracks.iter().enumerate() {
        let mut tick = 0u64;
        let mut track_name: Option<String> = None;
        // program active per channel (0..16) in this track
        let mut programs = [0u8; 16];
        // open notes: (channel, key) -> stack of (onset_sec, vel, program)
        let mut open: HashMap<(u8, u8), Vec<(f64, u8, u8)>> = HashMap::new();

        for ev in track {
            tick += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) => {
                    let n = String::from_utf8_lossy(n);
                    if !n.trim().is_empty() {
                        track_name = Some(n.trim().to_string());
                    }
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::ProgramChange { program } => {
                            programs[ch as usize] = program.as_int();
                        }
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            let sec = tempo.tick_to_sec(tick);
                            open.entry((ch, key.as_int())).or_default().push((
                                sec,
                                vel.as_int(),
                                programs[ch as usize],
                            ));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            let key = key.as_int();
                            if let Some(stack) = open.get_mut(&(ch, key))
                                && let Some((onset, vel, program)) = stack.pop()
                            {
                                let end = tempo.tick_to_sec(tick);
                                let dur = (end - onset).max(1e-3);
                                let gk = GroupKey { track: track_idx, channel: ch };
                                builders
                                    .entry(gk)
                                    .or_insert_with(|| GroupBuilder::new(track_name.clone(), program, ch == 9))
                                    .notes
                                    .push(RawNote { pitch: key, onset, dur, vel });
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        // Close any dangling notes at the last event of the track.
        let end = tempo.tick_to_sec(tick);
        for ((ch, key), stack) in open {
            for (onset, vel, program) in stack {
                let gk = GroupKey { track: track_idx, channel: ch };
                builders
                    .entry(gk)
                    .or_insert_with(|| GroupBuilder::new(track_name.clone(), program, ch == 9))
                    .notes
                    .push(RawNote { pitch: key, onset, dur: (end - onset).max(1e-3), vel });
            }
        }
        // Late TrackName meta: propagate to groups from this track that got none.
        if let Some(name) = track_name {
            for (gk, b) in builders.iter_mut() {
                if gk.track == track_idx && b.name.is_none() {
                    b.name = Some(name.clone());
                }
            }
        }
    }

    let mut keys: Vec<_> = builders.keys().copied().collect();
    keys.sort();
    let tracks = keys
        .into_iter()
        .map(|k| builders.remove(&k).unwrap().build())
        .collect();

    Ok(RawSong {
        name: song_name.to_string(),
        tracks: finalize_tracks(tracks),
        source_bpm: tempo.constant_bpm(),
        source_meter,
    })
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct GroupKey {
    track: usize,
    channel: u8,
}

struct GroupBuilder {
    name: Option<String>,
    program: u8,
    is_drums: bool,
    notes: Vec<RawNote>,
}

impl GroupBuilder {
    fn new(name: Option<String>, program: u8, is_drums: bool) -> Self {
        GroupBuilder { name, program, is_drums, notes: Vec::new() }
    }

    fn build(self) -> RawTrack {
        let name = match (&self.name, self.is_drums) {
            (Some(n), _) => sanitize_name(n),
            (None, true) => "drums".to_string(),
            (None, false) => gm::program_name(self.program).to_string(),
        };
        RawTrack { name, program: self.program, is_drums: self.is_drums, notes: self.notes }
    }
}

/// SMPTE-timed files: ticks are already wall-clock, no tempo map.
fn ingest_timecode(smf: &Smf, ticks_per_sec: f64, song_name: &str) -> Result<RawSong, Error> {
    // Reuse the metrical path by pretending 1 qn = 1 sec at the given rate.
    // These files are rare enough that a small dedicated loop keeps this honest.
    let mut tracks = Vec::new();
    for track in &smf.tracks {
        let mut tick = 0u64;
        let mut track_name: Option<String> = None;
        let mut programs = [0u8; 16];
        let mut open: HashMap<(u8, u8), Vec<(f64, u8)>> = HashMap::new();
        let mut per_channel: HashMap<u8, Vec<RawNote>> = HashMap::new();
        for ev in track {
            tick += ev.delta.as_int() as u64;
            let sec = tick as f64 / ticks_per_sec;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(n)) => {
                    track_name = Some(String::from_utf8_lossy(n).trim().to_string());
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    match message {
                        MidiMessage::ProgramChange { program } => programs[ch as usize] = program.as_int(),
                        MidiMessage::NoteOn { key, vel } if vel.as_int() > 0 => {
                            open.entry((ch, key.as_int())).or_default().push((sec, vel.as_int()));
                        }
                        MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => {
                            if let Some((onset, vel)) =
                                open.get_mut(&(ch, key.as_int())).and_then(Vec::pop)
                            {
                                per_channel.entry(ch).or_default().push(RawNote {
                                    pitch: key.as_int(),
                                    onset,
                                    dur: (sec - onset).max(1e-3),
                                    vel,
                                });
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
        for (ch, notes) in per_channel {
            let is_drums = ch == 9;
            let program = programs[ch as usize];
            let name = track_name
                .clone()
                .filter(|n| !n.is_empty())
                .map(|n| sanitize_name(&n))
                .unwrap_or_else(|| {
                    if is_drums { "drums".into() } else { gm::program_name(program).into() }
                });
            tracks.push(RawTrack { name, program, is_drums, notes });
        }
    }
    Ok(RawSong {
        name: song_name.into(),
        tracks: finalize_tracks(tracks),
        source_bpm: None,
        source_meter: None,
    })
}
