use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("midi parse: {0}")]
    Midi(String),
    #[error("jsonl parse: {0}")]
    Jsonl(String),
    #[error("leadsheet parse: {0}")]
    Parse(String),
}
