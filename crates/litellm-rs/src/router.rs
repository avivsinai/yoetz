use crate::config::{Config, ProviderConfig, ProviderKind};
use crate::error::{LiteLLMError, Result};

#[derive(Debug, Clone)]
pub struct ResolvedModel {
    pub provider: String,
    pub model: String,
    pub config: ProviderConfig,
}

pub fn resolve_model(model: &str, config: &Config) -> Result<ResolvedModel> {
    let (maybe_provider, maybe_model) = split_model(model);

    if let Some(provider) = maybe_provider {
        let provider_cfg = config
            .providers
            .get(provider)
            .cloned()
            .or_else(|| default_provider_config(provider));
        let provider_cfg =
            provider_cfg.ok_or_else(|| LiteLLMError::ProviderNotFound(provider.to_string()))?;
        return Ok(ResolvedModel {
            provider: provider.to_string(),
            model: maybe_model.unwrap_or(model).to_string(),
            config: provider_cfg,
        });
    }

    let default_provider = config.default_provider.as_deref().unwrap_or("openai");
    let provider_cfg = config
        .providers
        .get(default_provider)
        .cloned()
        .or_else(|| default_provider_config(default_provider))
        .ok_or_else(|| LiteLLMError::ProviderNotFound(default_provider.to_string()))?;

    Ok(ResolvedModel {
        provider: default_provider.to_string(),
        model: model.to_string(),
        config: provider_cfg,
    })
}

fn split_model(model: &str) -> (Option<&str>, Option<&str>) {
    if let Some((provider, model_name)) = model.split_once('/') {
        return (Some(provider), Some(model_name));
    }
    (None, None)
}

fn default_provider_config(provider: &str) -> Option<ProviderConfig> {
    let mut cfg = ProviderConfig::default();
    match provider {
        "openai" => {
            cfg.base_url = Some("https://api.openai.com/v1".to_string());
            cfg.api_key_env = Some("OPENAI_API_KEY".to_string());
            cfg.kind = ProviderKind::OpenAICompatible;
            Some(cfg)
        }
        "openrouter" => {
            cfg.base_url = Some("https://openrouter.ai/api/v1".to_string());
            cfg.api_key_env = Some("OPENROUTER_API_KEY".to_string());
            cfg.kind = ProviderKind::OpenAICompatible;
            Some(cfg)
        }
        "anthropic" => {
            cfg.base_url = Some("https://api.anthropic.com".to_string());
            cfg.api_key_env = Some("ANTHROPIC_API_KEY".to_string());
            cfg.kind = ProviderKind::Anthropic;
            Some(cfg)
        }
        "gemini" => {
            cfg.base_url = Some("https://generativelanguage.googleapis.com/v1beta".to_string());
            cfg.api_key_env = Some("GEMINI_API_KEY".to_string());
            cfg.kind = ProviderKind::Gemini;
            Some(cfg)
        }
        "xai" => {
            cfg.base_url = Some("https://api.x.ai/v1".to_string());
            cfg.api_key_env = Some("XAI_API_KEY".to_string());
            cfg.kind = ProviderKind::OpenAICompatible;
            Some(cfg)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    #[test]
    fn resolve_with_prefix() {
        let config = Config::default();
        let resolved = resolve_model("openai/gpt-5.2", &config).unwrap();
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "gpt-5.2");
    }

    #[test]
    fn resolve_with_default_provider() {
        let config = Config {
            default_provider: Some("openai".to_string()),
            ..Config::default()
        };
        let resolved = resolve_model("gpt-5.2", &config).unwrap();
        assert_eq!(resolved.provider, "openai");
        assert_eq!(resolved.model, "gpt-5.2");
    }
}
