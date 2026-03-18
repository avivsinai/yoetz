use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::home_dir;

/// Top-level yoetz configuration loaded from TOML files.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub defaults: Defaults,
    pub providers: HashMap<String, ProviderConfig>,
    pub registry: RegistryConfig,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

/// Default values for provider, model, and output settings.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Defaults {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub max_output_tokens: Option<usize>,
    pub browser_profile: Option<String>,
    pub browser_cdp: Option<String>,
}

/// Configuration for a single LLM provider (base URL, API key, kind).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub kind: Option<String>,
}

/// URLs and paths for model registry sources (OpenRouter, LiteLLM, org).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    pub openrouter_models_url: Option<String>,
    pub litellm_models_url: Option<String>,
    pub org_registry_path: Option<String>,
    /// Auto-sync interval in seconds. Default 86400 (24h). Set to 0 to disable.
    pub auto_sync_secs: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    pub defaults: Option<Defaults>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub registry: Option<RegistryConfig>,
    pub aliases: Option<HashMap<String, String>>,
}

impl Config {
    /// Load configuration by merging all config files in precedence order.
    pub fn load() -> Result<Self> {
        Self::load_with_profile(None)
    }

    /// Load configuration with an optional profile overlay.
    pub fn load_with_profile(profile: Option<&str>) -> Result<Self> {
        let mut config = Config::default();
        for (path, trusted) in default_config_paths(profile) {
            if path.exists() {
                let file = load_config_file(&path)?;
                config.merge(file, trusted, &path);
            }
        }
        Ok(config)
    }

    fn merge(&mut self, other: ConfigFile, trusted: bool, source: &Path) {
        if let Some(defaults) = other.defaults {
            merge_defaults(&mut self.defaults, defaults);
        }
        if let Some(providers) = other.providers {
            if trusted {
                for (k, v) in providers {
                    self.providers
                        .entry(k)
                        .and_modify(|existing| merge_provider(existing, &v))
                        .or_insert(v);
                }
            } else {
                eprintln!(
                    "warning: ignoring [providers] from untrusted config {}",
                    source.display()
                );
            }
        }
        if let Some(registry) = other.registry {
            if trusted {
                merge_registry(&mut self.registry, registry);
            } else {
                eprintln!(
                    "warning: ignoring [registry] from untrusted config {}",
                    source.display()
                );
            }
        }
        if let Some(aliases) = other.aliases {
            self.aliases.extend(aliases);
        }
    }
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read config {}", path.display()))?;
    let parsed: ConfigFile =
        toml::from_str(&content).with_context(|| format!("parse config {}", path.display()))?;
    Ok(parsed)
}

/// Returns `(path, trusted)` pairs. Paths under the user's home config dirs and
/// `YOETZ_CONFIG_PATH` are trusted; CWD-relative paths (repo-local) are untrusted.
fn default_config_paths(profile: Option<&str>) -> Vec<(PathBuf, bool)> {
    let mut paths: Vec<(PathBuf, bool)> = Vec::new();

    if let Some(home) = home_dir() {
        paths.push((home.join(".yoetz/config.toml"), true));
        paths.push((home.join(".config/yoetz/config.toml"), true));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        paths.push((PathBuf::from(xdg).join("yoetz/config.toml"), true));
    }
    // Repo-local config — untrusted (may come from a cloned repo)
    paths.push((PathBuf::from("./yoetz.toml"), false));

    if let Ok(custom) = env::var("YOETZ_CONFIG_PATH") {
        paths.push((PathBuf::from(custom), true));
    }

    if let Some(name) = profile {
        if let Some(home) = home_dir() {
            paths.push((
                home.join(".yoetz/profiles").join(format!("{name}.toml")),
                true,
            ));
            paths.push((
                home.join(".config/yoetz/profiles")
                    .join(format!("{name}.toml")),
                true,
            ));
        }
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            paths.push((
                PathBuf::from(xdg)
                    .join("yoetz/profiles")
                    .join(format!("{name}.toml")),
                true,
            ));
        }
        // Repo-local profile config — untrusted
        paths.push((PathBuf::from(format!("./yoetz.{name}.toml")), false));
    }
    paths
}

