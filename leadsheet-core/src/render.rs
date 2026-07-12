//! QSong → standard MIDI file bytes. Closes the loop: text the LLM wrote
//! becomes something you can hear (fluidsynth / SpessaSynth downstream).
//!
//! Written at 960 PPQ = [`grid::TICKS_PER_BEAT`]: 1 internal tick is
//! 1 MIDI tick, no conversion, no rounding, ever.

use crate::grid::{MusicalTime, QSong, TICKS_PER_BEAT};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{Format, Header, MetaMessage, MidiMessage, Smf, Timing, TrackEvent, TrackEventKind};

pub const PPQ: u16 = TICKS_PER_BEAT as u16;

/// Swing displacement in ticks for a note starting at `onset`. Only exact
/// grid positions swing: offbeat 8ths (half-beat) or offbeat 16ths
/// (quarter-beat), depending on the level. Render-time feel — notated
/// positions stay straight, and ±1 tick of integer truncation is fine.
fn swing_delta(swing: Option<crate::grid::Swing>, onset: MusicalTime) -> i64 {
    let Some(sw) = swing else { return 0 };
    let beat = TICKS_PER_BEAT;
    let (span, offbeat) = match sw.level {
        16 => (beat / 2, beat / 4),
        _ => (beat, beat / 2),
    };
    if onset.ticks().rem_euclid(span) == offbeat {
        sw.percent as i64 * span / 100 - offbeat
    } else {
        0
    }
}

pub fn render(q: &QSong) -> Vec<u8> {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(PPQ))));

    // Conductor track: tempo + time signature.
    let us_per_qn = (60e6 / q.bpm).round().clamp(1.0, 16_777_215.0) as u32;
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
            // 15 usable melodic channels: skip GM percussion (9) on every
            // lap of the wrap, not just the first.
            let c = next_melodic_channel % 15;
            next_melodic_channel += 1;
            if c >= 9 { c + 1 } else { c }
        };
        let channel = u4::new(channel);

        // (tick, on_after_off ordering key, message)
        let mut events: Vec<(u32, u8, MidiMessage)> = Vec::with_capacity(track.notes.len() * 2);
        for n in &track.notes {
            let start = (n.onset.ticks() + swing_delta(q.swing, n.onset)).max(0) as u32;
            // Drum stroke digits subdivide the note into that many hits
            // (drag / triplet / buzz).
            let strokes = if track.is_drums { n.strokes.clamp(1, 4) as u32 } else { 1 };
            let step = n.dur.ticks() as u32 / strokes;
            for k in 0..strokes {
                let on = start + k * step;
                // Swing shifts the whole note (B4: player-like — the
                // notated duration is preserved, even if that overlaps the
                // next straight onset).
                let off = if track.is_drums { on + step / 2 } else { start + n.dur.ticks() as u32 };
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
