//! Internal event model shared by all ingestion paths.
//!
//! Timing is absolute seconds — the common denominator between MuScriptor
//! output (which has no tempo) and MIDI files (which do). Tempo/grid live
//! downstream (Layer 1).

/// A single note in absolute time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawNote {
    /// MIDI pitch 0..=127. For drum tracks this is the GM percussion key.
    pub pitch: u8,
    /// Onset in seconds from the start of the song.
    pub onset: f64,
    /// Duration in seconds. Always > 0 after ingestion.
    pub dur: f64,
    /// MIDI velocity 1..=127. MuScriptor has none; ingestion defaults to 96.
    pub vel: u8,
}

impl RawNote {
    pub fn end(&self) -> f64 {
        self.onset + self.dur
    }
}

/// One instrument's notes.
#[derive(Debug, Clone)]
pub struct RawTrack {
    /// Sanitized unique name, e.g. `bass`, `piano`, `piano_2`.
    pub name: String,
    /// GM program 0..=127. Meaningless when `is_drums`.
    pub program: u8,
    pub is_drums: bool,
    /// Sorted by (onset, pitch).
    pub notes: Vec<RawNote>,
}

/// A whole ingested song, still in absolute seconds.
#[derive(Debug, Clone)]
pub struct RawSong {
    pub name: String,
    pub tracks: Vec<RawTrack>,
    /// BPM declared by the source, if it declared exactly one constant tempo
    /// (MIDI files usually do; MuScriptor streams never do).
    pub source_bpm: Option<f64>,
}

impl RawSong {
    pub fn note_count(&self) -> usize {
        self.tracks.iter().map(|t| t.notes.len()).sum()
    }

    /// End of the last note, in seconds. 0.0 for an empty song.
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .flat_map(|t| t.notes.iter())
            .map(RawNote::end)
            .fold(0.0, f64::max)
    }
}

/// Sort notes and make track names unique within a song.
pub(crate) fn finalize_tracks(mut tracks: Vec<RawTrack>) -> Vec<RawTrack> {
    tracks.retain(|t| !t.notes.is_empty());
    for t in &mut tracks {
        t.notes
            .sort_by(|a, b| a.onset.total_cmp(&b.onset).then(a.pitch.cmp(&b.pitch)));
    }
    let mut seen: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    for t in &mut tracks {
        let n = seen.entry(t.name.clone()).or_insert(0);
        *n += 1;
        if *n > 1 {
            t.name = format!("{}_{}", t.name, n);
        }
    }
    tracks
}

/// Lowercase snake_case, ascii-only, non-empty.
pub(crate) fn sanitize_name(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut last_us = true; // suppress leading underscore
    for c in raw.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_us = false;
        } else if !last_us {
            out.push('_');
            last_us = true;
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() { "track".into() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize() {
        assert_eq!(sanitize_name("Acoustic Piano!"), "acoustic_piano");
        assert_eq!(sanitize_name("  --  "), "track");
        assert_eq!(sanitize_name("Lead 1 (square)"), "lead_1_square");
    }

    #[test]
    fn uniquify() {
        let mk = |name: &str| RawTrack {
            name: name.into(),
            program: 0,
            is_drums: false,
            notes: vec![RawNote { pitch: 60, onset: 0.0, dur: 1.0, vel: 96 }],
        };
        let ts = finalize_tracks(vec![mk("piano"), mk("piano"), mk("bass")]);
        let names: Vec<_> = ts.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["piano", "piano_2", "bass"]);
    }
}
