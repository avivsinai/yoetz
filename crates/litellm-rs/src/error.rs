use thiserror::Error;

#[derive(Debug, Error)]
pub enum LiteLLMError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("missing api key for provider: {0}")]
    MissingApiKey(String),
    #[error("http error: {0}")]
    Http(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, LiteLLMError>;
