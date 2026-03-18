use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::http::send_json;
use litellm_rust::registry::Registry as EmbeddedRegistry;
use yoetz_core::config::Config;
use yoetz_core::paths::home_dir;
use yoetz_core::registry::{ModelCapability, ModelEntry, ModelPricing, ModelRegistry};

pub struct RegistryFetchResult {
    pub registry: ModelRegistry,
    pub warnings: Vec<String>,
}

pub fn registry_cache_path() -> PathBuf {
    if let Ok(path) = env::var("YOETZ_REGISTRY_PATH") {
        return PathBuf::from(path);
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz/registry.json");
    }
    PathBuf::from(".yoetz/registry.json")
}

pub fn load_registry_cache() -> Result<Option<ModelRegistry>> {
    let path = registry_cache_path();
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).with_context(|| format!("read registry {}", path.display()))?;
    let mut registry: ModelRegistry = serde_json::from_str(&content)?;
    registry.rebuild_index();
    Ok(Some(registry))
}

/// Default auto-sync interval: 24 hours.
const DEFAULT_AUTO_SYNC_SECS: u64 = 86400;

/// Returns true if the cached registry is stale (older than `auto_sync_secs`).
/// Returns true if no cache exists at all.
pub fn is_registry_stale(config: &Config) -> bool {
    is_cache_stale(&registry_cache_path(), config)
}

