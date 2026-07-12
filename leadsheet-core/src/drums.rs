//! Layer 2c — GM percussion ↔ step-grid lane labels.
//!
//! Drum patterns are written as one lane per voice (classic drum tab):
//!
//! ```text
//! P2 drums
//!   K  |x... x... x... x...|
//!   S  |.... x... .... x...|
//!   h  |x.x. x.x. x.x. x.x.|
//! ```
//!
//! The label table is injective so the roundtrip stays exact; anything not
//! in it gets a literal `dNN` lane (GM key number). Drum hits are one-shots:
//! durations are normalized to one cell at quantization.

/// Lane cell codes — the shared vocabulary between the emitter's lane
/// grids, the parser's lane reader, and hosts building
/// [`crate::doc::DrumsBody`] lanes by hand: empty / ghost / hit / accent,
/// or a multi-stroke subdivision (drag, triplet, buzz) filling the cell.
pub const LANE_EMPTY: u8 = 0;
pub const LANE_GHOST: u8 = 1;
pub const LANE_HIT: u8 = 2;
pub const LANE_ACCENT: u8 = 3;
pub const LANE_D2: u8 = 4; // two 32nd strokes
pub const LANE_D3: u8 = 5; // triplet strokes
pub const LANE_D4: u8 = 6; // four 64th strokes

/// (GM key, lane label) — display order = table order.
const LANES: &[(u8, &str)] = &[
    (36, "K"),   // acoustic kick
    (35, "K2"),  // kick 2
    (38, "S"),   // acoustic snare
    (40, "S2"),  // electric snare
    (37, "st"),  // side stick
    (39, "cp"),  // hand clap
    (42, "h"),   // closed hat
    (44, "hp"),  // pedal hat
    (46, "O"),   // open hat
    (49, "C"),   // crash 1
    (57, "C2"),  // crash 2
    (55, "Cs"),  // splash
    (52, "Cn"),  // china
    (51, "R"),   // ride 1
    (59, "R2"),  // ride 2
    (53, "rb"),  // ride bell
    (41, "T1"),  // low floor tom
    (43, "T2"),  // high floor tom
    (45, "T3"),  // low tom
    (47, "T4"),  // low-mid tom
    (48, "T5"),  // hi-mid tom
    (50, "T6"),  // high tom
    (54, "tm"),  // tambourine
    (56, "cb"),  // cowbell
    (58, "vs"),  // vibraslap
    (60, "B1"),  // hi bongo (uppercase: `b<n>` is the direct-bar prefix)
    (61, "B2"),  // low bongo
    (62, "cg1"), // mute hi conga
    (63, "cg2"), // open hi conga
    (64, "cg3"), // low conga
];

pub fn lane_label(pitch: u8) -> String {
    LANES
        .iter()
        .find(|(p, _)| *p == pitch)
        .map(|(_, l)| (*l).to_string())
        .unwrap_or_else(|| format!("d{pitch}"))
}

pub fn lane_pitch(label: &str) -> Option<u8> {
    if let Some(n) = label.strip_prefix('d')
        && let Ok(p) = n.parse::<u8>()
        && p <= 127
    {
        return Some(p);
    }
    LANES.iter().find(|(_, l)| *l == label).map(|(p, _)| *p)
}

/// Sort key for lane display: table order first, then dNN ascending.
pub fn lane_order(pitch: u8) -> usize {
    LANES.iter().position(|(p, _)| *p == pitch).unwrap_or(LANES.len() + pitch as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_injective_and_roundtrip() {
        let mut seen = std::collections::HashSet::new();
        for pitch in 0u8..=127 {
            let label = lane_label(pitch);
            assert!(seen.insert(label.clone()), "duplicate label {label}");
            assert_eq!(lane_pitch(&label), Some(pitch), "label {label}");
        }
    }

    #[test]
    fn common_names() {
        assert_eq!(lane_label(36), "K");
        assert_eq!(lane_label(42), "h");
        assert_eq!(lane_label(31), "d31");
        assert_eq!(lane_pitch("d31"), Some(31));
    }
}
