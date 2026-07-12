//! leadsheet-core — MIDI ↔ compact semantic text for LLM consumption.
//!
//! Pipeline: ingest (seconds-domain events) → beat inference & quantization →
//! chordify/voices → pattern dedup → text emission; plus the inverse
//! (parse → render to MIDI), which is the correctness oracle.

pub mod error;
pub mod gm;
pub mod grid;
pub mod ingest;
pub mod model;
pub mod tempo;

pub use error::Error;
