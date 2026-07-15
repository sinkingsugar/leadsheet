//! The CC automation vertical slice, end to end: `#bind` + `@lane` parse,
//! resolve to keyframed `QAuto`, render to CC events, and emit canonically
//! (the fixpoint that every format feature must satisfy).

use leadsheet_core::emit::emit_document;
use leadsheet_core::grid::Target;
use leadsheet_core::parse::{parse, parse_document};
use leadsheet_core::render::render;
use midly::{MidiMessage, Smf, TrackEventKind};

const SRC: &str = "\
# song: auto  tempo: 120.00  meter: 4/4  grid: 1/16
# instruments: lead:81
#bind cutoff = cc74

P1 lead | c4 e4 g4 c4 |
  @cutoff { 0:0 8:100 16:40 }

arrangement:
  [P1]
";

#[test]
fn bind_and_lane_emit_canonically() {
    let doc = parse_document(SRC).unwrap();
    let text = emit_document(&doc);
    // The bind and the lane survive emission verbatim...
    assert!(text.contains("#bind cutoff = cc74"), "emitted:\n{text}");
    assert!(text.contains("@cutoff { 0:0 8:100 16:40 }"), "emitted:\n{text}");
    // ...and emission is a byte-for-byte fixpoint.
    let doc2 = parse_document(&text).unwrap();
    assert_eq!(text, emit_document(&doc2), "emission is not canonical");
}

#[test]
fn hold_ease_round_trips() {
    // A `hold` ease is preserved; the trailing (meaningless) ease is dropped.
    let src = SRC.replace("@cutoff { 0:0 8:100 16:40 }", "@cutoff { 0:0 hold 8:100 16:40 hold }");
    let doc = parse_document(&src).unwrap();
    let text = emit_document(&doc);
    assert!(text.contains("@cutoff { 0:0 hold 8:100 16:40 }"), "emitted:\n{text}");
    let doc2 = parse_document(&text).unwrap();
    assert_eq!(text, emit_document(&doc2));
}

#[test]
fn lane_resolves_to_keyframes() {
    let q = parse(SRC).unwrap();
    let lead = &q.tracks[0];
    assert_eq!(lead.autos.len(), 1);
    let auto = &lead.autos[0];
    assert_eq!(auto.target, Target::Cc(74));
    let cells: Vec<i64> = auto.keys.iter().map(|(at, _, _)| at.ticks() / 240).collect();
    let vals: Vec<f64> = auto.keys.iter().map(|(_, v, _)| *v).collect();
    assert_eq!(cells, vec![0, 8, 16]);
    assert_eq!(vals, vec![0.0, 100.0, 40.0]);
}

#[test]
fn render_emits_cc_ramp() {
    let q = parse(SRC).unwrap();
    let midi = render(&q);
    let smf = midly::Smf::parse(&midi).unwrap();
    let mut cc74: Vec<u8> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let midly::TrackEventKind::Midi {
                message: midly::MidiMessage::Controller { controller, value },
                ..
            } = ev.kind
                && controller.as_int() == 74
            {
                cc74.push(value.as_int());
            }
        }
    }
    assert!(!cc74.is_empty(), "no CC74 events were rendered");
    assert_eq!(*cc74.first().unwrap(), 0, "the ramp should start at 0");
    assert!(cc74.contains(&100), "the ramp should reach the peak (100)");
    assert_eq!(*cc74.last().unwrap(), 40, "the ramp should end at 40");
    // A linear segment interpolates — more than just the three keyframes.
    assert!(cc74.len() > 3, "expected interpolated CC steps, got {cc74:?}");
}

#[test]
fn unbound_lane_is_rejected() {
    let src = SRC.replace("#bind cutoff = cc74\n", "");
    let err = parse(&src).unwrap_err();
    assert!(format!("{err:?}").contains("bound"), "expected an unbound-name error, got {err:?}");
}

#[test]
fn sub_precision_value_is_rejected() {
    let src = SRC.replace("8:100", "8:100.12345");
    assert!(parse_document(&src).is_err(), "a value finer than the decimal grid must be rejected");
}

