use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

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
        let mut config = Config::default();
        for path in default_config_paths() {
            if path.exists() {
                let file = load_config_file(&path)?;
                config.merge(file);
            }
        }
        Ok(config)
    }

    fn merge(&mut self, other: ConfigFile) {
        if let Some(defaults) = other.defaults {
            self.defaults = defaults;
        }
        if let Some(providers) = other.providers {
            for (k, v) in providers {
                self.providers.insert(k, v);
            }
        }
        if let Some(registry) = other.registry {
            self.registry = registry;
        }
    }
}

fn load_config_file(path: &Path) -> Result<ConfigFile> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("read config {}", path.display()))?;
    let parsed: ConfigFile = toml::from_str(&content)
        .with_context(|| format!("parse config {}", path.display()))?;
    Ok(parsed)
}

fn default_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();

    if let Some(home) = home_dir() {
        paths.push(home.join(".yoetz/config.toml"));
        paths.push(home.join(".config/yoetz/config.toml"));
    }
    if let Ok(xdg) = env::var("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg).join("yoetz/config.toml"));
    }
    paths.push(PathBuf::from("./yoetz.toml"));
    paths
}

fn home_dir() -> Option<PathBuf> {
    env::var("HOME").map(PathBuf::from).ok()
}
