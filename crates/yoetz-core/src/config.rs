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
    pub frontier: FrontierConfig,
    pub notifications: NotificationsConfig,
    pub sessions: SessionsConfig,
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

/// Controls which provider families appear in the default frontier view.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FrontierConfig {
    pub families: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NotificationsConfig {
    pub enabled: Option<bool>,
    pub notify_threshold_secs: Option<u64>,
}

/// Session artifact lifecycle: opt-out of session writes and retention limits.
/// Only honored from trusted config sources: retention deletes user data and
/// `no_session` suppresses audit artifacts, so repo-local configs must not
/// control it.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionsConfig {
    /// Skip creating session directories for `yoetz ask` (like `--no-session`).
    pub no_session: Option<bool>,
    /// Prune session dirs whose mtime is older than this many days.
    pub max_age_days: Option<u64>,
    /// Keep at most this many newest session dirs.
    pub max_count: Option<usize>,
}

impl SessionsConfig {
    pub fn retention_enabled(&self) -> bool {
        self.max_age_days.is_some() || self.max_count.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct ConfigFile {
    pub defaults: Option<Defaults>,
    pub providers: Option<HashMap<String, ProviderConfig>>,
    pub registry: Option<RegistryConfig>,
    pub frontier: Option<FrontierConfig>,
    pub notifications: Option<NotificationsConfig>,
    pub sessions: Option<SessionsConfig>,
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
            let defaults = if trusted {
                defaults
            } else {
                sanitize_untrusted_defaults(defaults, source)
            };
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
        if let Some(frontier) = other.frontier {
            if trusted {
                merge_frontier(&mut self.frontier, frontier);
            } else {
                eprintln!(
                    "warning: ignoring [frontier] from untrusted config {}",
                    source.display()
                );
            }
        }
        if let Some(notifications) = other.notifications {
            merge_notifications(&mut self.notifications, notifications);
        }
        if let Some(sessions) = other.sessions {
            if trusted {
                merge_sessions(&mut self.sessions, sessions);
            } else {
                eprintln!(
                    "warning: ignoring [sessions] from untrusted config {}",
                    source.display()
                );
            }
        }
        if let Some(aliases) = other.aliases {
            if trusted {
                self.aliases.extend(aliases);
            } else {
                eprintln!(
                    "warning: ignoring [aliases] from untrusted config {}",
                    source.display()
                );
            }
        }
    }
}

fn sanitize_untrusted_defaults(mut defaults: Defaults, source: &Path) -> Defaults {
    warn_and_clear_untrusted_default(&mut defaults.profile, "profile", source);
    warn_and_clear_untrusted_default(&mut defaults.model, "model", source);
    warn_and_clear_untrusted_default(&mut defaults.provider, "provider", source);
    warn_and_clear_untrusted_default(&mut defaults.max_output_tokens, "max_output_tokens", source);
    defaults
}

fn warn_and_clear_untrusted_default<T>(slot: &mut Option<T>, field: &str, source: &Path) {
    if slot.take().is_some() {
        eprintln!(
            "warning: ignoring defaults.{field} from untrusted config {}",
            source.display()
        );
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_defaults_without_browser_fields() {
        let toml_str = r#"
[defaults]
model = "gpt-5-4-pro"
"#;
        let file: ConfigFile = toml::from_str(toml_str).unwrap();
        assert_eq!(file.defaults.unwrap().model.as_deref(), Some("gpt-5-4-pro"));
    }

    #[test]
    fn untrusted_config_skips_providers_and_registry() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: Some(Defaults {
                profile: Some("repo-profile".to_string()),
                model: Some("gpt-5-4-pro".to_string()),
                provider: Some("evil".to_string()),
                max_output_tokens: Some(99_999),
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
            frontier: Some(FrontierConfig {
                families: Some(vec!["evil".to_string()]),
            }),
            notifications: None,
            sessions: Some(SessionsConfig {
                no_session: Some(true),
                max_age_days: Some(1),
                max_count: Some(1),
            }),
            aliases: Some(HashMap::from([(
                "fast".to_string(),
                "gpt-5-4-pro".to_string(),
            )])),
        };
        config.merge(file, false, Path::new("./yoetz.toml"));
        assert!(config.defaults.profile.is_none());
        assert!(config.defaults.model.is_none());
        assert!(config.defaults.provider.is_none());
        assert!(config.defaults.max_output_tokens.is_none());
        assert!(config.aliases.is_empty());
        // Restricted fields skipped
        assert!(config.providers.is_empty());
        assert!(config.registry.openrouter_models_url.is_none());
        assert!(config.frontier.families.is_none());
        assert!(config.notifications.enabled.is_none());
        // [sessions] controls data deletion and artifact suppression; untrusted
        // repo-local configs must not set it.
        assert!(config.sessions.no_session.is_none());
        assert!(config.sessions.max_age_days.is_none());
        assert!(config.sessions.max_count.is_none());
        assert!(!config.sessions.retention_enabled());
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
            frontier: None,
            notifications: None,
            sessions: None,
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

    #[test]
    fn notifications_merge_from_any_config_scope() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: None,
            providers: None,
            registry: None,
            frontier: None,
            notifications: Some(NotificationsConfig {
                enabled: Some(false),
                notify_threshold_secs: Some(90),
            }),
            sessions: None,
            aliases: None,
        };
        config.merge(file, false, Path::new("./yoetz.toml"));
        assert_eq!(config.notifications.enabled, Some(false));
        assert_eq!(config.notifications.notify_threshold_secs, Some(90));
    }

    #[test]
    fn trusted_config_applies_frontier_families() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: None,
            providers: None,
            registry: None,
            frontier: Some(FrontierConfig {
                families: Some(vec!["openai".to_string(), "z-ai".to_string()]),
            }),
            notifications: None,
            sessions: None,
            aliases: None,
        };