// ---------------------------------------------------------------------------
// Helpers: reconstruct absolute-tick wire events from a rendered SMF.

/// All controller `(tick, value)` for controller `cc`, absolute ticks.
fn controllers(midi: &[u8], cc: u8) -> Vec<(u32, u8)> {
    let smf = Smf::parse(midi).unwrap();
    let mut out = Vec::new();
    for track in &smf.tracks {
        let mut t = 0u32;
        for ev in track {
            t += ev.delta.as_int();
            if let TrackEventKind::Midi {
                message: MidiMessage::Controller { controller, value },
                ..
            } = ev.kind
                && controller.as_int() == cc
            {
                out.push((t, value.as_int()));
            }
        }
    }
    out
}

/// The value in effect at `tick` for a `(tick, value)` step series.
fn active_at(series: &[(u32, u8)], tick: u32) -> Option<u8> {
    series.iter().rfind(|(t, _)| *t <= tick).map(|(_, v)| *v)
}

// ---------------------------------------------------------------------------
// Easings: smooth + exp:k parse, emit canonically, and actually bend.

#[test]
fn continuous_eases_round_trip() {
    for ease in
        ["smooth", "exp:3", "exp:-1.5", "exp:16", "bez:0.42,0,0.58,1", "bez:0.25,0.1,0.25,1"]
    {
        let src = SRC.replace("8:100", &format!("8:100 {ease}"));
        let doc = parse_document(&src).unwrap_or_else(|e| panic!("{ease}: {e}"));
        let text = emit_document(&doc);
        assert!(text.contains(&format!("8:100 {ease}")), "{ease} not emitted:\n{text}");
        let doc2 = parse_document(&text).unwrap();
        assert_eq!(text, emit_document(&doc2), "{ease} emission is not a fixpoint");
    }
}

#[test]
fn malformed_ease_is_rejected() {
    for bad in [
        "exp:0",
        "exp:0.12345",
        "exp:20",
        "exp:x",
        "exp:",
        "bez:2,0,0.5,1",       // x1 outside [0,1]
        "bez:0,0,0.5,1.00001", // y2 off the decimal grid
        "bez:0,0,1",           // too few params
        "bez:a,0,0.5,1",       // non-numeric
    ] {
        let src = SRC.replace("8:100", &format!("8:100 {bad}"));
        assert!(parse_document(&src).is_err(), "{bad} must be rejected");
    }
}

#[test]
fn bezier_renders_and_round_trips() {
    // An ease-in bezier (slow start) is concave: below linear at the quarter.
    let src = SRC.replace("@cutoff { 0:0 8:100 16:40 }", "@cutoff { 0:0 bez:0.7,0,1,1 16:120 }");
    let doc = parse_document(&src).unwrap();
    assert_eq!(emit_document(&doc), emit_document(&parse_document(&emit_document(&doc)).unwrap()));
    let cc74 = controllers(&render(&parse(&src).unwrap()), 74);
    assert_eq!(active_at(&cc74, 0), Some(0));
    let q = active_at(&cc74, 960).expect("a value at the quarter");
    assert!(q < 20, "ease-in bezier should lag (got {q} at the quarter of a 0->120 ramp)");
}

#[test]
fn exp_ease_bends_the_curve() {
    // A steep exp:3 ramp from 0 to 100 over cells 0..16 (ticks 0..3840) is
    // concave: at the midpoint it sits far below the linear 50.
    let src = SRC.replace("@cutoff { 0:0 8:100 16:40 }", "@cutoff { 0:0 exp:3 16:100 }");
    let midi = render(&parse(&src).unwrap());
    let cc74 = controllers(&midi, 74);
    assert_eq!(active_at(&cc74, 0), Some(0), "starts at 0");
    assert_eq!(cc74.last().map(|(_, v)| *v), Some(100), "ends at 100");
    let mid = active_at(&cc74, 1920).expect("a value at the midpoint");
    assert!(mid < 35, "exp:3 should be concave (got {mid} at the midpoint, linear would be ~50)");
}

// ---------------------------------------------------------------------------
// Targets beyond CC: pitch bend, channel aftertouch, NRPN, opaque extern.

