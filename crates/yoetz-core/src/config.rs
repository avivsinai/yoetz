use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::paths::home_dir;
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub defaults: Defaults,
    pub providers: HashMap<String, ProviderConfig>,
    pub registry: RegistryConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Defaults {
    pub profile: Option<String>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub max_output_tokens: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ProviderConfig {
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RegistryConfig {
    pub openrouter_models_url: Option<String>,
    pub litellm_models_url: Option<String>,
    pub org_registry_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    pub defaults: Option<Defaults>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub registry: Option<RegistryConfig>,
}

impl Config {
    pub fn load() -> Result<Self> {
        Self::load_with_profile(None)
    }

    pub fn load_with_profile(profile: Option<&str>) -> Result<Self> {
        let mut config = Config::default();
        for path in default_config_paths(profile) {
            if path.exists() {
                let file = load_config_file(&path)?;
                config.merge(file);
            }
        }
        Ok(config)
    }

    fn merge(&mut self, other: ConfigFile) {
        if let Some(defaults) = other.defaults {
            merge_defaults(&mut self.defaults, defaults);
        }
        if let Some(providers) = other.providers {
            for (k, v) in providers {
                self.providers
                    .entry(k)
                    .and_modify(|existing| merge_provider(existing, &v))
                    .or_insert(v);
            }
        }
        if let Some(registry) = other.registry {
            merge_registry(&mut self.registry, registry);
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

fn default_config_paths(profile: Option<&str>) -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = home_dir() {
        paths.push(home.join(".yoetz/config.toml"));
        paths.push(home.join(".config/yoetz/config.toml"));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("yoetz/config.toml"));
    }
    paths.push(PathBuf::from("./yoetz.toml"));

    if let Ok(custom) = env::var("YOETZ_CONFIG_PATH") {
        paths.push(PathBuf::from(custom));
    }

    if let Some(name) = profile {
        if let Some(home) = home_dir() {
            paths.push(home.join(".yoetz/profiles").join(format!("{name}.toml")));
            paths.push(
                home.join(".config/yoetz/profiles")
                    .join(format!("{name}.toml")),
            );
        }
        if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
            paths.push(
                PathBuf::from(xdg)
                    .join("yoetz/profiles")
                    .join(format!("{name}.toml")),
            );
        }
        paths.push(PathBuf::from(format!("./yoetz.{name}.toml")));
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
}
