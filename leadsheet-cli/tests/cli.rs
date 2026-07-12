//! End-to-end CLI checks: the agent loop (`check` / `fmt`) and the byte
//! boundary (arbitrary bytes must produce a clean error, never a panic).

use std::path::PathBuf;
use std::process::{Command, Output};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_leadsheet"))
}

/// A scratch file that cleans up after itself.
struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str, bytes: &[u8]) -> Tmp {
        let path =
            std::env::temp_dir().join(format!("leadsheet-cli-{}-{name}", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        Tmp(path)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn no_panic(out: &Output) {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("panicked"), "process panicked:\n{stderr}");
}

const VALID_LS: &str = "\
# song: cli  tempo: 120.00  meter: 4/4  grid: 1/16
# instruments: p:0 d:kit

P1 p | C4 E4 G4 c4 |
P2 d
  K |x... .... x... ....|
  S |.... x... .... x...|

arrangement:
  [P1+P2] x2
";

#[test]
fn check_accepts_valid_file() {
    let f = Tmp::new("ok.ls", VALID_LS.as_bytes());
    let out = bin().args(["check"]).arg(&f.0).output().unwrap();
    no_panic(&out);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("2 bars"), "{stdout}");
}

#[test]
fn check_json_reports_structured_diagnostics() {
    let broken = VALID_LS.replace("| C4 E4 G4 c4 |", "| C4 E4 G4 |");
    let f = Tmp::new("broken.ls", broken.as_bytes());
    let out = bin().args(["check", "--json"]).arg(&f.0).output().unwrap();
    no_panic(&out);
    assert!(!out.status.success(), "must exit nonzero on invalid input");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    let d = &v["diagnostics"][0];
    assert_eq!(d["code"], "bar-length");
    assert_eq!(d["line"], 4);
    assert!(d["suggestion"].as_str().unwrap().contains("rests"), "{d}");
}

#[test]
fn check_json_ok_shape() {
    let f = Tmp::new("ok2.ls", VALID_LS.as_bytes());
    let out = bin().args(["check", "--json"]).arg(&f.0).output().unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["bars"], 2);
    assert_eq!(v["tracks"], 2);
}

#[test]
fn fmt_is_document_canonical_and_idempotent() {
    // Scruffy but valid: extra whitespace, author structure (custom
    // pattern id, direct bars out of order, label). Document-canonical
    // fmt normalizes spelling/layout but PRESERVES the structure.
    let scruffy = "\
# song: f  tempo:   90.00   meter: 4/4  grid: 1/16
# instruments: p:0
P7 p |   G4 E4   C8   |
b2 p |   E4 G4   C8   |
b1 p | C8 E4 G4 |
arrangement:
  verse: [P7] x2
";
    let f = Tmp::new("scruffy.ls", scruffy.as_bytes());
    let out = bin().args(["fmt"]).arg(&f.0).output().unwrap();
    no_panic(&out);
    assert!(out.status.success(), "{out:?}");
    let once = std::fs::read_to_string(&f.0).unwrap();
    assert_ne!(once, scruffy, "fmt must canonicalize");
    assert!(once.contains("P7 p | G4 E4 C8 |"), "author id + spacing normalized:\n{once}");
    assert!(once.contains("b2 p | E4 G4 C8 |"), "direct bars survive:\n{once}");
    assert!(once.contains("verse: [P7] x2"), "labels survive:\n{once}");
    let b1 = once.find("b1 p").unwrap();
    let b2 = once.find("b2 p").unwrap();
    assert!(b2 < b1, "timeline order preserved as written:\n{once}");

    // Second run: byte-identical, reported as unchanged.
    let out = bin().args(["fmt"]).arg(&f.0).output().unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unchanged"));
    assert_eq!(std::fs::read_to_string(&f.0).unwrap(), once, "fmt must be idempotent");
}