/// A one-track song binding `name` to `target_text`, with `lane` keyframes.
fn one_lane(target_text: &str, lane: &str) -> String {
    format!(
        "# song: t  tempo: 120.00  meter: 4/4  grid: 1/16\n# instruments: lead:81\n#bind p = \
         {target_text}\n\nP1 lead | c4 e4 g4 c4 |\n  @p {{ {lane} }}\n\narrangement:\n  [P1]\n"
    )
}

#[test]
fn pitch_bend_renders_signed_14bit() {
    let midi = render(&parse(&one_lane("bend", "0:-8192 8:0 16:8191")).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    let mut bends: Vec<u16> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi { message: MidiMessage::PitchBend { bend }, .. } = ev.kind {
                bends.push(bend.0.as_int());
            }
        }
    }
    assert!(!bends.is_empty(), "no pitch-bend events");
    assert_eq!(*bends.first().unwrap(), 0, "min bend is wire 0");
    assert!(bends.contains(&8192), "passes through center (wire 8192)");
    assert_eq!(*bends.last().unwrap(), 16383, "max bend is wire 16383");
}

#[test]
fn channel_aftertouch_renders() {
    let midi = render(&parse(&one_lane("at", "0:0 8:127")).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    let mut at: Vec<u8> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi {
                message: MidiMessage::ChannelAftertouch { vel }, ..
            } = ev.kind
            {
                at.push(vel.as_int());
            }
        }
    }
    assert_eq!(at.first(), Some(&0));
    assert_eq!(at.last(), Some(&127));
}

#[test]
fn nrpn_selects_param_then_streams_data() {
    let midi = render(&parse(&one_lane("nrpn1000", "0:0 8:16383")).unwrap());
    // Parameter 1000 = MSB 7, LSB 104; data sweeps 0 → 16383 (MSB 127).
    assert!(controllers(&midi, 99).iter().any(|(_, v)| *v == 7), "NRPN param MSB (CC99=7)");
    assert!(controllers(&midi, 98).iter().any(|(_, v)| *v == 104), "NRPN param LSB (CC98=104)");
    let data_msb = controllers(&midi, 6);
    assert_eq!(data_msb.first().map(|(_, v)| *v), Some(0), "data starts at 0");
    assert!(data_msb.iter().any(|(_, v)| *v == 127), "data MSB reaches 127");
}

#[test]
fn extern_target_round_trips_but_renders_nothing() {
    let src = one_lane("vst3:synth/cutoff", "0:0 8:1");
    let doc = parse_document(&src).unwrap();
    let text = emit_document(&doc);
    assert!(text.contains("#bind p = vst3:synth/cutoff"), "extern bind not emitted:\n{text}");
    assert_eq!(text, emit_document(&parse_document(&text).unwrap()), "extern emission fixpoint");
    // Beyond-MIDI: no controller / bend / aftertouch events at all.
    let midi = render(&parse(&src).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi { message, .. } = ev.kind {
                assert!(
                    !matches!(
                        message,
                        MidiMessage::Controller { .. }
                            | MidiMessage::PitchBend { .. }
                            | MidiMessage::ChannelAftertouch { .. }
                    ),
                    "an extern lane must render no wire automation"
                );
            }
        }
    }
}

#[test]
fn all_target_spellings_round_trip() {
    for t in [
        "cc74",
        "bend",
        "at",
        "poly0",
        "poly127",
        "nrpn0",
        "nrpn16383",
        "rpn0",
        "rpn16383",
        "prog",
        "clap:mix",
        "osc:/fx/1",
        "host:tempo",
    ] {
        let src = one_lane(t, "0:0 8:1");
        let doc = parse_document(&src).unwrap_or_else(|e| panic!("{t}: {e}"));
        let text = emit_document(&doc);
        assert!(text.contains(&format!("#bind p = {t}")), "{t} not emitted:\n{text}");
        assert_eq!(text, emit_document(&parse_document(&text).unwrap()), "{t} fixpoint");
    }
    // Out-of-range and malformed targets are rejected.
    for t in ["cc128", "poly128", "nrpn16384", "rpn16384", "vst3:", "vst3:has space", "wiggle"] {
        assert!(parse_document(&one_lane(t, "0:0 8:1")).is_err(), "{t} must be rejected");
    }
}

