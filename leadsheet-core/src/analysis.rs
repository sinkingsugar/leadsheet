//! Derived analysis views — never authoritative, never in source truth
//! (invariant 2: everything here re-derives deterministically).
//!
//! [`harmony`] is the roman-numeral chord-function view for `leadsheet
//! inspect --harmony`: real comping rarely matches the strict chord-mode
//! voicing contract, so `.ls` text keeps honest `[...]` tuples — this
//! view makes such bars *legible* by scoring each bar's duration-weighted
//! pitch-class content against chord templates and naming the best fit
//! relative to the key. Lossy by design; the text stays the truth.

use crate::grid::{MusicalTime, QSong};
use crate::key::{Key, pc_name};

/// Analysis templates — looser than [`crate::chord::QUALITIES`] (which
/// demand exact canonical voicings): these score pitch-class *content*.
const TEMPLATES: &[(&str, &[u8])] = &[
    ("", &[0, 4, 7]),
    ("m", &[0, 3, 7]),
    ("7", &[0, 4, 7, 10]),
    ("maj7", &[0, 4, 7, 11]),
    ("m7", &[0, 3, 7, 10]),
    ("dim", &[0, 3, 6]),
    ("m7b5", &[0, 3, 6, 10]),
    ("aug", &[0, 4, 8]),
];

/// One bar's best-fit harmony.
#[derive(Debug, Clone, PartialEq)]
pub struct BarHarmony {
    /// Chord symbol, key-aware spelling (`G7`, `Bbmaj7`).
    pub symbol: String,
    /// Function relative to the key (`V7`, `bVImaj7`, `iiø7`).
    pub roman: String,
}

/// Per-bar harmony over the song's non-drum content. `None` for bars
/// without harmonic material. Uses the song's key (header or detected);
/// returns all-`None` when there is none to be had.
pub fn harmony(q: &QSong) -> Vec<Option<BarHarmony>> {
    let Some(key) = q.key.or_else(|| crate::key::detect(q)) else {
        return vec![None; q.n_bars as usize];
    };
    let bar_len = q.bar_ticks();
    let mut out = Vec::with_capacity(q.n_bars as usize);
    for bar in 0..q.n_bars {
        let start = bar_len * bar as i64;
        let end = start + bar_len;
        let mut hist = [0f64; 12];
        let mut bass: Option<(MusicalTime, u8)> = None;
        for t in q.tracks.iter().filter(|t| !t.is_drums) {
            for n in &t.notes {
                let s = n.onset.max(start);
                let e = (n.onset + n.dur).min(end);
                if e <= s {
                    continue;
                }
                hist[(n.pitch % 12) as usize] += (e - s).ticks() as f64;
                if bass.is_none_or(|(_, p)| n.pitch < p) {
                    bass = Some((s, n.pitch));
                }
            }
        }
        let total: f64 = hist.iter().sum();
        if total <= 0.0 {
            out.push(None);
            continue;
        }
        // Best (root, template) by covered weight minus foreign weight;
        // a root that matches the bass note gets a nudge. Deterministic
        // tie-break: score, then template order, then root.
        let mut best: Option<(f64, usize, u8)> = None;
        for (ti, (_, ints)) in TEMPLATES.iter().enumerate() {
            for root in 0u8..12 {
                let covered: f64 = ints.iter().map(|i| hist[((root + i) % 12) as usize]).sum();
                let foreign = total - covered;
                let mut score = covered - 0.7 * foreign;
                if bass.is_some_and(|(_, p)| p % 12 == root) {
                    score += 0.1 * total;
                }
                if best.is_none_or(|(s, bt, br)| {
                    score > s + 1e-9 || (score > s - 1e-9 && (ti < bt || (ti == bt && root < br)))
                }) && covered > 0.0
                {
                    best = Some((score, ti, root));
                }
            }
        }
        let Some((_, ti, root)) = best else {
            out.push(None);
            continue;
        };
        let (suffix, _) = TEMPLATES[ti];
        let flats = key.use_flats();
        out.push(Some(BarHarmony {
            symbol: format!("{}{}", pc_name(root, flats), suffix),
            roman: roman(&key, root, suffix),
        }));
    }
    out
}

