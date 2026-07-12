//! Layer 2b — chord symbols: detection from voicings and exact
//! reconstruction back to pitches.
//!
//! The contract that keeps the roundtrip lossless: a voicing is only named
//! (`Am`, `F/A`, `G7(4)`) if the *canonical voicing* of that name — chord
//! tones stacked closely upward from the bass note — reproduces the input
//! pitches exactly. Anything else (doublings, spread voicings, clusters)
//! stays as explicit pitch tuples in melodic notation. Never lose data to a
//! wrong chord name.

use crate::key::pc_name;

/// (suffix, intervals from root). Detection preference follows list order.
pub const QUALITIES: &[(&str, &[u8])] = &[
    ("", &[0, 4, 7]),
    ("m", &[0, 3, 7]),
    ("7", &[0, 4, 7, 10]),
    ("m7", &[0, 3, 7, 10]),
    ("maj7", &[0, 4, 7, 11]),
    ("sus4", &[0, 5, 7]),
    ("sus2", &[0, 2, 7]),
    ("dim", &[0, 3, 6]),
    ("aug", &[0, 4, 8]),
    ("m7b5", &[0, 3, 6, 10]),
    ("dim7", &[0, 3, 6, 9]),
    ("6", &[0, 4, 7, 9]),
    ("m6", &[0, 3, 7, 9]),
];

/// The comping register: `(3)` hints are omitted for bass octave 3.
pub const DEFAULT_BASS_OCTAVE: i8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChordSym {
    pub root_pc: u8,
    /// Index into [`QUALITIES`].
    pub quality: usize,
    /// Lowest sounding pitch class; ≠ root_pc means a slash chord.
    pub bass_pc: u8,
    /// Octave of the bass note (scientific, C4 = middle C).
    pub bass_octave: i8,
}

/// Canonical voicing: bass note, then the remaining chord tones in template
/// order, each at the closest position above the previous note.
pub fn voicing(sym: &ChordSym) -> Option<Vec<u8>> {
    let ints = QUALITIES.get(sym.quality)?.1;
    let pcs: Vec<u8> = ints.iter().map(|i| (sym.root_pc + i) % 12).collect();
    let start = pcs.iter().position(|&p| p == sym.bass_pc % 12)?;
    let base = (sym.bass_octave as i32 + 1) * 12 + (sym.bass_pc % 12) as i32;
    if !(0..=127).contains(&base) {
        return None;
    }
    let mut out = vec![base as u8];
    let mut prev = base;
    for k in 1..pcs.len() {
        let pc = pcs[(start + k) % pcs.len()] as i32;
        let mut step = (pc - prev.rem_euclid(12)).rem_euclid(12);
        if step == 0 {
            step = 12;
        }
        prev += step;
        if prev > 127 {
            return None;
        }
        out.push(prev as u8);
    }
    Some(out)
}

/// Name a voicing, or `None` if no name reproduces it exactly.
/// `pitches` must be sorted ascending.
pub fn detect(pitches: &[u8]) -> Option<ChordSym> {
    if !(3..=4).contains(&pitches.len()) {
        return None;
    }
    let pcs: std::collections::BTreeSet<u8> = pitches.iter().map(|p| p % 12).collect();
    if pcs.len() != pitches.len() {
        return None; // doubled pitch class → tuple, not a symbol
    }
    let bass = pitches[0];
    // Root-position reading is preferred over any slash reading
    // (Am7 beats C6/A when A is in the bass).
    let mut roots: Vec<u8> = vec![bass % 12];
    roots.extend(pcs.iter().copied().filter(|&pc| pc != bass % 12));
    for root_pc in roots {
        for (quality, (_, ints)) in QUALITIES.iter().enumerate() {
            if ints.len() != pitches.len() {
                continue;
            }
            let tset: std::collections::BTreeSet<u8> =
                ints.iter().map(|i| (root_pc + i) % 12).collect();
            if tset != pcs {
                continue;
            }
            let sym = ChordSym {
                root_pc,
                quality,
                bass_pc: bass % 12,
                bass_octave: (bass / 12) as i8 - 1,
            };
            if voicing(&sym).as_deref() == Some(pitches) {
                return Some(sym);
            }
        }
    }
    None
}

pub fn symbol_to_string(sym: &ChordSym, flats: bool) -> String {
    let mut s = format!("{}{}", pc_name(sym.root_pc, flats), QUALITIES[sym.quality].0);
    if sym.bass_pc != sym.root_pc {
        s.push('/');
        s.push_str(pc_name(sym.bass_pc, flats));
    }
    if sym.bass_octave != DEFAULT_BASS_OCTAVE {
        s.push_str(&format!("({})", sym.bass_octave));
    }
    s
}

