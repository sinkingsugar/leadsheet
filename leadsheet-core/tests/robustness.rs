//! Parser/ingest robustness: arbitrary input never panics, pathological
//! input fails fast with a clean `Err` (bounded time and memory).
//!
//! `&str` is valid UTF-8 by construction, so the interesting surfaces are
//! arbitrary Unicode into `parse()`, structured mutations of valid `.ls`
//! text (much better at reaching deep parser states than raw noise), and
//! arbitrary bytes into the ingest layer. The CLI's byte boundary (invalid
//! UTF-8 files) is covered in leadsheet-cli's integration tests.

use leadsheet_core::{ingest, parse};
use proptest::prelude::*;

/// A small but feature-complete .ls: melodic (chord tuple, tie, dynamics),
/// drum lanes (accents, ghosts, stroke digits), chord mode, direct bars,
/// variants, arrangement labels/reps/silence.
const BASE_LS: &str = "\
# song: m  tempo: 120.00  meter: 4/4  key: Am  swing: 56%  grid: 1/16
# instruments: p:0 d:kit

P1 p | C4 >[EG]2 ~z2 ^F8- |
P2 d
  K |x... .... x.x. ....|
  S |.... X..o .... 234.|
P3 d ~P2
  h |x.x. x.x. x.x. x.x.|
b3 p* | Am . F G7 |

arrangement:
  A: [P1+P2] x2
  [z]
  B: [P1+P3]
";

/// Splice fragments that exercise every grammar corner (huge numbers,
/// unclosed brackets, stray markers, header keys in the wrong place).
const SNIPPETS: &[&str] = &[
    "|",
    "~P3",
    "~P999",
    "x4294967295",
    "x0",
    "[",
    "]",
    "&",
    "-",
    "@ff",
    "@zz",
    "16th",
    "58%",
    "arrangement:",
    "\n",
    "  ",
    "z",
    "^",
    "__",
    "4294967295",
    "b0",
    "b4294967295",
    "P0",
    "meter: 0/4",
    "meter: 4294967295/8",
    "tempo: nan",
    "swing: 90%",
    "[z]",
    "(",
    ")",
    "/",
    "*",
    ".",
    "C,,,,,,,,,,",
    "K |xxxx|",
    "d128",
    "'",
    "P1 p |",
    "kit",
    ":",
];

fn mutated_ls() -> impl Strategy<Value = String> {
    prop::collection::vec((any::<prop::sample::Index>(), prop::sample::select(SNIPPETS)), 0..10)
        .prop_map(|muts| {
            let mut text = BASE_LS.to_string();
            for (idx, snip) in muts {
                // BASE_LS and all snippets are ASCII: any index is a char
                // boundary.
                let pos = idx.index(text.len() + 1);
                text.insert_str(pos, snip);
            }
            text
        })
}

proptest! {
    #[test]
    fn parse_never_panics_on_unicode(s in "\\PC{0,400}") {
        let _ = parse::parse(&s);
    }

    #[test]
    fn parse_never_panics_on_mutated_ls(text in mutated_ls()) {
        let _ = parse::parse(&text);
    }

    #[test]
    fn ingest_midi_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = ingest::ingest_midi(&bytes, "fuzz");
    }

    #[test]
    fn ingest_jsonl_never_panics(s in "\\PC{0,600}") {
        let _ = ingest::ingest_jsonl(&s, "fuzz");
    }
}

/// The pathological inputs that used to panic (integer overflow, div by
/// zero) or eat unbounded memory (arrangement rep bombs) — all must be
/// clean errors now, and stay that way.
#[test]
fn pathological_inputs_error_cleanly() {
    let head = "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\n";
    for (label, text) in [
        ("huge direct bar", format!("{head}b4294967295 p | C16 |\n")),
        ("bar beyond limit", format!("{head}b100001 p | C16 |\n")),
        (
            "huge meter numerator",
            "# song: x  tempo: 120  meter: 4294967295/4\n# instruments: p:0\nb1 p | C16 |\n".into(),
        ),
        (
            "zero meter numerator",
            "# song: x  tempo: 120  meter: 0/4\n# instruments: p:0\nb1 p |  |\n".into(),
        ),
        ("duration sum overflow", format!("{head}b1 p | C4294967290 C10 |\n")),
        ("single overflowing duration", format!("{head}b1 p | C4294967295 |\n")),
        ("arrangement rep bomb", format!("{head}P1 p | C16 |\narrangement:\n  [P1] x4294967295\n")),
        (
            "rep bomb via unit",
            format!("{head}P1 p | C16 | D16 |\narrangement:\n  [P1] x2147483647\n"),
        ),
        ("multi-bar direct beyond limit", format!("{head}b99999 p | C16 | D16 | E16 |\n")),
    ] {
        let r = std::panic::catch_unwind(|| parse::parse(&text));
        match r {
            Ok(Err(_)) => {}
            Ok(Ok(q)) => panic!("{label}: accepted ({} bars)", q.n_bars),
            Err(_) => panic!("{label}: panicked"),
        }
    }
}

/// The limits must not reject real material: a long-but-sane song parses.
#[test]
fn generous_but_bounded() {
    let head = "# song: x  tempo: 120  meter: 4/4\n# instruments: p:0\n";
    let long = format!("{head}P1 p | C16 |\narrangement:\n  [P1] x99999\n");
    assert_eq!(parse::parse(&long).unwrap().n_bars, 99999);
    let odd_meter = "# song: x  tempo: 120  meter: 15/8\n# instruments: p:0\nb1 p | z30 |\n";
    assert_eq!(parse::parse(odd_meter).unwrap().meter, (15, 8));
}

/// Parser-valid extremes must render without panicking (B3: MIDI tempo is
/// a 24-bit µs/quarter field — unrepresentable BPMs clamp).
#[test]
fn extreme_tempo_and_meter_render_cleanly() {
    for tempo in ["0.0001", "0.001", "1000000", "3.5"] {
        let text = format!(
            "# song: x  tempo: {tempo}  meter: 4/4  grid: 1/16\n# instruments: p:0\nb1 p | C16 |\n"
        );
        let q = parse::parse(&text).unwrap();
        let midi = leadsheet_core::render::render(&q);
        assert!(ingest::ingest_midi(&midi, "x").is_ok(), "tempo {tempo}");
    }
    let text =
        "# song: x  tempo: 120  meter: 64/8  grid: 1/16\n# instruments: p:0\nb1 p | z128 |\n";
    let q = parse::parse(text).unwrap();
    assert!(ingest::ingest_midi(&leadsheet_core::render::render(&q), "x").is_ok());
}
