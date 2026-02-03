use anyhow::{anyhow, Context, Result};
use std::env;

use yoetz_core::config::Config;

pub mod gemini;
pub mod openai;

#[derive(Debug, Clone)]
pub struct ProviderAuth {
    pub base_url: String,
    pub api_key: String,
}

pub fn resolve_provider_auth(config: &Config, provider: &str) -> Result<ProviderAuth> {
    let provider_cfg = config.providers.get(provider);

    let base_url = provider_cfg
        .and_then(|p| p.base_url.clone())
        .or_else(|| default_base_url(provider))
        .ok_or_else(|| anyhow!("base_url not found for provider {provider}"))?;

    let api_key_env = provider_cfg
        .and_then(|p| p.api_key_env.clone())
        .or_else(|| default_api_key_env(provider))
        .ok_or_else(|| anyhow!("api_key_env not configured for provider {provider}"))?;

    let api_key =
        env::var(&api_key_env).with_context(|| format!("missing env var {api_key_env}"))?;

    Ok(ProviderAuth { base_url, api_key })
}

pub fn default_base_url(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("https://openrouter.ai/api/v1".to_string()),
        "openai" => Some("https://api.openai.com/v1".to_string()),
        "gemini" => Some("https://generativelanguage.googleapis.com/v1beta".to_string()),
        _ => None,
    }
}

pub fn default_api_key_env(provider: &str) -> Option<String> {
    match provider {
        "openrouter" => Some("OPENROUTER_API_KEY".to_string()),
        "openai" => Some("OPENAI_API_KEY".to_string()),
        "litellm" => Some("LITELLM_API_KEY".to_string()),
        "gemini" => Some("GEMINI_API_KEY".to_string()),
        _ => None,
    }
}
