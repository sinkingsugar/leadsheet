//! Ingestion: external formats → [`crate::model::RawSong`].

mod jsonl;
mod midi;

pub use jsonl::ingest_jsonl;
pub use midi::ingest_midi;

use crate::error::Error;
use crate::model::RawSong;
use std::path::Path;

/// Load a song from a path, dispatching on extension:
/// `.mid`/`.midi` → SMF parser, anything else → MuScriptor jsonl.
pub fn ingest_path(path: &Path) -> Result<RawSong, Error> {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "song".into());
    let ext = path.extension().map(|e| e.to_ascii_lowercase());
    if ext.as_deref().is_some_and(|e| e == "mid" || e == "midi") {
        let bytes = std::fs::read(path)?;
        ingest_midi(&bytes, &name)
    } else {
        let text = std::fs::read_to_string(path)?;
        ingest_jsonl(&text, &name)
    }
}
