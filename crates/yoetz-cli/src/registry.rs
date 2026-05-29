use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde_json::Value;
use std::collections::HashSet;
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
use yoetz_core::registry::{ModelCapability, ModelEntry, ModelKind, ModelPricing, ModelRegistry};

pub struct RegistryFetchResult {
    pub registry: ModelRegistry,
    pub warnings: Vec<String>,
}

struct OpenRouterCatalog {
    registry: ModelRegistry,
    live_ids: HashSet<String>,
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
    let mut openrouter_live_ids: Option<HashSet<String>> = None;

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
        Ok(Some(openrouter)) => {
            openrouter_live_ids = merge_openrouter_catalog(&mut registry, openrouter);
            if openrouter_live_ids.is_none() {
                warnings.push(
                    "openrouter returned empty catalog; authoritative prune skipped".to_string(),
                );
            }
        }
        Ok(None) => warnings.push("openrouter skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("openrouter failed: {err}")),
    }

    match fetch_litellm(client, config).await {
        Ok(Some(litellm)) => registry.merge(litellm),
        Ok(None) => warnings.push("litellm skipped: missing API key".to_string()),
        Err(err) => warnings.push(format!("litellm failed: {err}")),
    }

    prune_openrouter_to_live_catalog(&mut registry, openrouter_live_ids.as_ref());

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
            id: canonical_embedded_model_id(&name).to_string(),
            context_length,
            max_output_tokens,
            pricing: ModelPricing {
                prompt_per_1k: pricing.input_cost_per_1k,
                completion_per_1k: pricing.output_cost_per_1k,
                request: None,
            },
            provider: pricing.provider.clone(),
            capability: pricing
                .mode
                .as_deref()
                .and_then(ModelKind::from_litellm_mode)
                .map(|kind| ModelCapability {
                    kind: Some(kind),
                    ..Default::default()
                }),
            tier: None,
        });
    }
    registry.rebuild_index();
    Ok(registry)
}

fn canonical_embedded_model_id(id: &str) -> &str {
    id.strip_prefix("openrouter/").unwrap_or(id)
}

async fn fetch_openrouter(client: &Client, config: &Config) -> Result<Option<OpenRouterCatalog>> {
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

fn parse_openrouter_models(value: &Value) -> OpenRouterCatalog {
    let mut registry = ModelRegistry::default();
    let mut live_ids = HashSet::new();
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
        live_ids.insert(id.to_string());
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
            tier: None,
        });
    }

    registry.rebuild_index();
    OpenRouterCatalog { registry, live_ids }
}

fn merge_openrouter_catalog(
    registry: &mut ModelRegistry,
    catalog: OpenRouterCatalog,
) -> Option<HashSet<String>> {
    let live_ids = if catalog.live_ids.is_empty() {
        None
    } else {
        Some(catalog.live_ids)
    };
    registry.merge(catalog.registry);
    live_ids
}

