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
fn fmt_canonicalizes_and_is_idempotent() {
    // Scruffy but valid: extra whitespace, direct bars out of order.
    let scruffy = "\
# song: f  tempo:   90.00   meter: 4/4  grid: 1/16
# instruments: p:0
b2 p |   G4 E4   C8   |
b1 p | C8 E4 G4 |
";
    let f = Tmp::new("scruffy.ls", scruffy.as_bytes());
    let out = bin().args(["fmt"]).arg(&f.0).output().unwrap();
    no_panic(&out);
    assert!(out.status.success(), "{out:?}");
    let once = std::fs::read_to_string(&f.0).unwrap();
    assert_ne!(once, scruffy, "fmt must canonicalize");
    assert!(once.contains("arrangement:"), "{once}");

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
