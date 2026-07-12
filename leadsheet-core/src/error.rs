use serde::Serialize;
use thiserror::Error;

/// A structured parse diagnostic: code + location + message + concrete fix
/// advice. The goal is that an LLM (or a human) can repair the `.ls` text
/// from the diagnostic alone, without re-reading the format spec.
#[derive(Debug, Clone, Serialize)]
pub struct Diagnostic {
    /// Stable kebab-case class, e.g. `bar-length`, `unknown-lane`.
    pub code: &'static str,
    /// 1-based line; 0 = whole file.
    pub line: usize,
    /// 1-based column of the offending token; 0 = unknown.
    pub col: usize,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub suggestion: Option<String>,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "line {}", self.line)?;
            if self.col > 0 {
                write!(f, ":{}", self.col)?;
            }
            write!(f, ": ")?;
        }
        write!(f, "{} [{}]", self.message, self.code)?;
        if let Some(s) = &self.suggestion {
            write!(f, "\n  help: {s}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("midi parse: {0}")]
    Midi(String),
    #[error("jsonl parse: {0}")]
    Jsonl(String),
    #[error("leadsheet parse: {0}")]
    Parse(Diagnostic),
}

impl Error {
    /// The structured diagnostic, when this error carries one.
    pub fn diagnostic(&self) -> Option<&Diagnostic> {
        match self {
            Error::Parse(d) => Some(d),
            _ => None,
        }
    }
}
