//! `leadsheet eval <dir>` — the Phase 4 editability harness (lean).
//!
//! Each subdirectory of `<dir>` is one task:
//!
//! ```text
//! eval/transpose-up-2/
//!   input.ls           the source the model was given
//!   instruction.txt    what the (external) runner asked the model to do
//!   constraints.json   checks applied to the model's saved answer
//!   sample-output.ls   a known-good answer (used when output.ls absent,
//!                      so the harness self-tests in CI)
//!   output.ls          the model's saved answer (written by the runner)
//! ```
//!
//! No API calls, no model deps: an external runner produces `output.ls`,
//! this checks it against musical constraints and prints a pass/fail
//! table. Constraint reference (JSON array of objects):
//!
//! - `{"type":"parses"}` — output parses and validates
//! - `{"type":"pitch_shift","track":T,"semitones":N}` — track T equals
//!   the input transposed by N, rhythm identical
//! - `{"type":"tracks_unchanged","tracks":[..]}` — named tracks equal
//! - `{"type":"track_changed","track":T}` — track T must differ
//! - `{"type":"n_bars","equals":N}`
//! - `{"type":"bars_unchanged","upto":N}` — every note starting in the
//!   first N bars equal
//! - `{"type":"top_line_unchanged","track":T}` — the highest pitch at
//!   each onset is preserved (reharmonization keeps the melody)
//! - `{"type":"matches","file":F}` — note-exact equality with F
//! - `{"type":"transcription_snaps"}` — expected-behavior fixture (D3):
//!   render → ingest → quantize snaps the input's sub-16th content onto
//!   the 16th grid (authoring resolution ≠ transcription resolution)

use anyhow::{Context, Result, bail};
use leadsheet_core::grid::{MusicalTime, QSong, QuantizeOptions};
use leadsheet_core::{ingest, parse, render};
use std::path::Path;

pub fn run(dir: &Path) -> Result<bool> {
    let mut tasks: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    tasks.sort();
    if tasks.is_empty() {
        bail!("no task directories in {}", dir.display());
    }
    let mut all_ok = true;
    for task in &tasks {
        let name = task.file_name().unwrap().to_string_lossy().to_string();
        let outcome = check_task(task);
        match outcome {
            Ok(results) => {
                for (constraint, ok, detail) in results {
                    all_ok &= ok;
                    let status = if ok { "PASS" } else { "FAIL" };
                    match detail {
                        Some(d) if !ok => println!("{status}  {name} :: {constraint} — {d}"),
                        _ => println!("{status}  {name} :: {constraint}"),
                    }
                }
            }
            Err(e) => {
                all_ok = false;
                println!("FAIL  {name} :: {e}");
            }
        }
    }
    Ok(all_ok)
}

type CheckResult = (String, bool, Option<String>);

fn check_task(task: &Path) -> Result<Vec<CheckResult>> {
    let input_text = std::fs::read_to_string(task.join("input.ls"))?;
    let constraints: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(task.join("constraints.json"))?)?;
    let output_path = if task.join("output.ls").exists() {
        task.join("output.ls")
    } else {
        task.join("sample-output.ls")
    };
    let output_text = std::fs::read_to_string(&output_path)?;

    // The input of a repair task is deliberately broken; parse lazily.
    let input = parse::parse(&input_text);
    let mut results = Vec::new();
    let mut output: Option<QSong> = None;

    for c in constraints.as_array().context("constraints.json must be a JSON array")?.iter() {
        let ty = c["type"].as_str().context("constraint without a type")?;
        let (ok, detail) = match ty {
            "parses" => match parse::parse_document(&output_text) {
                Ok(d) => match d.validate().and_then(|_| d.resolve()) {
                    Ok(q) => {
                        output = Some(q);
                        (true, None)
                    }
                    Err(e) => (false, Some(e.to_string())),
                },
                Err(e) => (false, Some(e.to_string())),
            },
            "transcription_snaps" => transcription_snaps(input.as_ref().ok().context("input")?),
            other => {
                let q = match &output {
                    Some(q) => q,
                    None => {
                        output = Some(parse::parse(&output_text)?);
                        output.as_ref().unwrap()
                    }
                };
                let inp = input.as_ref().ok();
                match other {
                    "pitch_shift" => pitch_shift(
                        inp.context("input")?,
                        q,
                        c["track"].as_str().context("track")?,
                        c["semitones"].as_i64().context("semitones")? as i32,
                    ),
                    "tracks_unchanged" => {
                        let mut ok = true;
                        let mut why = None;
                        for t in c["tracks"].as_array().context("tracks")? {
                            let t = t.as_str().context("track name")?;
                            if notes_of(inp.context("input")?, t) != notes_of(q, t) {
                                ok = false;
                                why = Some(format!("track {t} changed"));
                            }
                        }
                        (ok, why)
                    }
                    "track_changed" => {
                        let t = c["track"].as_str().context("track")?;
                        (
                            notes_of(inp.context("input")?, t) != notes_of(q, t),
                            Some(format!("track {t} is unchanged")),
                        )
                    }
                    "n_bars" => {
                        let want = c["equals"].as_u64().context("equals")? as u32;
                        (q.n_bars == want, Some(format!("{} bars, wanted {want}", q.n_bars)))
                    }
                    "bars_unchanged" => {
                        let upto = c["upto"].as_u64().context("upto")? as i64;
                        let inp = inp.context("input")?;
                        let limit = MusicalTime(inp.bar_ticks().ticks() * upto);
                        (prefix(inp, limit) == prefix(q, limit), None)
                    }
                    "top_line_unchanged" => {
                        let t = c["track"].as_str().context("track")?;
                        (top_line(inp.context("input")?, t) == top_line(q, t), None)
                    }
                    "matches" => {
                        let f = c["file"].as_str().context("file")?;
                        let target = parse::parse(&std::fs::read_to_string(task.join(f))?)?;
                        matches_exactly(q, &target)
                    }
                    _ => bail!("unknown constraint type {other:?}"),
                }
            }
        };
        results.push((ty.to_string(), ok, detail));
    }
    Ok(results)
}