#[test]
fn rpn_selects_with_101_100() {
    let midi = render(&parse(&one_lane("rpn5", "0:0 8:16383")).unwrap());
    // RPN 5 = MSB 0, LSB 5, selected on CC101/CC100; data sweeps on CC6/38.
    assert!(controllers(&midi, 101).iter().any(|(_, v)| *v == 0), "RPN param MSB (CC101=0)");
    assert!(controllers(&midi, 100).iter().any(|(_, v)| *v == 5), "RPN param LSB (CC100=5)");
    assert!(controllers(&midi, 6).iter().any(|(_, v)| *v == 127), "data MSB reaches 127");
}

#[test]
fn poly_aftertouch_targets_a_note() {
    let midi = render(&parse(&one_lane("poly60", "0:0 8:127")).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    let mut ats: Vec<(u8, u8)> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi { message: MidiMessage::Aftertouch { key, vel }, .. } =
                ev.kind
            {
                ats.push((key.as_int(), vel.as_int()));
            }
        }
    }
    assert!(!ats.is_empty(), "no poly-aftertouch events");
    assert!(ats.iter().all(|(k, _)| *k == 60), "all on note 60: {ats:?}");
    assert_eq!(ats.first().map(|(_, v)| *v), Some(0));
    assert_eq!(ats.last().map(|(_, v)| *v), Some(127));
}

#[test]
fn program_change_is_discrete() {
    // Three keyframes -> three program changes at those ticks, no ramp.
    let midi = render(&parse(&one_lane("prog", "0:0 8:64 16:127")).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    let mut progs: Vec<u8> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi {
                message: MidiMessage::ProgramChange { program }, ..
            } = ev.kind
            {
                progs.push(program.as_int());
            }
        }
    }
    // The track's own program (81) plus the three keyframe values — and
    // crucially nothing in between (discrete, not interpolated).
    assert!(progs.contains(&0) && progs.contains(&64) && progs.contains(&127), "{progs:?}");
    assert!(progs.len() <= 4, "program change must not interpolate, got {progs:?}");
}

// ---------------------------------------------------------------------------
// Fractional keyframe positions: tick-exact cell fractions, no decimals.

#[test]
fn fractional_positions_round_trip() {
    let src = one_lane("cc74", "0:0 1/2:64 1:100 17/2:20");
    let q = parse(&src).unwrap();
    let cells: Vec<i64> = q.tracks[0].autos[0].keys.iter().map(|(at, _, _)| at.ticks()).collect();
    assert_eq!(cells, vec![0, 120, 240, 2040], "1/2 cell = 120 ticks, 17/2 = 2040");
    let text = emit_document(&parse_document(&src).unwrap());
    assert!(text.contains("@p { 0:0 1/2:64 1:100 17/2:20 }"), "fractions not emitted:\n{text}");
    assert_eq!(text, emit_document(&parse_document(&text).unwrap()), "fractional fixpoint");
}

#[test]
fn decimal_position_is_rejected() {
    // Time is rational, not decimal — a decimal cell position is an error.
    assert!(parse_document(&one_lane("cc74", "0:0 0.5:64 1:100")).is_err());
    assert!(parse_document(&one_lane("cc74", "0:0 8/7:64 16:100")).is_err(), "off the tick grid");
}

// ---------------------------------------------------------------------------
// Instrument-scoped binds: innermost wins, per-track resolution.

const SCOPED: &str = "\
# song: scope  tempo: 120.00  meter: 4/4  grid: 1/16
# instruments: lead:81 pad:89
#bind cutoff = cc1
#bind lead.cutoff = cc74

P1 lead | c4 e4 g4 c4 |
  @cutoff { 0:0 16:100 }
P2 pad | [ceg]16 |
  @cutoff { 0:20 16:80 }

arrangement:
  [P1+P2]
";

