//! Crate-wide error type unifying EPUB / LLM / IO / JSON errors for non-anyhow call sites.

#[derive(thiserror::Error, Debug)]
#[allow(dead_code)]
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

#[allow(dead_code)]
pub type Result<T> = std::result::Result<T, HonyaError>;