/// Roman numeral for a root pitch-class in a key, with the template's
/// quality shaping case and suffix.
fn roman(key: &Key, root: u8, suffix: &str) -> String {
    let deg = (root + 12 - key.tonic_pc) % 12;
    // Degree names per semitone offset; non-diatonic degrees take a flat
    // prefix (the usual jazz convention: bII, bIII, bV, bVI, bVII).
    const MAJOR: [&str; 12] =
        ["I", "bII", "II", "bIII", "III", "IV", "bV", "V", "bVI", "VI", "bVII", "VII"];
    // Offset 10 is the natural seventh (subtonic, G in Am -> VII);
    // offset 11 the raised leading tone (G# in Am -> #VII, so the
    // classical vii° reads #vii°). Distinct names for distinct roots.
    const MINOR: [&str; 12] =
        ["I", "bII", "II", "III", "#III", "IV", "bV", "V", "VI", "#VI", "VII", "#VII"];
    let base = if key.minor { MINOR[deg as usize] } else { MAJOR[deg as usize] };
    let minorish = matches!(suffix, "m" | "m7" | "dim" | "m7b5");
    let numeral: String = if minorish { base.to_lowercase() } else { base.to_string() };
    let tail = match suffix {
        "" | "m" => "",
        "7" | "m7" => "7",
        "maj7" => "maj7",
        "dim" => "°",
        "m7b5" => "ø7",
        "aug" => "+",
        other => other,
    };
    format!("{numeral}{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse;

    #[test]
    fn functions_in_a_minor() {
        let text = "\
# song: h  tempo: 100.00  meter: 4/4  key: Am  grid: 1/16
# instruments: piano:0 bass:33
b1 piano | [A,CE]16 |
b2 piano | [FA,C]16 & z16 |
b3 piano | [GB,D]16 |
b4 piano | [^GB,DE]8 [A,CE]8 |
b1 bass | A,,16 |
b2 bass | F,,16 |
b3 bass | G,,16 |
b4 bass | E,,8 A,,8 |
";
        let q = parse::parse(text).unwrap();
        let h: Vec<String> =
            harmony(&q).into_iter().map(|b| b.map(|b| b.roman).unwrap_or_default()).collect();
        assert_eq!(h[0], "i", "{h:?}");
        assert_eq!(h[1], "VI", "{h:?}");
        assert_eq!(h[2], "VII", "{h:?}");
        // Bar 4 is dominated by E7 (V7 in minor) resolving home.
        assert!(h[3].starts_with('V') || h[3] == "i", "{h:?}");
    }

    /// E1: every chromatic root spells a *distinct* degree in both
    /// tables — G (VII) and G# (#VII) in A minor were both "VII" once,
    /// making the raised leading tone unreadable.
    #[test]
    fn chromatic_degrees_are_distinct_in_both_tables() {
        for minor in [false, true] {
            let key = Key { tonic_pc: 9, minor }; // A / Am
            let names: Vec<String> = (0..12u8).map(|off| roman(&key, (9 + off) % 12, "")).collect();
            let unique: std::collections::HashSet<&String> = names.iter().collect();
            assert_eq!(unique.len(), 12, "minor={minor}: {names:?}");
        }
        // The classical pair, pinned: subtonic vs raised leading tone.
        let am = Key { tonic_pc: 9, minor: true };
        assert_eq!(roman(&am, 7, ""), "VII", "G major in Am");
        assert_eq!(roman(&am, 8, "dim"), "#vii°", "G#dim in Am");
    }

    #[test]
    fn silent_bars_are_none() {
        let text = "\
# song: h  tempo: 100.00  meter: 4/4  grid: 1/16
# instruments: p:0
P1 p | C16 |
arrangement:
  [P1]
  [z]
  [P1]
";
        let q = parse::parse(text).unwrap();
        let h = harmony(&q);
        assert!(h[0].is_some() && h[1].is_none() && h[2].is_some());
    }
}