fn merge_defaults(target: &mut Defaults, other: Defaults) {
    if other.profile.is_some() {
        target.profile = other.profile;
    }
    if other.model.is_some() {
        target.model = other.model;
    }
    if other.provider.is_some() {
        target.provider = other.provider;
    }
    if other.max_output_tokens.is_some() {
        target.max_output_tokens = other.max_output_tokens;
    }
    if other.browser_profile.is_some() {
        target.browser_profile = other.browser_profile;
    }
    if other.browser_cdp.is_some() {
        target.browser_cdp = other.browser_cdp;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_with_browser_cdp() {
        let toml_str = r#"
[defaults]
browser_cdp = "http://127.0.0.1:9222"
"#;
        let file: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(
            file.defaults.unwrap().browser_cdp.as_deref(),
            Some("http://127.0.0.1:9222")
        );
    }

    #[test]
    fn parse_config_without_browser_cdp_defaults_to_none() {
        let toml_str = r#"
[defaults]
model = "gpt-5-4-pro"
"#;
        let file: ConfigFile = toml::from_str(toml_str).unwrap();
        assert!(file.defaults.unwrap().browser_cdp.is_none());
    }

    #[test]
    fn merge_defaults_browser_cdp() {
        let mut target = Defaults::default();
        let other = Defaults {
            browser_cdp: Some("http://127.0.0.1:9222".to_string()),
            ..Default::default()
        };
        merge_defaults(&mut target, other);
        assert_eq!(target.browser_cdp.as_deref(), Some("http://127.0.0.1:9222"));
    }

    #[test]
    fn untrusted_config_skips_providers_and_registry() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: Some(Defaults {
                model: Some("gpt-5-4-pro".to_string()),
                ..Default::default()
            }),
            providers: Some(HashMap::from([(
                "evil".to_string(),
                ProviderConfig {
                    base_url: Some("http://evil.example.com".to_string()),
                    api_key_env: Some("EVIL_KEY".to_string()),
                    kind: None,
                },
            )])),
            registry: Some(RegistryConfig {
                openrouter_models_url: Some("http://evil.example.com/models".to_string()),
                ..Default::default()
            }),
            aliases: Some(HashMap::from([(
                "fast".to_string(),
                "gpt-5-4-pro".to_string(),
            )])),
        };
        config.merge(file, false, Path::new("./yoetz.toml"));
        // Safe fields applied
        assert_eq!(config.defaults.model.as_deref(), Some("gpt-5-4-pro"));
        assert_eq!(
            config.aliases.get("fast").map(|s| s.as_str()),
            Some("gpt-5-4-pro")
        );
        // Restricted fields skipped
        assert!(config.providers.is_empty());
        assert!(config.registry.openrouter_models_url.is_none());
    }

    #[test]
    fn trusted_config_applies_providers_and_registry() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: None,
            providers: Some(HashMap::from([(
                "openai".to_string(),
                ProviderConfig {
                    base_url: Some("https://api.openai.com".to_string()),
                    api_key_env: Some("OPENAI_API_KEY".to_string()),
                    kind: None,
                },
            )])),
            registry: Some(RegistryConfig {
                openrouter_models_url: Some("https://openrouter.ai/api/v1/models".to_string()),
                ..Default::default()
            }),
            aliases: None,
        };
        config.merge(
            file,
            true,
            Path::new("/home/user/.config/yoetz/config.toml"),
        );
        assert!(config.providers.contains_key("openai"));
        assert!(config.registry.openrouter_models_url.is_some());
    }
}

fn merge_provider(target: &mut ProviderConfig, other: &ProviderConfig) {
    if other.base_url.is_some() {
        target.base_url = other.base_url.clone();
    }
    if other.api_key_env.is_some() {
        target.api_key_env = other.api_key_env.clone();
    }
    if other.kind.is_some() {
        target.kind = other.kind.clone();
    }
}

fn merge_registry(target: &mut RegistryConfig, other: RegistryConfig) {
    if other.openrouter_models_url.is_some() {
        target.openrouter_models_url = other.openrouter_models_url;
    }
    if other.litellm_models_url.is_some() {
        target.litellm_models_url = other.litellm_models_url;
    }
    if other.org_registry_path.is_some() {
        target.org_registry_path = other.org_registry_path;
    }
    if other.auto_sync_secs.is_some() {
        target.auto_sync_secs = other.auto_sync_secs;
    }
}
