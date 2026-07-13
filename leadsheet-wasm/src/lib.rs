//! Thin JS bindings over leadsheet-core for the web playground.
//!
//! Bytes and strings in, JSON strings out — no logic lives here. The JSON
//! shapes mirror `leadsheet check --json` so the CLI and the browser speak
//! the same diagnostics dialect.

use leadsheet_core::{emit, grid, ingest, metrics, parse, render as render_midi};
use wasm_bindgen::prelude::*;

fn quantize_options(
    bpm: Option<f64>,
    infer_tempo: bool,
    no_infer_tempo: bool,
) -> grid::QuantizeOptions {
    grid::QuantizeOptions { bpm_override: bpm, infer_tempo, no_infer: no_infer_tempo }
}

fn tempo_source_json(ts: &grid::TempoSource) -> serde_json::Value {
    match ts {
        grid::TempoSource::Declared => serde_json::json!({ "kind": "declared" }),
        grid::TempoSource::Inferred => serde_json::json!({ "kind": "inferred" }),
        grid::TempoSource::Override => serde_json::json!({ "kind": "override" }),
        grid::TempoSource::AutoInferred { declared_bpm, declared_mean_ms } => serde_json::json!({
            "kind": "auto-inferred",
            "declaredBpm": declared_bpm,
            "declaredMeanMs": declared_mean_ms,
        }),
    }
}

fn error_json(e: &leadsheet_core::Error) -> String {
    let payload = match e.diagnostic() {
        Some(d) => serde_json::json!({ "ok": false, "diagnostics": [d] }),
        None => serde_json::json!({ "ok": false, "error": e.to_string() }),
    };
    payload.to_string()
}

fn compress_song(song: leadsheet_core::model::RawSong, opts: &grid::QuantizeOptions) -> String {
    let (qsong, report) = grid::quantize(&song, opts);
    let text = emit::emit(&qsong);
    let naive = metrics::naive_event_text(&song).len();
    serde_json::json!({
        "ok": true,
        "text": text,
        "bpm": report.bpm,
        "tempoSource": tempo_source_json(&report.tempo_source),
        "bars": qsong.n_bars,
        "notes": report.note_count,
        "lsBytes": text.len(),
        "naiveBytes": naive,
    })
    .to_string()
}

/// Compress a standard MIDI file. Returns JSON:
/// `{ok, text, bpm, tempoSource, bars, notes, lsBytes, naiveBytes}`
/// or `{ok: false, error}`.
#[wasm_bindgen]
pub fn compress_midi(
    bytes: &[u8],
    name: &str,
    bpm: Option<f64>,
    infer_tempo: bool,
    no_infer_tempo: bool,
) -> String {
    let opts = quantize_options(bpm, infer_tempo, no_infer_tempo);
    match ingest::ingest_midi(bytes, name) {
        Ok(song) => compress_song(song, &opts),
        Err(e) => error_json(&e),
    }
}

/// Compress a MuScriptor .jsonl transcription. Same JSON shape as
/// `compress_midi`.
#[wasm_bindgen]
pub fn compress_jsonl(
    text: &str,
    name: &str,
    bpm: Option<f64>,
    infer_tempo: bool,
    no_infer_tempo: bool,
) -> String {
    let opts = quantize_options(bpm, infer_tempo, no_infer_tempo);
    match ingest::ingest_jsonl(text, name) {
        Ok(song) => compress_song(song, &opts),
        Err(e) => error_json(&e),
    }
}

/// Validate leadsheet text. Returns JSON mirroring `leadsheet check --json`:
/// `{ok, bars, tracks, notes, bpm, meter}` or `{ok: false, diagnostics}`.
#[wasm_bindgen]
pub fn check(text: &str) -> String {
    match parse::parse(text) {
        Ok(q) => {
            let notes: usize = q.tracks.iter().map(|t| t.notes.len()).sum();
            serde_json::json!({
                "ok": true,
                "bars": q.n_bars,
                "tracks": q.tracks.len(),
                "notes": notes,
                "bpm": q.bpm,
                "meter": [q.meter.0, q.meter.1],
            })
            .to_string()
        }
        Err(e) => error_json(&e),
    }
}

/// Render leadsheet text to a standard MIDI file. Throws the same JSON
/// payload `check` would return on invalid input.
#[wasm_bindgen]
pub fn render(text: &str) -> Result<Vec<u8>, JsError> {
    let qsong = parse::parse(text).map_err(|e| JsError::new(&error_json(&e)))?;
    Ok(render_midi::render(&qsong))
}

/// Rewrite leadsheet text in canonical form (Document-canonical: authored
/// structure survives). Throws the `check` JSON payload on invalid input.
#[wasm_bindgen]
pub fn fmt(text: &str) -> Result<String, JsError> {
    let document = parse::parse_document(text).map_err(|e| JsError::new(&error_json(&e)))?;
    document.resolve().map_err(|e| JsError::new(&error_json(&e)))?;
    Ok(emit::emit_document(&document))
}