fn is_cache_stale(path: &Path, config: &Config) -> bool {
    let interval = config
        .registry
        .auto_sync_secs
        .unwrap_or(DEFAULT_AUTO_SYNC_SECS);
    if interval == 0 {
        return false; // auto-sync disabled
    }
    if !path.exists() {
        return true;
    }
    let Ok(meta) = fs::metadata(path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let age = modified.elapsed().unwrap_or_default();
    age.as_secs() >= interval
}

/// Load registry, auto-syncing if stale or corrupt.
/// Only overwrites the cache if the new registry is at least as large as the old one,
/// preventing degraded data from replacing a healthy cache.
pub async fn load_registry_with_auto_sync(
    client: &Client,
    config: &Config,
) -> Result<Option<ModelRegistry>> {
    let cached = load_registry_cache();
    let cache_corrupt = cached.is_err();
    let cached = cached.ok().flatten();

    let needs_sync = cache_corrupt || is_registry_stale(config);
    if needs_sync {
        match fetch_registry(client, config).await {
            Ok(fetch) => {
                let new_count = fetch.registry.models.len();
                let old_count = cached.as_ref().map_or(0, |r| r.models.len());

                if new_count >= old_count || cached.is_none() || cache_corrupt {
                    if let Err(e) = save_registry_cache(&fetch.registry) {
                        eprintln!("auto-sync: failed to save registry: {e}");
                    } else {
                        eprintln!("auto-sync: registry refreshed ({new_count} models)");
                    }
                    return Ok(Some(fetch.registry));
                } else {
                    // New registry is smaller (degraded fetch) — keep old cache, log warning.
                    // Touch mtime so we don't re-fetch on every subsequent invocation.
                    touch_registry_cache();
                    eprintln!(
                        "auto-sync: fetched {new_count} models (had {old_count}); \
                         keeping cached registry (run `yoetz models sync` to force)"
                    );
                    for w in &fetch.warnings {
                        eprintln!("auto-sync: {w}");
                    }
                    return Ok(cached);
                }
            }
            Err(e) => {
                // Touch mtime to avoid re-fetching on every invocation during outages.
                touch_registry_cache();
                eprintln!("auto-sync: refresh failed ({e}), using cached registry");
            }
        }
    }
    Ok(cached)
}

/// Touch the registry cache file's mtime to reset the staleness timer.
fn touch_registry_cache() {
    let path = registry_cache_path();
    if let Ok(file) = fs::OpenOptions::new().write(true).open(&path) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

pub fn save_registry_cache(registry: &ModelRegistry) -> Result<PathBuf> {
    let path = registry_cache_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_string_pretty(registry)?;
    let mut tmp =
        NamedTempFile::new_in(path.parent().unwrap_or_else(|| std::path::Path::new(".")))?;
    tmp.write_all(data.as_bytes())?;
    tmp.persist(&path)
        .map_err(|e| anyhow!("write registry {}: {}", path.display(), e))?;
    Ok(path)
}

pub async fn fetch_registry(client: &Client, config: &Config) -> Result<RegistryFetchResult> {
    let mut registry = ModelRegistry::default();
    let mut warnings = Vec::new();

    // Embedded Gemini registry merged first as a low-priority fallback.
    // Dynamic sources (OpenRouter, LiteLLM) merged after and override these entries.
    match embedded_gemini_registry() {
        Ok(embedded) => registry.merge(embedded),
        Err(err) => warnings.push(format!("embedded gemini registry skipped: {err}")),
    }

    if let Some(org_path) = config.registry.org_registry_path.as_deref() {
        if Path::new(org_path).exists() {
            let content = fs::read_to_string(org_path)?;
            let org_registry: ModelRegistry = serde_json::from_str(&content)?;
            registry.merge(org_registry);
        } else {
            warnings.push(format!("org registry not found: {org_path}"));
        }
    }

    match fetch_openrouter(client, config).await {
        Ok(Some(openrouter)) => registry.merge(openrouter),
        Ok(None) => warnings.push("openrouter skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("openrouter failed: {err}")),
    }

    match fetch_litellm(client, config).await {
        Ok(Some(litellm)) => registry.merge(litellm),
        Ok(None) => warnings.push("litellm skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("litellm failed: {err}")),
    }

    registry.updated_at = Some(
        OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_default(),
    );
    if registry.version == 0 {
        registry.version = 1;
    }
    registry.rebuild_index();

    Ok(RegistryFetchResult { registry, warnings })
}

fn embedded_gemini_registry() -> Result<ModelRegistry> {
    let embedded =
        EmbeddedRegistry::load_embedded().map_err(|e| anyhow!("load embedded registry: {e}"))?;
    let mut registry = ModelRegistry::default();
    for (name, pricing) in embedded.models.into_iter() {
        let name_lc = name.to_lowercase();
        let provider_lc = pricing.provider.as_deref().unwrap_or("").to_lowercase();
        let is_gemini = name_lc.contains("gemini")
            || name_lc.contains("veo")
            || provider_lc.contains("gemini")
            || provider_lc.contains("google");
        if !is_gemini {
            continue;
        }

        let context_length = pricing.max_input_tokens.map(|v| v as usize);
        let max_output_tokens = pricing.max_output_tokens.map(|v| v as usize);

        registry.models.push(ModelEntry {
            id: name,
            context_length,
            max_output_tokens,
            pricing: ModelPricing {
                prompt_per_1k: pricing.input_cost_per_1k,
                completion_per_1k: pricing.output_cost_per_1k,
                request: None,
            },
            provider: pricing.provider.clone(),
            capability: None,
        });
    }
    registry.rebuild_index();
    Ok(registry)
}

async fn fetch_openrouter(client: &Client, config: &Config) -> Result<Option<ModelRegistry>> {
    let url = config
        .registry
        .openrouter_models_url
        .clone()
        .unwrap_or_else(|| "https://openrouter.ai/api/v1/models".to_string());

    let provider = config.providers.get("openrouter");
    let api_key_env = provider
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| "OPENROUTER_API_KEY".to_string());

    let api_key = match env::var(&api_key_env) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let (payload, _) = send_json::<Value>(client.get(&url).bearer_auth(api_key)).await?;
    Ok(Some(parse_openrouter_models(&payload)))
}

async fn fetch_litellm(client: &Client, config: &Config) -> Result<Option<ModelRegistry>> {
    let provider = config.providers.get("litellm");
    let api_key_env = provider
        .and_then(|p| p.api_key_env.clone())
        .unwrap_or_else(|| "LITELLM_API_KEY".to_string());

    let api_key = env::var(&api_key_env).ok();

    let urls = if let Some(url) = config.registry.litellm_models_url.clone() {
        vec![url]
    } else {
        vec![
            "http://localhost:4000/model/info".to_string(),
            "http://localhost:4000/v1/model/info".to_string(),
        ]
    };

    let mut last_err: Option<anyhow::Error> = None;
    for url in urls {
        let mut req = client.get(&url);
        if let Some(key) = api_key.as_deref() {
            req = req.bearer_auth(key);
        }
        match send_json::<Value>(req).await {
            Ok((payload, _)) => return Ok(Some(parse_litellm_models(&payload))),
            Err(err) => last_err = Some(err),
        }
    }

    if let Some(err) = last_err {
        return Err(err);
    }
    if api_key.is_none() {
        return Ok(None);
    }
    Ok(None)
}

fn parse_openrouter_models(value: &Value) -> ModelRegistry {
    let mut registry = ModelRegistry::default();
    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for item in data {
        let id = item.get("id").and_then(|v| v.as_str()).unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let context_length = item
            .get("context_length")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let pricing_obj = item.get("pricing");
        // OpenRouter pricing is USD per token; convert to USD per 1k tokens.
        let pricing = ModelPricing {
            prompt_per_1k: pricing_obj
                .and_then(|p| parse_price(p.get("prompt")))
                .map(|v| v * 1000.0),
            completion_per_1k: pricing_obj
                .and_then(|p| parse_price(p.get("completion")))
                .map(|v| v * 1000.0),
            request: pricing_obj.and_then(|p| parse_price(p.get("request"))),
        };

        let max_output_tokens = item
            .get("top_provider")
            .and_then(|v| v.get("max_completion_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let capability = parse_openrouter_capability(&item);
        registry.models.push(ModelEntry {
            id: id.to_string(),
            context_length,
            max_output_tokens,
            pricing,
            provider: Some("openrouter".to_string()),
            capability,
        });
    }

    registry.rebuild_index();
    registry
}

fn parse_litellm_models(value: &Value) -> ModelRegistry {
    let mut registry = ModelRegistry::default();
    let data = value
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for item in data {
        let model_name = item
            .get("model_name")
            .and_then(|v| v.as_str())
            .unwrap_or_default();

        let model_info = item.get("model_info").unwrap_or(&Value::Null);
        let id = if !model_name.is_empty() {
            model_name.to_string()
        } else {
            model_info
                .get("key")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        if id.is_empty() {
            continue;
        }

        let input_cost = parse_price(model_info.get("input_cost_per_token"));
        let output_cost = parse_price(model_info.get("output_cost_per_token"));
        let max_tokens = model_info
            .get("max_input_tokens")
            .or_else(|| model_info.get("max_tokens"))
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let max_output_tokens = model_info
            .get("max_output_tokens")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        registry.models.push(ModelEntry {
            id,
            context_length: max_tokens,
            max_output_tokens,
            pricing: ModelPricing {
                prompt_per_1k: input_cost.map(|v| v * 1000.0),
                completion_per_1k: output_cost.map(|v| v * 1000.0),
                request: None,
            },
            provider: Some("litellm".to_string()),
            capability: None,
        });
    }

    registry.rebuild_index();
    registry
}

fn parse_price(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(n) = v.as_f64() {
        return Some(n);
    }
    if let Some(s) = v.as_str() {
        return s.parse::<f64>().ok();
    }
    None
}

pub fn estimate_pricing(
    registry: Option<&ModelRegistry>,
    model_id: &str,
    input_tokens: usize,
    output_tokens: usize,
) -> Result<yoetz_core::types::PricingEstimate> {
    let mut estimate = yoetz_core::types::PricingEstimate::default();
    let Some(registry) = registry else {
        estimate
            .warnings
            .push("registry unavailable; run `yoetz models sync`".to_string());
        return Ok(estimate);
    };

    let entry = registry
        .find(model_id)
        .ok_or_else(|| anyhow!("model not found in registry: {model_id}"));
    if let Ok(entry) = entry {
        estimate.input_tokens = Some(input_tokens);
        estimate.output_tokens = Some(output_tokens);
        estimate.pricing_source = entry.provider.clone();
        estimate.estimate_usd = entry.pricing.estimate(input_tokens, output_tokens);
    } else {
        estimate.warnings.push(format!(
            "model not found in registry: {model_id}; run `yoetz models sync` to refresh"
        ));
    }

    Ok(estimate)
}

fn parse_openrouter_capability(item: &Value) -> Option<ModelCapability> {
    let mut cap = ModelCapability::default();

    if let Some(modalities) = item
        .get("architecture")
        .and_then(|v| v.get("input_modalities"))
        .and_then(|v| v.as_array())
    {
        let has_image = modalities
            .iter()
            .any(|m| m.as_str().is_some_and(|s| s.eq_ignore_ascii_case("image")));
        cap.vision = Some(has_image);
    }

    if let Some(params) = item.get("supported_parameters").and_then(|v| v.as_array()) {
        let has_reasoning = params.iter().any(|p| {
            p.as_str().is_some_and(|s| {
                s.eq_ignore_ascii_case("reasoning")
                    || s.eq_ignore_ascii_case("reasoning_effort")
                    || s.eq_ignore_ascii_case("include_reasoning")
                    || s.eq_ignore_ascii_case("thinking")
            })
        });
        if has_reasoning {
            cap.reasoning = Some(true);
        }
    }

    let web_search = item
        .get("pricing")
        .and_then(|v| v.get("web_search"))
        .and_then(|v| if v.is_null() { None } else { Some(v) })
        .is_some();
    if web_search {
        cap.web_search = Some(true);
    }

    if cap.vision.is_none() && cap.reasoning.is_none() && cap.web_search.is_none() {
        None
    } else {
        Some(cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yoetz_core::config::RegistryConfig;

    fn config_with_sync(secs: u64) -> Config {
        Config {
            registry: RegistryConfig {
                auto_sync_secs: Some(secs),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn stale_disabled_returns_false() {
        let config = config_with_sync(0);
        let path = PathBuf::from("/nonexistent");
        assert!(!is_cache_stale(&path, &config));
    }

    #[test]
    fn stale_missing_file_returns_true() {
        let config = config_with_sync(86400);
        let path = PathBuf::from("/tmp/yoetz_test_nonexistent_path.json");
        assert!(is_cache_stale(&path, &config));
    }

    #[test]
    fn stale_fresh_file_returns_false() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp.as_file(), b"{}").unwrap();
        let config = config_with_sync(86400);
        assert!(!is_cache_stale(tmp.path(), &config));
    }

    #[test]
    fn stale_old_file_returns_true() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut tmp.as_file(), b"{}").unwrap();
        let two_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86400);
        tmp.as_file().set_modified(two_days_ago).unwrap();
        let config = config_with_sync(86400);
        assert!(is_cache_stale(tmp.path(), &config));
    }
}
