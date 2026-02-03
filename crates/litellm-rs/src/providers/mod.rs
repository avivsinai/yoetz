use crate::config::ProviderConfig;
use crate::error::{LiteLLMError, Result};
use std::env;

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;

pub fn resolve_api_key(cfg: &ProviderConfig) -> Result<Option<String>> {
    if cfg.no_auth {
        return Ok(None);
    }
    if let Some(key) = cfg.api_key.clone() {
        return Ok(Some(key));
    }
    if let Some(env_key) = cfg.api_key_env.as_deref() {
        let key =
            env::var(env_key).map_err(|_| LiteLLMError::MissingApiKey(env_key.to_string()))?;
        return Ok(Some(key));
    }
    Err(LiteLLMError::MissingApiKey("<unset>".into()))
}
