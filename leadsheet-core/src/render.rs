//! QSong → standard MIDI file bytes. Closes the loop: text the LLM wrote
//! becomes something you can hear (fluidsynth / SpessaSynth downstream).

use crate::grid::{CELLS_PER_BEAT, QSong};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

pub const PPQ: u16 = 480;
pub const TICKS_PER_CELL: u32 = PPQ as u32 / CELLS_PER_BEAT; // 120

/// Swing displacement in ticks for a note starting at `cell`.
/// 8th swing pushes offbeat 8ths (cell ≡ 2 mod 4) toward the triplet;
/// 16th swing pushes offbeat 16ths (cell ≡ 1 mod 2).
fn swing_delta(swing: Option<crate::grid::Swing>, cell: u32) -> u32 {
    let Some(sw) = swing else { return 0 };
    match sw.level {
        16 if cell % 2 == 1 => sw.percent as u32 * 2 * TICKS_PER_CELL / 100 - TICKS_PER_CELL,
        8 if cell % 4 == 2 => sw.percent as u32 * 4 * TICKS_PER_CELL / 100 - 2 * TICKS_PER_CELL,
        _ => 0,
    }
}

pub fn render(q: &QSong) -> Vec<u8> {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(PPQ))));

    // Conductor track: tempo + time signature.
    let us_per_qn = (60e6 / q.bpm).round() as u32;
    smf.tracks.push(vec![
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_qn))),
        },
        TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
                q.meter.0 as u8,
                q.meter.1.trailing_zeros() as u8,
                24,
                8,
            )),
        },
        TrackEvent { delta: u28::new(0), kind: TrackEventKind::Meta(MetaMessage::EndOfTrack) },
    ]);

    let mut next_melodic_channel = 0u8;
    for track in &q.tracks {
        let channel = if track.is_drums {
            9
        } else {
            let ch = next_melodic_channel;
            next_melodic_channel += 1;
            if next_melodic_channel == 9 {
                next_melodic_channel += 1;
            }
            ch % 16
        };
        let channel = u4::new(channel);

        // (tick, on_after_off ordering key, message)
        let mut events: Vec<(u32, u8, MidiMessage)> = Vec::with_capacity(track.notes.len() * 2);
        for n in &track.notes {
            let start = n.cell * TICKS_PER_CELL + swing_delta(q.swing, n.cell);
            // For drums, dur_cells is a stroke count: the cell subdivides
            // into that many hits (drag / triplet / buzz).
            let strokes = if track.is_drums { n.dur_cells.clamp(1, 4) } else { 1 };
            let step = TICKS_PER_CELL / strokes;
            for k in 0..strokes {
                let on = start + k * step;
                let off = if track.is_drums {
                    on + step / 2
                } else {
                    (n.cell + n.dur_cells) * TICKS_PER_CELL
                };
                events.push((
                    on,
                    1,
                    MidiMessage::NoteOn {
                        key: u7::new(n.pitch),
                        vel: u7::new(n.vel.clamp(1, 127)),
                    },
                ));
                events.push((
                    off.max(on + 1),
                    0, // offs before ons at the same tick, so repeats don't collapse
                    MidiMessage::NoteOff { key: u7::new(n.pitch), vel: u7::new(0) },
                ));
            }
        }
        events.sort_by_key(|(tick, order, msg)| {
            let key = match msg {
                MidiMessage::NoteOn { key, .. } | MidiMessage::NoteOff { key, .. } => key.as_int(),
                _ => 0,
            };
            (*tick, *order, key)
        });

        let mut ev_out = Vec::with_capacity(events.len() + 3);
        ev_out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::TrackName(track.name.as_bytes())),
        });
        ev_out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Midi {
                channel,
                message: MidiMessage::ProgramChange { program: u7::new(track.program) },
            },
        });
        let mut last_tick = 0u32;
        for (tick, _, message) in events {
            ev_out.push(TrackEvent {
                delta: u28::new(tick - last_tick),
                kind: TrackEventKind::Midi { channel, message },
            });
            last_tick = tick;
        }
        ev_out.push(TrackEvent {
            delta: u28::new(0),
            kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
        });
        smf.tracks.push(ev_out);
    }

    let mut out = Vec::new();
    smf.write_std(&mut out).expect("writing to Vec cannot fail");
    out
}