/// Note-exact equality in BOTH directions (C1): a canonical map per
/// side, so an invented extra track fails just like a missing one.
/// Header tempo/key are deliberately out of scope — `matches` checks
/// musical content; header constraints can become their own type when a
/// task needs them (meter is covered indirectly: it shifts `n_bars`).
fn matches_exactly(q: &QSong, target: &QSong) -> (bool, Option<String>) {
    if q.n_bars != target.n_bars {
        return (false, Some(format!("{} bars, wanted {}", q.n_bars, target.n_bars)));
    }
    let map = |q: &QSong| -> std::collections::BTreeMap<String, (u8, bool, Vec<NoteKey>)> {
        q.tracks
            .iter()
            .map(|t| (t.name.clone(), (t.program, t.is_drums, t.notes.iter().map(key).collect())))
            .collect()
    };
    let (got, want) = (map(q), map(target));
    for name in want.keys() {
        if !got.contains_key(name) {
            return (false, Some(format!("missing track {name:?}")));
        }
    }
    for name in got.keys() {
        if !want.contains_key(name) {
            return (false, Some(format!("unexpected extra track {name:?}")));
        }
    }
    for (name, t) in &want {
        if got[name] != *t {
            return (false, Some(format!("track {name:?} differs from the target")));
        }
    }
    (true, None)
}

type NoteKey = (u8, i64, i64, u8);

fn key(n: &leadsheet_core::grid::QNote) -> NoteKey {
    (n.pitch, n.onset.ticks(), n.dur.ticks(), n.strokes)
}

fn notes_of(q: &QSong, track: &str) -> Vec<NoteKey> {
    q.tracks
        .iter()
        .find(|t| t.name == track)
        .map(|t| t.notes.iter().map(key).collect())
        .unwrap_or_default()
}

fn prefix(q: &QSong, limit: MusicalTime) -> Vec<(String, NoteKey)> {
    let mut v: Vec<(String, NoteKey)> = q
        .tracks
        .iter()
        .flat_map(|t| t.notes.iter().filter(|n| n.onset < limit).map(|n| (t.name.clone(), key(n))))
        .collect();
    v.sort();
    v
}

fn top_line(q: &QSong, track: &str) -> Vec<(i64, u8)> {
    let mut tops: std::collections::BTreeMap<i64, u8> = Default::default();
    if let Some(t) = q.tracks.iter().find(|t| t.name == track) {
        for n in &t.notes {
            let e = tops.entry(n.onset.ticks()).or_insert(0);
            *e = (*e).max(n.pitch);
        }
    }
    tops.into_iter().collect()
}

fn pitch_shift(inp: &QSong, out: &QSong, track: &str, semis: i32) -> (bool, Option<String>) {
    let a = notes_of(inp, track);
    let b = notes_of(out, track);
    if a.len() != b.len() {
        return (false, Some(format!("{} notes, wanted {}", b.len(), a.len())));
    }
    let shifted: Vec<NoteKey> =
        a.iter().map(|(p, o, d, s)| ((*p as i32 + semis) as u8, *o, *d, *s)).collect();
    (shifted == b, Some("rhythm or interval mismatch".into()))
}

/// D3 expected behavior: quantization snaps to the 16th grid, so the
/// fractional/tuplet content of the input does NOT survive a MIDI →
/// compress trip — and everything lands on cells.
fn transcription_snaps(input: &QSong) -> (bool, Option<String>) {
    let midi = render::render(input);
    let Ok(song) = ingest::ingest_midi(&midi, "d3") else {
        return (false, Some("render output failed to ingest".into()));
    };
    let (q, _) = leadsheet_core::grid::quantize(&song, &QuantizeOptions::default());
    let all_on_grid =
        q.tracks.iter().all(|t| t.notes.iter().all(|n| n.onset.try_as_sixteenths().is_some()));
    let input_had_subgrid =
        input.tracks.iter().any(|t| t.notes.iter().any(|n| n.onset.try_as_sixteenths().is_none()));
    match (all_on_grid, input_had_subgrid) {
        (true, true) => (true, None),
        (false, _) => (false, Some("quantized output left the 16th grid".into())),
        (_, false) => (false, Some("fixture must contain sub-16th content".into())),
    }
}