fn prune_openrouter_to_live_catalog(
    registry: &mut ModelRegistry,
    live_ids: Option<&HashSet<String>>,
) {
    let Some(live_ids) = live_ids else {
        return;
    };
    if live_ids.is_empty() {
        return;
    }
    registry.prune_provider("openrouter", live_ids);
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
            tier: None,
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

    if let Some(out) = item
        .get("architecture")
        .and_then(|v| v.get("output_modalities"))
        .and_then(|v| v.as_array())
    {
        let mods: Vec<String> = out
            .iter()
            .filter_map(|m| m.as_str())
            .map(|s| s.to_ascii_lowercase())
            .collect();
        cap.kind = classify_output_modalities(&mods);
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

    if cap.vision.is_none()
        && cap.reasoning.is_none()
        && cap.web_search.is_none()
        && cap.kind.is_none()
    {
        None
    } else {
        Some(cap)
    }
}

/// Classify a model's OpenRouter `architecture.output_modalities` into a kind.
/// Chat is exactly `["text"]`. Any non-empty list that is not exactly `["text"]`
/// carries a non-text output modality and is therefore non-chat — classified
/// specifically when recognized (image/video/audio) and `Other` otherwise.
/// Returns `None` (fail-open, chat-eligible) only when the list is empty, i.e.
/// the catalog gave no output-modality signal at all.
fn classify_output_modalities(mods: &[String]) -> Option<ModelKind> {
    let has = |needle: &str| mods.iter().any(|m| m == needle);
    if mods.is_empty() {
        return None;
    }
    if mods.len() == 1 && has("text") {
        return Some(ModelKind::Chat);
    }
    if has("image") {
        return Some(ModelKind::ImageGeneration);
    }
    if has("video") {
        return Some(ModelKind::VideoGeneration);
    }
    if has("audio") {
        return Some(ModelKind::Audio);
    }
    // Non-empty and not exactly ["text"], with no recognized media token: still a
    // non-text output, so it is non-chat (excluded from frontier), not fail-open.
    Some(ModelKind::Other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::io::Read as _;
    use yoetz_core::config::{ProviderConfig, RegistryConfig};

    struct EnvGuard {
        key: &'static str,
        old: Option<OsString>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let old = env::var_os(key);
            env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = self.old.take() {
                env::set_var(self.key, old);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn registry_with_models(models: Vec<ModelEntry>) -> ModelRegistry {
        let mut registry = ModelRegistry::default();
        registry.models = models;
        registry.rebuild_index();
        registry
    }

    fn model_entry(id: &str, provider: Option<&str>) -> ModelEntry {
        ModelEntry {
            id: id.to_string(),
            provider: provider.map(ToString::to_string),
            ..Default::default()
        }
    }

    fn serve_json_once(body: &'static str) -> String {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        format!("http://{addr}/models")
    }

    fn mods(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn classify_output_modalities_matches_openrouter_catalog_shapes() {
        // Verified live against OpenRouter /models (2026-05-29): output tokens
        // are only text/image/audio, and every media model also carries "text"
        // (so exactly ["text"] is the chat predicate).
        assert_eq!(
            classify_output_modalities(&mods(&["text"])),
            Some(ModelKind::Chat)
        );
        assert_eq!(
            classify_output_modalities(&mods(&["image", "text"])),
            Some(ModelKind::ImageGeneration)
        );
        assert_eq!(
            classify_output_modalities(&mods(&["audio", "text"])),
            Some(ModelKind::Audio)
        );
        assert_eq!(
            classify_output_modalities(&mods(&["text", "video"])),
            Some(ModelKind::VideoGeneration)
        );
        // Non-empty and not exactly ["text"], no recognized media token: still a
        // non-text output -> non-chat (Other), NOT fail-open chat-eligible.
        assert_eq!(
            classify_output_modalities(&mods(&["text", "speech"])),
            Some(ModelKind::Other)
        );
        assert_eq!(
            classify_output_modalities(&mods(&["embeddings"])),
            Some(ModelKind::Other)
        );
        // Only a truly empty list (no output-modality signal) is fail-open.
        assert_eq!(classify_output_modalities(&[]), None);
    }

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

    #[test]
    fn embedded_gemini_registry_strips_openrouter_prefix() {
        let registry = embedded_gemini_registry().unwrap();

        assert!(registry
            .models
            .iter()
            .all(|model| !model.id.starts_with("openrouter/")));

        if let Some(entry) = registry.find("google/gemini-3-pro-preview") {
            assert_eq!(entry.provider.as_deref(), Some("openrouter"));
        }
    }

    #[test]
    fn parse_openrouter_models_returns_live_ids_in_natural_namespace() {
        let catalog = parse_openrouter_models(&json!({
            "data": [{
                "id": "google/gemini-3.1-pro-preview",
                "context_length": 1048576,
                "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
                "pricing": {"prompt": "0.00000125", "completion": "0.00001"},
                "top_provider": {"max_completion_tokens": 65536}
            }]
        }));

        assert!(catalog.live_ids.contains("google/gemini-3.1-pro-preview"));
        assert!(catalog
            .registry
            .find("google/gemini-3.1-pro-preview")
            .is_some());
        assert!(catalog
            .registry
            .find("openrouter/google/gemini-3.1-pro-preview")
            .is_none());
    }

    #[test]
    fn openrouter_reconcile_prunes_dead_provider_rows_after_later_merges() {
        let mut registry = registry_with_models(vec![
            model_entry("google/gemini-3-pro-preview", Some("openrouter")),
            model_entry("gemini/gemini-3-pro-preview", Some("gemini")),
        ]);
        let catalog = parse_openrouter_models(&json!({
            "data": [{
                "id": "google/gemini-3.1-pro-preview",
                "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
                "pricing": {"prompt": "0.000001", "completion": "0.000002"}
            }]
        }));

        let live_ids = merge_openrouter_catalog(&mut registry, catalog);
        registry.merge(registry_with_models(vec![model_entry(
            "google/stale-from-later-source",
            Some("openrouter"),
        )]));
        prune_openrouter_to_live_catalog(&mut registry, live_ids.as_ref());

        assert!(registry.find("google/gemini-3-pro-preview").is_none());
        assert!(registry.find("google/stale-from-later-source").is_none());
        assert!(registry.find("google/gemini-3.1-pro-preview").is_some());
        assert!(registry.find("gemini/gemini-3-pro-preview").is_some());
    }

    #[test]
    fn openrouter_reconcile_does_not_prune_without_non_empty_live_catalog() {
        let mut registry = registry_with_models(vec![
            model_entry("google/gemini-3-pro-preview", Some("openrouter")),
            model_entry("google/old-model", Some("openrouter")),
        ]);

        prune_openrouter_to_live_catalog(&mut registry, None);
        assert!(registry.find("google/gemini-3-pro-preview").is_some());
        assert!(registry.find("google/old-model").is_some());

        let live_ids = merge_openrouter_catalog(
            &mut registry,
            OpenRouterCatalog {
                registry: ModelRegistry::default(),
                live_ids: Default::default(),
            },
        );
        prune_openrouter_to_live_catalog(&mut registry, live_ids.as_ref());

        assert!(registry.find("google/gemini-3-pro-preview").is_some());
        assert!(registry.find("google/old-model").is_some());
    }

    #[tokio::test]
    #[serial]
    async fn fetch_registry_uses_openrouter_fixture_as_authoritative_catalog() {
        let _env = EnvGuard::set("YOETZ_TEST_OPENROUTER_KEY", "test-key");
        let mut config = Config {
            registry: RegistryConfig {
                openrouter_models_url: Some(serve_json_once(
                    r#"{"data":[{"id":"google/gemini-3.1-pro-preview","architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},"pricing":{"prompt":"0.000001","completion":"0.000002"}}]}"#,
                )),
                ..Default::default()
            },
            ..Default::default()
        };
        config.providers.insert(
            "openrouter".to_string(),
            ProviderConfig {
                api_key_env: Some("YOETZ_TEST_OPENROUTER_KEY".to_string()),
                ..Default::default()
            },
        );

        let fetch = fetch_registry(&Client::new(), &config).await.unwrap();

        assert!(fetch
            .registry
            .find("google/gemini-3.1-pro-preview")
            .is_some());
        assert!(fetch
            .registry
            .find("openrouter/google/gemini-3.1-pro-preview")
            .is_none());
        assert!(fetch.registry.find("google/gemini-3-pro-preview").is_none());
    }
}