#[test]
fn instrument_scope_overrides_song_scope() {
    let q = parse(SCOPED).unwrap();
    // Track 0 (lead) resolves @cutoff to its instrument bind cc74; track 1
    // (pad) has no instrument bind, so it falls back to the song bind cc1.
    let lead = &q.tracks[0];
    let pad = &q.tracks[1];
    assert_eq!(lead.name, "lead");
    assert_eq!(lead.autos[0].target, Target::Cc(74), "lead uses its instrument-scoped bind");
    assert_eq!(pad.name, "pad");
    assert_eq!(pad.autos[0].target, Target::Cc(1), "pad falls back to the song bind");
}

#[test]
fn scoped_binds_emit_canonically() {
    let text = emit_document(&parse_document(SCOPED).unwrap());
    // Both binds survive, sorted by their spelled key (cutoff < lead.cutoff).
    assert!(text.contains("#bind cutoff = cc1"), "song bind:\n{text}");
    assert!(text.contains("#bind lead.cutoff = cc74"), "scoped bind:\n{text}");
    let cutoff = text.find("#bind cutoff").unwrap();
    let scoped = text.find("#bind lead.cutoff").unwrap();
    assert!(cutoff < scoped, "binds sort by spelled key");
    assert_eq!(text, emit_document(&parse_document(&text).unwrap()), "scoped-bind fixpoint");
}

#[test]
fn lane_bound_only_on_another_instrument_is_rejected() {
    // `foo` is bound only on `lead`; a `@foo` lane on `pad` has no bind.
    let src = "\
# song: s  tempo: 120.00  meter: 4/4  grid: 1/16
# instruments: lead:81 pad:89
#bind lead.foo = cc9

P1 pad | [ceg]16 |
  @foo { 0:0 16:100 }

arrangement:
  [P1]
";
    let err = parse(src).unwrap_err();
    assert!(format!("{err:?}").contains("bound"), "expected an unbound error, got {err:?}");
}

// ---------------------------------------------------------------------------
// Value-domain remap: [min..max] maps the authored range onto wire units.

#[test]
fn domain_remaps_onto_the_cc_range() {
    // Author in 0..1; the domain scales it onto CC 0..127.
    let src = one_lane("cc74 [0..1]", "0:0 8:1");
    let doc = parse_document(&src).unwrap();
    let text = emit_document(&doc);
    assert!(text.contains("#bind p = cc74 [0..1]"), "domain not emitted:\n{text}");
    assert_eq!(text, emit_document(&parse_document(&text).unwrap()), "domain fixpoint");
    let cc74 = controllers(&render(&parse(&src).unwrap()), 74);
    assert_eq!(cc74.first().map(|(_, v)| *v), Some(0), "0.0 -> wire 0");
    assert_eq!(cc74.last().map(|(_, v)| *v), Some(127), "1.0 -> wire 127");
    assert!(cc74.iter().any(|(_, v)| (60..=68).contains(v)), "0.5 -> ~64");
}

#[test]
fn domain_remaps_a_signed_bend() {
    // Author in semitone-ish -2..2; the domain scales it across the bend.
    let midi = render(&parse(&one_lane("bend [-2..2]", "0:-2 8:0 16:2")).unwrap());
    let smf = Smf::parse(&midi).unwrap();
    let mut bends: Vec<u16> = Vec::new();
    for track in &smf.tracks {
        for ev in track {
            if let TrackEventKind::Midi { message: MidiMessage::PitchBend { bend }, .. } = ev.kind {
                bends.push(bend.0.as_int());
            }
        }
    }
    assert_eq!(bends.first(), Some(&0), "-2 -> wire 0");
    // The bend wire is asymmetric (−8192..8191), so a symmetric domain's
    // 0 lands at the linear middle ≈ 8191, not exactly center 8192.
    assert!(bends.iter().any(|&b| (8190..=8192).contains(&b)), "0 -> ~center: {bends:?}");
    assert_eq!(bends.last(), Some(&16383), "2 -> wire 16383");
}

#[test]
fn malformed_domain_is_rejected() {
    for bad in ["cc74 [1..0]", "cc74 [0..0]", "cc74 [0..1.00001]", "cc74 [0..]", "cc74 [0]"] {
        assert!(parse_document(&one_lane(bad, "0:0 8:1")).is_err(), "{bad} must be rejected");
    }
}
