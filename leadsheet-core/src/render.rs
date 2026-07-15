//! QSong → standard MIDI file bytes. Closes the loop: text the LLM wrote
//! becomes something you can hear (fluidsynth / SpessaSynth downstream).
//!
//! Written at 960 PPQ = [`grid::TICKS_PER_BEAT`]: 1 internal tick is
//! 1 MIDI tick, no conversion, no rounding, ever.

use crate::grid::{Ease, MusicalTime, QSong, TICKS_PER_BEAT, Target};
use midly::num::{u4, u7, u15, u24, u28};
use midly::{
    Format, Header, MetaMessage, MidiMessage, PitchBend, Smf, Timing, TrackEvent, TrackEventKind,
};

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

/// Sample a keyframed automation lane into `(tick, raw_value)` points:
/// each continuous segment is warped by its ease and stepped at 1/64-note
/// resolution; `Hold` emits no interior points (the next keyframe jumps).
/// Values stay in the target's own units — the per-target mappers below
/// round, clamp and dedup to wire values, so a future `[min..max]` domain
/// remap has one place to live.
fn sample_curve(keys: &[(MusicalTime, f64, Ease)]) -> Vec<(u32, f64)> {
    const STEP: i64 = TICKS_PER_BEAT / 16; // 1/64 note
    fn push_pt(pts: &mut Vec<(u32, f64)>, tick: i64, val: f64) {
        let tick = tick.max(0) as u32;
        if let Some(&(lt, lv)) = pts.last() {
            if lv == val {
                return; // no point for an unchanged value
            }
            if lt == tick {
                pts.pop(); // keep only the latest value at a given tick
            }
        }
        pts.push((tick, val));
    }
    let mut pts: Vec<(u32, f64)> = Vec::new();
    for w in keys.windows(2) {
        let (t0, v0, ease) = w[0];
        let (t1, v1, _) = w[1];
        push_pt(&mut pts, t0.ticks(), v0);
        if ease != Ease::Hold && (v1 - v0).abs() > f64::EPSILON {
            let (a, b) = (t0.ticks(), t1.ticks());
            let mut t = a + STEP;
            while t < b {
                let frac = (t - a) as f64 / (b - a) as f64;
                push_pt(&mut pts, t, v0 + (v1 - v0) * ease.warp(frac));
                t += STEP;
            }
        }
    }
    if let Some(&(t, v, _)) = keys.last() {
        push_pt(&mut pts, t.ticks(), v);
    }
    pts
}

/// Map a raw lane value to its target's wire units: with a `[lo..hi]`
/// domain, the authored range maps linearly onto `[wlo..whi]`; without
/// one, the value is already in wire units. Rounded and clamped to the
/// wire range.
fn to_wire(v: f64, domain: Option<(f64, f64)>, wlo: f64, whi: f64) -> f64 {
    let w = match domain {
        Some((lo, hi)) => wlo + (v - lo) / (hi - lo) * (whi - wlo),
        None => v,
    };
    w.round().clamp(wlo, whi)
}

