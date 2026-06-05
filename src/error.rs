//! src/error.rs — crate-wide error type unifying EPUB / LLM / IO / JSON errors for
//! non-anyhow call sites. The app boundary (main, app/) uses anyhow; library internals
//! may use this. Built after epub + llm so their error types exist.

#[derive(thiserror::Error, Debug)]
pub enum HonyaError {
    #[error(transparent)]
    Epub(#[from] crate::epub::EpubError),
    #[error(transparent)]
    Llm(#[from] crate::llm::LlmError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, HonyaError>;
