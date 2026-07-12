//! Structured diagnostics: every common authoring mistake must come back
//! with a stable code, the right location, and a suggestion good enough
//! for an LLM to repair the file without re-reading FORMAT.md.

use leadsheet_core::error::{Diagnostic, Error};
use leadsheet_core::parse;

const HEAD: &str = "# song: t  tempo: 120.00  meter: 4/4  grid: 1/16\n\
                    # instruments: piano:0 drums:kit\n";

fn diag_of(text: &str) -> Diagnostic {
    match parse::parse(text) {
        Err(Error::Parse(d)) => d,
        Err(e) => panic!("expected a parse diagnostic, got {e}"),
        Ok(_) => panic!("expected an error for:\n{text}"),
    }
}

#[track_caller]
fn expect(text: &str, code: &str, line: usize, hint_contains: &str) -> Diagnostic {
    let d = diag_of(text);
    assert_eq!(d.code, code, "wrong code for: {}\n(diagnostic: {d})", d.message);
    assert_eq!(d.line, line, "wrong line ({d})");
    let hint = d.suggestion.as_deref().unwrap_or("");
    assert!(hint.contains(hint_contains), "suggestion {hint:?} should mention {hint_contains:?}");
    d
}

// The five mistakes PLAN.md names, plus the ones right next to them.

#[test]
fn bad_bar_length_short_and_long() {
    let d = expect(&format!("{HEAD}b1 piano | C4 D4 |\n"), "bar-length", 3, "fill the bar");
    assert!(d.message.contains("8 of 16"), "{d}");
    // Overflowing token points at the culprit, column included.
    let d = expect(&format!("{HEAD}b1 piano | C12 D8 |\n"), "bar-length", 3, "tie");
    assert!(d.message.contains("D8"), "{d}");
    assert!(d.col > 0, "column expected ({d})");
    // Drum lanes count cells too.
    let d = expect(&format!("{HEAD}b1 drums\n  K |x... x...|\n"), "bar-length", 4, "16th");
    assert!(d.message.contains("8 cells, expected 16"), "{d}");
}

#[test]
fn unknown_lane_names_the_lanes() {
    let text = format!("{HEAD}b1 drums\n  K |x... .... x... ....|\n  Q |x... .... x... ....|\n");
    let d = expect(&text, "unknown-lane", 5, "d<key>");
    assert!(d.message.contains("\"Q\""), "{d}");
    assert!(d.suggestion.as_deref().unwrap_or("").contains("K K2 S"), "{d}");
}

#[test]
fn hold_across_barline() {
    let text = format!("{HEAD}b1 piano* | Dm7 . . . |\nb2 piano* | . z G7 . |\n");
    let d = expect(&text, "hold-across-bar", 4, "restate the chord");
    assert!(d.message.contains("hold with no chord"), "{d}");
}

#[test]
fn bad_chord_symbol() {
    let d = expect(&format!("{HEAD}b1 piano* | Xm7 . . . |\n"), "bad-chord", 3, "[CEG]4");
    assert!(d.col > 0, "column of the symbol ({d})");
    // A real root with an impossible bass is also a chord problem.
    expect(&format!("{HEAD}b1 piano* | C/D . . . |\n"), "bad-chord", 3, "Am7");
}

#[test]
fn dangling_variant_reference() {
    // In a drum block…
    let text = format!("{HEAD}P8 drums ~P3\n  h |x.x. x.x. x.x. x.x.|\n");
    let d = expect(&text, "unknown-pattern", 3, "defined earlier");
    assert!(d.message.contains("P3"), "{d}");
    // …and on a melodic line.
    let text = format!("{HEAD}P1 piano | C16 |\nP2 piano ~P7 | D16 |\n");
    let d = expect(&text, "unknown-pattern", 4, "defined patterns: P1");
    assert!(d.message.contains("P7"), "{d}");
}

// The rest of the everyday mistakes.

#[test]
fn unknown_instrument_lists_declared_ones() {
    let d = expect(&format!("{HEAD}b1 bass | C16 |\n"), "unknown-instrument", 3, "piano drums");
    assert!(d.message.contains("bass"), "{d}");
    assert!(d.col > 0, "{d}");
}

#[test]
fn melodic_instrument_in_a_drum_block() {
    let d = expect(&format!("{HEAD}b1 piano\n  K |x... .... x... ....|\n"), "not-a-kit", 3, "kit");
    assert!(d.message.contains("piano"), "{d}");
}