/// Emit a keyframed lane onto its target's wire events, applying the bind's
/// `[min..max]` domain and deduping consecutive identical wire values. MIDI
/// targets produce channel messages; `Extern` intents have no wire form and
/// are skipped (an LLM may rewrite them onto a MIDI target to hear them).
fn push_automation(
    events: &mut Vec<(u32, u8, MidiMessage)>,
    target: &Target,
    domain: Option<(f64, f64)>,
    keys: &[(MusicalTime, f64, Ease)],
) {
    let cc = |n: u8, v: u8| MidiMessage::Controller { controller: u7::new(n), value: u7::new(v) };
    let pts = sample_curve(keys);
    match target {
        Target::Cc(n) => {
            let mut last: Option<u8> = None;
            for (tick, v) in pts {
                let w = to_wire(v, domain, 0.0, 127.0) as u8;
                if last != Some(w) {
                    last = Some(w);
                    events.push((tick, 0, cc(*n, w)));
                }
            }
        }
        Target::ChannelPressure => {
            let mut last: Option<u8> = None;
            for (tick, v) in pts {
                let w = to_wire(v, domain, 0.0, 127.0) as u8;
                if last != Some(w) {
                    last = Some(w);
                    events.push((tick, 0, MidiMessage::ChannelAftertouch { vel: u7::new(w) }));
                }
            }
        }
        Target::PolyPressure(note) => {
            let key = u7::new(*note);
            let mut last: Option<u8> = None;
            for (tick, v) in pts {
                let w = to_wire(v, domain, 0.0, 127.0) as u8;
                if last != Some(w) {
                    last = Some(w);
                    events.push((tick, 0, MidiMessage::Aftertouch { key, vel: u7::new(w) }));
                }
            }
        }
        Target::PitchBend => {
            let mut last: Option<i16> = None;
            for (tick, v) in pts {
                let w = to_wire(v, domain, -8192.0, 8191.0) as i16;
                if last != Some(w) {
                    last = Some(w);
                    events.push((tick, 0, MidiMessage::PitchBend { bend: PitchBend::from_int(w) }));
                }
            }
        }
        Target::Nrpn(param) | Target::Rpn(param) => {
            // Select the parameter once — NRPN with CC99/98, RPN with
            // CC101/100 — then stream 14-bit data (CC6 MSB / CC38 LSB).
            let (sel_hi, sel_lo) =
                if matches!(target, Target::Rpn(_)) { (101u8, 100u8) } else { (99u8, 98u8) };
            let (pmsb, plsb) = ((param >> 7) as u8, (param & 0x7f) as u8);
            let mut last: Option<u16> = None;
            let mut selected = false;
            for (tick, v) in pts {
                let w = to_wire(v, domain, 0.0, 16383.0) as u16;
                if last == Some(w) {
                    continue;
                }
                last = Some(w);
                if !selected {
                    events.push((tick, 0, cc(sel_hi, pmsb)));
                    events.push((tick, 0, cc(sel_lo, plsb)));
                    selected = true;
                }
                events.push((tick, 0, cc(6, (w >> 7) as u8)));
                events.push((tick, 0, cc(38, (w & 0x7f) as u8)));
            }
        }
        Target::Program => {
            // Program change is discrete: emit at the keyframes only (no
            // interpolation), the value mapped and rounded to a GM program.
            let mut last: Option<u8> = None;
            for (at, val, _) in keys {
                let w = to_wire(*val, domain, 0.0, 127.0) as u8;
                if last != Some(w) {
                    last = Some(w);
                    let tick = at.ticks().max(0) as u32;
                    events.push((tick, 0, MidiMessage::ProgramChange { program: u7::new(w) }));
                }
            }
        }
        Target::Extern { .. } => {}
    }
}

