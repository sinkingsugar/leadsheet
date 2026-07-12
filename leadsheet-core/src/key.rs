//! Layer 2a — key estimation (Krumhansl-Schmuckler) and key-aware spelling.

use crate::grid::QSong;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub tonic_pc: u8,
    pub minor: bool,
}

const SHARP_PC: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
const FLAT_PC: [&str; 12] = ["C", "Db", "D", "Eb", "E", "F", "Gb", "G", "Ab", "A", "Bb", "B"];

impl Key {
    /// Flat-side keys spell with flats: F, Bb, Eb, Ab, Db major and their
    /// relative minors. The six-accidental toss-up goes by convention:
    /// F# major (not Gb), but Eb minor (not D#m).
    pub fn use_flats(&self) -> bool {
        let rel_major = if self.minor { (self.tonic_pc + 3) % 12 } else { self.tonic_pc };
        matches!(rel_major, 5 | 10 | 3 | 8 | 1) || (rel_major == 6 && self.minor)
    }

    pub fn name(&self) -> String {
        let names = if self.use_flats() { &FLAT_PC } else { &SHARP_PC };
        format!("{}{}", names[self.tonic_pc as usize], if self.minor { "m" } else { "" })
    }

    pub fn parse(s: &str) -> Option<Key> {
        let (base, minor) = match s.strip_suffix('m') {
            Some(b) => (b, true),
            None => (s, false),
        };
        let pc = SHARP_PC
            .iter()
            .position(|n| *n == base)
            .or_else(|| FLAT_PC.iter().position(|n| *n == base))? as u8;
        Some(Key { tonic_pc: pc, minor })
    }
}

/// Krumhansl-Kessler probe-tone profiles.
const MAJOR_PROFILE: [f64; 12] =
    [6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88];
const MINOR_PROFILE: [f64; 12] =
    [6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17];

/// Duration-weighted pitch-class histogram (drums excluded) correlated
/// against all 24 rotated profiles.
pub fn detect(q: &QSong) -> Option<Key> {
    let mut hist = [0.0f64; 12];
    for t in q.tracks.iter().filter(|t| !t.is_drums) {
        for n in &t.notes {
            hist[(n.pitch % 12) as usize] += n.dur_cells as f64;
        }
    }
    if hist.iter().sum::<f64>() <= 0.0 {
        return None;
    }
    let mut best: Option<(f64, Key)> = None;
    for (minor, profile) in [(false, &MAJOR_PROFILE), (true, &MINOR_PROFILE)] {
        for tonic in 0u8..12 {
            let rotated: Vec<f64> =
                (0..12).map(|pc| profile[(pc + 12 - tonic as usize) % 12]).collect();
            let r = pearson(&hist, &rotated);
            if best.is_none_or(|(b, _)| r > b) {
                best = Some((r, Key { tonic_pc: tonic, minor }));
            }
        }
    }
    best.map(|(_, k)| k)
}

fn pearson(a: &[f64; 12], b: &[f64]) -> f64 {
    let ma = a.iter().sum::<f64>() / 12.0;
    let mb = b.iter().sum::<f64>() / 12.0;
    let (mut num, mut da, mut db) = (0.0, 0.0, 0.0);
    for i in 0..12 {
        let (xa, xb) = (a[i] - ma, b[i] - mb);
        num += xa * xb;
        da += xa * xa;
        db += xb * xb;
    }
    if da == 0.0 || db == 0.0 { 0.0 } else { num / (da * db).sqrt() }
}

/// Spell a pitch class in a key context (standalone note names, e.g. for
/// chord roots).
pub fn pc_name(pc: u8, flats: bool) -> &'static str {
    if flats { FLAT_PC[(pc % 12) as usize] } else { SHARP_PC[(pc % 12) as usize] }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_parse() {
        for s in ["C", "Am", "F#m", "Bb", "Ebm", "G"] {
            let k = Key::parse(s).unwrap();
            assert_eq!(k.name(), s);
        }
        assert_eq!(Key::parse("A#").unwrap().name(), "Bb", "enharmonic normalizes");
        assert!(Key::parse("H").is_none());
    }

    #[test]
    fn flat_side() {
        assert!(Key::parse("F").unwrap().use_flats());
        assert!(Key::parse("Dm").unwrap().use_flats());
        assert!(!Key::parse("G").unwrap().use_flats());
        assert!(!Key::parse("Em").unwrap().use_flats());
    }
}