#[test]
fn bad_token_and_bad_lane_char() {
    expect(&format!("{HEAD}b1 piano | Q16 |\n"), "bad-token", 3, "accidentals");
    expect(&format!("{HEAD}b1 drums\n  K |q... .... x... ....|\n"), "bad-lane-char", 4, "ghost");
}

#[test]
fn unknown_pattern_in_arrangement() {
    let text = format!("{HEAD}P1 piano | C16 |\narrangement:\n  [P1+P2]\n");
    let d = expect(&text, "unknown-pattern", 5, "defined patterns: P1");
    assert!(d.message.contains("P2"), "{d}");
}

#[test]
fn header_mistakes() {
    expect("# song: x  tempo: fast\n# instruments: p:0\nb1 p | C16 |\n", "bad-tempo", 1, "");
    expect(
        "# song: x  tempo: 120  meter: 5/16\n# instruments: p:0\nb1 p | C16 |\n",
        "bad-meter",
        1,
        "6/8",
    );
    expect(
        "# song: x  tempo: 120  key: H\n# instruments: p:0\nb1 p | C16 |\n",
        "bad-key",
        1,
        "Ebm",
    );
    expect(
        "# song: x  tempo: 120  swing: 90%\n# instruments: p:0\nb1 p | C16 |\n",
        "bad-swing",
        1,
        "50%..75%",
    );
    expect("b1 p | C16 |\n", "missing-header", 1, "# song:");
    expect(
        "# song: x  tempo: 120\n# instruments: p:0 p:1\nb1 p | C16 |\n",
        "duplicate-instrument",
        2,
        "",
    );
    expect(&format!("{HEAD}b1 piano@fff | C16 |\n"), "unknown-dynamic", 3, "@pp");
}

#[test]
fn duplicate_pattern_and_bad_row() {
    expect(
        &format!("{HEAD}P1 piano | C16 |\nP1 piano | D16 |\n"),
        "duplicate-pattern",
        4,
        "renumber",
    );
    expect(&format!("{HEAD}P1 piano | C16 |\narrangement:\n  [P1] x0\n"), "bad-row", 5, "x<n>");
    expect(&format!("{HEAD}P1 piano | C16 |\narrangement:\n  [P1\n"), "bad-row", 5, "[z]");
}

#[test]
fn too_large_is_its_own_code() {
    expect(&format!("{HEAD}b200000 piano | C16 |\n"), "too-large", 3, "");
    expect(&format!("{HEAD}P1 piano | C16 |\narrangement:\n  [P1] x2000000\n"), "too-large", 5, "");
}

#[test]
fn display_carries_code_and_help() {
    let e = parse::parse(&format!("{HEAD}b1 piano | C4 D4 |\n")).unwrap_err();
    let text = e.to_string();
    assert!(text.contains("line 3"), "{text}");
    assert!(text.contains("[bar-length]"), "{text}");
    assert!(text.contains("help:"), "{text}");
}

#[test]
fn diagnostics_serialize_for_check_json() {
    let e = parse::parse(&format!("{HEAD}b1 piano | C12 D8 |\n")).unwrap_err();
    let d = e.diagnostic().expect("parse errors carry diagnostics");
    let json = serde_json::to_value(d).unwrap();
    assert_eq!(json["code"], "bar-length");
    assert_eq!(json["line"], 3);
    assert!(json["col"].as_u64().unwrap() > 0);
    assert!(json["message"].as_str().unwrap().contains("D8"));
    assert!(json["suggestion"].as_str().unwrap().len() > 10);
}

#[test]
fn unknown_header_field_is_an_error_not_a_default() {
    // `metre:` (typo) must not silently yield 4/4.
    let d = expect(
        "# song: x  tempo: 120  metre: 3/4\n# instruments: p:0\nb1 p | C16 |\n",
        "unknown-header-field",
        1,
        "meter",
    );
    assert!(d.message.contains("metre"), "{d}");
    assert!(d.col > 0, "{d}");
    // Stray swing percent without the key.
    expect("# song: x  tempo: 120  58%\n# instruments: p:0\nb1 p | C16 |\n", "bad-swing", 1, "66%");
}

#[test]
fn trailing_row_junk_is_an_error() {
    let d = expect(
        &format!("{HEAD}P1 piano | C16 |\narrangement:\n  [P1] x4 garbage\n"),
        "bad-row",
        5,
        "[z]",
    );
    assert!(d.message.contains("garbage"), "{d}");
    assert!(d.col > 0, "{d}");
}