pub fn render(q: &QSong) -> Vec<u8> {
    let mut smf = Smf::new(Header::new(Format::Parallel, Timing::Metrical(u15::new(PPQ))));

    // Conductor track: tempo, then a time signature at every bar where
    // the meter changes (bar starts come from the per-bar meter map).
    let us_per_qn = (60e6 / q.bpm).round().clamp(1.0, 16_777_215.0) as u32;
    let ts_event = |meter: (u32, u32), delta: u32| TrackEvent {
        delta: u28::new(delta),
        kind: TrackEventKind::Meta(MetaMessage::TimeSignature(
            meter.0 as u8,
            meter.1.trailing_zeros() as u8,
            24,
            8,
        )),
    };
    let mut conductor = vec![TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::Tempo(u24::new(us_per_qn))),
    }];
    if q.n_bars == 0 || q.bar_meters.is_empty() {
        conductor.push(ts_event(q.meter, 0));
    } else {
        let starts = q.bar_starts();
        let mut last: Option<(u32, u32)> = None;
        let mut last_tick = 0u32;
        for bar in 0..q.n_bars {
            let m = q.bar_meter(bar);
            if last != Some(m) {
                let tick = starts[bar as usize].ticks() as u32;
                conductor.push(ts_event(m, tick - last_tick));
                last_tick = tick;
                last = Some(m);
            }
        }
    }
    conductor.push(TrackEvent {
        delta: u28::new(0),
        kind: TrackEventKind::Meta(MetaMessage::EndOfTrack),
    });
    smf.tracks.push(conductor);

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

        // (tick, on_after_off ordering key, message). The u32 ticks and
        // the u28 deltas below are safe because both validates and the
        // parser bound songs to grid::MAX_SONG_TICKS (< 2^28, with
        // headroom for the swing shift and the note-off bump) — midly's
        // u28::new masks silently, so that bound is load-bearing.
        let mut events: Vec<(u32, u8, MidiMessage)> = Vec::with_capacity(track.notes.len() * 2);
        for n in &track.notes {
            let start = (n.onset.ticks() + swing_delta(q.swing, n.onset)).max(0) as u32;
            // Drum strokes subdivide the note: digits (`2`/`3`/`4`) into
            // that many hits over one cell, lane tuplet groups into n
            // members over their span, `stroke_mask` picking which
            // members sound. Members place by the DESIGN-960 boundary
            // rule — the same rounding melodic tuplets use.
            let strokes = if track.is_drums { n.strokes.clamp(1, 31) as u32 } else { 1 };
            // A one-cell full subdivision is a played rudiment (drag /
            // ruff / buzz); anything wider is written rhythm.
            let is_digit = track.is_drums
                && (2..=4).contains(&strokes)
                && n.dur == MusicalTime::from_sixteenths(1)
                && n.stroke_mask == crate::grid::full_stroke_mask(n.strokes);
            for k in 0..strokes {
                if track.is_drums && n.stroke_mask & (1u32 << k.min(31)) == 0 {
                    continue;
                }
                let b0 = crate::notation::tuplet_boundary(n.dur, strokes, k).ticks() as u32;
                let b1 = crate::notation::tuplet_boundary(n.dur, strokes, k + 1).ticks() as u32;
                let on = start + b0;
                // A drummer's strokes are not equal: a drag/ruff is soft
                // grace strokes into the tap, a buzz is a press leaning
                // forward. Equal-velocity retriggers of one sample are
                // the classic machine-gun giveaway, so the subdivision
                // shapes velocity (percent of the note's anchor vel).
                // Render interpretation, like swing — the notation stays
                // `2`/`3`/`4` and QNote.vel stays the anchor; strokes
                // are authoring expression and deliberately don't
                // survive transcription (D3), so nothing round-trips
                // through these numbers.
                const STROKE_SHAPE: [&[u16]; 3] = [
                    &[72, 100],        // drag: grace into the tap
                    &[58, 74, 100],    // ruff: two graces into the tap
                    &[62, 68, 76, 86], // buzz: pulsing press, leaning in
                ];
                let vel = if is_digit {
                    let pct = STROKE_SHAPE[strokes as usize - 2][k as usize];
                    ((n.vel as u16 * pct / 100) as u8).clamp(1, 127)
                } else {
                    n.vel.clamp(1, 127)
                };
                // Swing shifts the whole note (B4: player-like — the
                // notated duration is preserved, even if that overlaps the
                // next straight onset).
                let off =
                    if track.is_drums { on + (b1 - b0) / 2 } else { start + n.dur.ticks() as u32 };
                events.push((
                    on,
                    1,
                    MidiMessage::NoteOn { key: u7::new(n.pitch), vel: u7::new(vel) },
                ));
                events.push((
                    off.max(on + 1),
                    0, // offs before ons at the same tick, so repeats don't collapse
                    MidiMessage::NoteOff { key: u7::new(n.pitch), vel: u7::new(0) },
                ));
            }
        }
        // Automation lanes: sample each keyframed curve onto its target's
        // wire events on this track's channel. Non-MIDI targets carry no
        // wire form and are skipped; events sort before the note-ons at the
        // same tick (order 0 < 1).
        for a in &track.autos {
            push_automation(&mut events, &a.target, a.domain, &a.keys);
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
