use std::error::Error as StdError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum LiteLLMError {
    #[error("configuration error: {0}")]
    Config(String),
    #[error("provider not found: {0}")]
    ProviderNotFound(String),
    #[error("missing api key for provider: {0}")]
    MissingApiKey(String),
    #[error("http error: {message}")]
    Http {
        message: String,
        #[source]
        source: Option<Box<dyn StdError + Send + Sync>>,
    },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, LiteLLMError>;

impl LiteLLMError {
    pub fn http(message: impl Into<String>) -> Self {
        Self::Http {
            message: message.into(),
            source: None,
        }
    }

    pub fn http_with_source<E>(err: E) -> Self
    where
        E: StdError + Send + Sync + 'static,
    {
        Self::Http {
            message: err.to_string(),
            source: Some(Box::new(err)),
        }
    }
}

impl From<reqwest::Error> for LiteLLMError {
    fn from(err: reqwest::Error) -> Self {
        LiteLLMError::http_with_source(err)
    }
}

impl From<std::io::Error> for LiteLLMError {
    fn from(err: std::io::Error) -> Self {
        LiteLLMError::http_with_source(err)
    }
}
