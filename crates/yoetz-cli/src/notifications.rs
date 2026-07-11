use anyhow::Result;
use std::env;
use std::process::Command;

use yoetz_core::config::Config;

const DEFAULT_THRESHOLD_SECS: u64 = 60;
const PREVIEW_LIMIT: usize = 120;

pub(crate) fn should_notify_completion(
    runtime_ms: u64,
    threshold_secs: u64,
    is_macos: bool,
    muted_by_flag: bool,
    muted_by_env: bool,
    muted_by_config: bool,
) -> bool {
    if !is_macos {
        return false;
    }
    if muted_by_flag || muted_by_env || muted_by_config {
        return false;
    }
    runtime_ms >= threshold_secs.saturating_mul(1000)
}

pub(crate) fn notification_threshold_secs(config: &Config) -> u64 {
    config
        .notifications
        .notify_threshold_secs
        .unwrap_or(DEFAULT_THRESHOLD_SECS)
}

pub(crate) fn config_mutes_notifications(config: &Config) -> bool {
    config.notifications.enabled == Some(false)
}

pub(crate) fn env_mutes_notifications() -> bool {
    env::var_os("CI").is_some()
        || env::var_os("SSH_TTY").is_some()
        || env::var_os("SSH_CONNECTION").is_some()
        || env::var("YOETZ_NO_NOTIFY").ok().as_deref() == Some("1")
}

pub(crate) fn sanitize_preview(input: &str) -> String {
    let collapsed = input
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    collapsed.chars().take(PREVIEW_LIMIT).collect()
}

pub(crate) fn format_elapsed(elapsed_ms: u64) -> String {
    let seconds = elapsed_ms / 1000;
    if seconds < 60 {
        format!("{seconds}s")
    } else {
        let minutes = seconds / 60;
        let remainder = seconds % 60;
        if remainder == 0 {
            format!("{minutes}m")
        } else {
            format!("{minutes}m {remainder}s")
        }
    }
}

pub(crate) fn maybe_notify_completion(
    config: &Config,
    no_notify: bool,
    command: &str,
    target: &str,
    preview: &str,
    elapsed_ms: u64,
    cost_usd: Option<f64>,
    debug: bool,
) {
    let threshold_secs = notification_threshold_secs(config);
    if !should_notify_completion(
        elapsed_ms,
        threshold_secs,
        cfg!(target_os = "macos"),
        no_notify,
        env_mutes_notifications(),
        config_mutes_notifications(config),
    ) {
        return;
    }

    let title = format!("yoetz {command}");
    let mut subtitle = if target.is_empty() {
        format_elapsed(elapsed_ms)
    } else {
        format!("{target} • {}", format_elapsed(elapsed_ms))
    };
    if let Some(cost) = cost_usd {
        subtitle.push_str(&format!(" • ${cost:.2}"));
    }
    let message = {
        let preview = sanitize_preview(preview);
        if preview.is_empty() {
            "(no preview)".to_string()
        } else {
            preview
        }
    };

    if let Err(err) = send_macos_notification(&title, &subtitle, &message) {
        if debug {
            eprintln!("debug: completion notification failed: {err}");
        }
    }
}

fn send_macos_notification(title: &str, subtitle: &str, message: &str) -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Ok(());
    }

    let script = format!(
        "display notification \"{}\" with title \"{}\" subtitle \"{}\"",
        escape_applescript_string(message),
        escape_applescript_string(title),
        escape_applescript_string(subtitle),
    );

    let output = Command::new("osascript").arg("-e").arg(script).output()?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(anyhow::anyhow!(
            "osascript exited with status {}: {}",
            output.status,
            stderr.trim()
        ))
    }
}

fn escape_applescript_string(input: &str) -> String {
    input.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use yoetz_core::config::{Config, NotificationsConfig};

    #[test]
    fn notify_gate_requires_runtime_threshold_and_no_mutes() {
        assert!(!should_notify_completion(
            59_999, 60, true, false, false, false
        ));
        assert!(should_notify_completion(
            60_000, 60, true, false, false, false
        ));
        assert!(!should_notify_completion(
            60_000, 60, false, false, false, false
        ));
        assert!(!should_notify_completion(
            60_000, 60, true, true, false, false
        ));
        assert!(!should_notify_completion(
            60_000, 60, true, false, true, false
        ));
        assert!(!should_notify_completion(
            60_000, 60, true, false, false, true
        ));
    }

    #[test]
    fn preview_is_single_line_and_truncated() {
        let preview = sanitize_preview("line 1\nline\t2\x07  \n  extra words");
        assert_eq!(preview, "line 1 line 2 extra words");

        let long = "a".repeat(200);
        assert_eq!(sanitize_preview(&long).chars().count(), PREVIEW_LIMIT);
    }

    #[test]
    fn config_defaults_apply_threshold_and_enable_notifications() {
        let config = Config::default();
        assert_eq!(notification_threshold_secs(&config), DEFAULT_THRESHOLD_SECS);
        assert!(!config_mutes_notifications(&config));
    }

    #[test]
    fn config_can_mute_notifications() {
        let config = Config {
            notifications: NotificationsConfig {
                enabled: Some(false),
                notify_threshold_secs: Some(5),
            },
            ..Default::default()
        };
        assert!(config_mutes_notifications(&config));
        assert_eq!(notification_threshold_secs(&config), 5);
    }
}