#[test]
fn fmt_refuses_invalid_input_without_touching_it() {
    let broken = "# song: x  tempo: 120\n# instruments: p:0\nb1 p | C4 |\n";
    let f = Tmp::new("badfmt.ls", broken.as_bytes());
    let out = bin().args(["fmt"]).arg(&f.0).output().unwrap();
    no_panic(&out);
    assert!(!out.status.success());
    assert_eq!(std::fs::read_to_string(&f.0).unwrap(), broken, "input must be untouched");
    assert!(String::from_utf8_lossy(&out.stderr).contains("bar-length"));
}

/// The plan's byte-boundary property: arbitrary (non-UTF-8) bytes at the
/// file boundary are a clean encoding/parse error for every text-eating
/// subcommand, never a crash.
#[test]
fn garbage_bytes_are_a_clean_error() {
    let garbage: Vec<u8> = (0..=255u8).cycle().take(1024).collect();
    let f = Tmp::new("garbage.ls", &garbage);
    for cmd in ["check", "fmt", "render"] {
        let out = bin().args([cmd]).arg(&f.0).output().unwrap();
        no_panic(&out);
        assert!(!out.status.success(), "{cmd} must fail on garbage bytes");
    }
    // And the binary ingest path (a .mid that isn't MIDI).
    let m = Tmp::new("garbage.mid", &garbage);
    for args in [vec!["inspect"], vec!["compress"], vec!["roundtrip"]] {
        let out = bin().args(&args).arg(&m.0).output().unwrap();
        no_panic(&out);
        assert!(!out.status.success(), "{args:?} must fail on garbage bytes");
    }
}

/// The Phase 4 harness self-tests on its committed sample outputs; a
/// FAIL here means a fixture or checker regressed.
#[test]
fn eval_harness_passes_on_sample_outputs() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().join("eval");
    let out = bin().args(["eval"]).arg(&dir).output().unwrap();
    no_panic(&out);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{stdout}");
    assert!(!stdout.contains("FAIL"), "{stdout}");
    assert!(stdout.contains("transcription-grid :: transcription_snaps"), "{stdout}");
    // A wrong answer fails the right constraint.
    let tmp = std::env::temp_dir().join(format!("leadsheet-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let task = tmp.join("transpose-up-2");
    std::fs::create_dir_all(&task).unwrap();
    for f in ["input.ls", "instruction.txt", "constraints.json"] {
        std::fs::copy(dir.join("transpose-up-2").join(f), task.join(f)).unwrap();
    }
    // Model "answer": transposed the wrong way.
    let wrong = std::fs::read_to_string(dir.join("transpose-up-2/input.ls"))
        .unwrap()
        .replace("| e4 c4 d4 B4 |", "| d4 _B4 c4 A4 |");
    std::fs::write(task.join("output.ls"), wrong).unwrap();
    let out = bin().args(["eval"]).arg(&tmp).output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL  transpose-up-2 :: pitch_shift"));
    let _ = std::fs::remove_dir_all(&tmp);

    // C1: `matches` is note-exact in BOTH directions — an answer that
    // contains every target track plus an invented extra one must FAIL,
    // and so must a renamed track.
    let repair = dir.join("repair");
    let target = std::fs::read_to_string(repair.join("target.ls")).unwrap();
    for (label, wrong) in [
        (
            "extra track",
            target
                .replace("# instruments: lead:81", "# instruments: lead:81 ghost:0")
                .replace("b1 lead | e4 c4 d4 B4 |", "b1 lead | e4 c4 d4 B4 |\nb1 ghost | C16 |"),
        ),
        ("renamed track", target.replace("lead", "solo")),
    ] {
        let task = tmp.join("repair");
        std::fs::create_dir_all(&task).unwrap();
        for f in ["input.ls", "instruction.txt", "constraints.json", "target.ls"] {
            std::fs::copy(repair.join(f), task.join(f)).unwrap();
        }
        std::fs::write(task.join("output.ls"), &wrong).unwrap();
        let out = bin().args(["eval"]).arg(&tmp).output().unwrap();
        no_panic(&out);
        assert!(!out.status.success(), "{label} must fail");
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("FAIL  repair :: matches"),
            "{label}:\n{}",
            String::from_utf8_lossy(&out.stdout)
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