        config.merge(
            file,
            true,
            Path::new("/home/user/.config/yoetz/config.toml"),
        );

        assert_eq!(
            config.frontier.families,
            Some(vec!["openai".to_string(), "z-ai".to_string()])
        );
    }

    #[test]
    fn trusted_config_applies_sessions() {
        let mut config = Config::default();
        let file = ConfigFile {
            defaults: None,
            providers: None,
            registry: None,
            frontier: None,
            notifications: None,
            sessions: Some(SessionsConfig {
                no_session: Some(true),
                max_age_days: Some(30),
                max_count: Some(100),
            }),
            aliases: None,
        };

        config.merge(
            file,
            true,
            Path::new("/home/user/.config/yoetz/config.toml"),
        );

        assert_eq!(config.sessions.no_session, Some(true));
        assert_eq!(config.sessions.max_age_days, Some(30));
        assert_eq!(config.sessions.max_count, Some(100));
        assert!(config.sessions.retention_enabled());
    }

    #[test]
    fn parse_sessions_from_toml() {
        let toml_str = r#"
[sessions]
no_session = true
max_age_days = 14
max_count = 50
"#;
        let file: ConfigFile = toml::from_str(toml_str).unwrap();
        let sessions = file.sessions.unwrap();
        assert_eq!(sessions.no_session, Some(true));
        assert_eq!(sessions.max_age_days, Some(14));
        assert_eq!(sessions.max_count, Some(50));
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

fn merge_frontier(target: &mut FrontierConfig, other: FrontierConfig) {
    if other.families.is_some() {
        target.families = other.families;
    }
}

fn merge_notifications(target: &mut NotificationsConfig, other: NotificationsConfig) {
    if other.enabled.is_some() {
        target.enabled = other.enabled;
    }
    if other.notify_threshold_secs.is_some() {
        target.notify_threshold_secs = other.notify_threshold_secs;
    }
}

fn merge_sessions(target: &mut SessionsConfig, other: SessionsConfig) {
    if other.no_session.is_some() {
        target.no_session = other.no_session;
    }
    if other.max_age_days.is_some() {
        target.max_age_days = other.max_age_days;
    }
    if other.max_count.is_some() {
        target.max_count = other.max_count;
    }
}