fn parse_pc(s: &str) -> Option<(u8, &str)> {
    let mut chars = s.chars();
    let letter = chars.next()?;
    let base: i32 = match letter {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => return None,
    };
    let rest = chars.as_str();
    if let Some(r) = rest.strip_prefix('#') {
        Some((((base + 1).rem_euclid(12)) as u8, r))
    } else if let Some(r) = rest.strip_prefix('b') {
        Some((((base - 1).rem_euclid(12)) as u8, r))
    } else {
        Some((base as u8, rest))
    }
}

/// Parse `Am`, `F/A`, `G7(4)`, `Bbm7b5(2)`, …
pub fn parse_symbol(s: &str) -> Result<ChordSym, String> {
    let (root_pc, rest) = parse_pc(s).ok_or_else(|| format!("bad chord root in {s:?}"))?;
    // Longest quality suffix first, so `maj7` isn't read as `m` + junk.
    let mut by_len: Vec<usize> = (0..QUALITIES.len()).collect();
    by_len.sort_by_key(|&i| std::cmp::Reverse(QUALITIES[i].0.len()));
    for qi in by_len {
        let Some(mut rest) = rest.strip_prefix(QUALITIES[qi].0) else { continue };
        let mut bass_pc = root_pc;
        if let Some(r) = rest.strip_prefix('/') {
            let Some((pc, r)) = parse_pc(r) else { continue };
            bass_pc = pc;
            rest = r;
        }
        let mut bass_octave = DEFAULT_BASS_OCTAVE;
        if let Some(r) = rest.strip_prefix('(') {
            let Some((num, r)) = r.split_once(')') else { continue };
            let Ok(o) = num.parse::<i8>() else { continue };
            bass_octave = o;
            rest = r;
        }
        if !rest.is_empty() {
            continue;
        }
        let sym = ChordSym { root_pc, quality: qi, bass_pc, bass_octave };
        if voicing(&sym).is_none() {
            return Err(format!("bass note not in chord: {s:?}"));
        }
        return Ok(sym);
    }
    Err(format!("bad chord symbol {s:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(s: &str) -> String {
        let sym = parse_symbol(s).unwrap();
        let v = voicing(&sym).unwrap();
        let back = detect(&v).unwrap();
        assert_eq!(back, sym, "through voicing {v:?}");
        symbol_to_string(&back, false)
    }

    #[test]
    fn symbols_roundtrip() {
        for s in ["Am", "C", "G7", "Fmaj7", "Dsus4", "Bdim", "F#m7", "C6", "Am7(2)", "F/A", "G/B(2)"] {
            assert_eq!(roundtrip(s), s);
        }
    }

    #[test]
    fn known_voicings() {
        // Am(3) = A3 C4 E4
        let am = parse_symbol("Am").unwrap();
        assert_eq!(voicing(&am).unwrap(), vec![57, 60, 64]);
        // F/A(3) = A3 C4 F4 (first inversion, stacked from bass)
        let fa = parse_symbol("F/A").unwrap();
        assert_eq!(voicing(&fa).unwrap(), vec![57, 60, 65]);
    }

    #[test]
    fn root_position_beats_slash_alias() {
        // A C E G from A = Am7, even though it's also C6/A.
        let sym = detect(&[57, 60, 64, 67]).unwrap();
        assert_eq!(symbol_to_string(&sym, false), "Am7");
        // Same pcs from C = C6.
        let sym = detect(&[60, 64, 67, 69]).unwrap();
        assert_eq!(symbol_to_string(&sym, false), "C6(4)");
    }

    #[test]
    fn non_canonical_voicings_are_rejected() {
        assert!(detect(&[57, 60, 64, 69]).is_none(), "doubled pc");
        assert!(detect(&[45, 60, 64]).is_none(), "spread voicing (A2 C4 E4)");
        assert!(detect(&[60, 61, 62]).is_none(), "cluster");
        assert!(detect(&[60, 64]).is_none(), "dyad");
    }

    #[test]
    fn flat_spelling() {
        let sym = parse_symbol("Bbm").unwrap();
        assert_eq!(symbol_to_string(&sym, true), "Bbm");
        assert_eq!(symbol_to_string(&sym, false), "A#m");
    }
}
