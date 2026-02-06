use crate::paths::home_dir;
use crate::types::SessionInfo;
use anyhow::{Context, Result};
use rand::{distributions::Alphanumeric, Rng};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

static TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]_[hour][minute][second]");

/// Create a new timestamped session directory under `~/.yoetz/sessions/`.
pub fn create_session_dir() -> Result<SessionInfo> {
    let base = session_base_dir();
    fs::create_dir_all(&base).with_context(|| format!("create sessions dir {}", base.display()))?;

    let ts = OffsetDateTime::now_utc()
        .format(TS_FORMAT)
        .unwrap_or_else(|_| "unknown".to_string());
    let rand: String = rand::thread_rng()
        .sample_iter(&Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    let id = format!("{ts}_{rand}");
    let path = base.join(&id);
    fs::create_dir_all(&path).with_context(|| format!("create session {}", path.display()))?;

    Ok(SessionInfo { id, path })
}

pub fn session_base_dir() -> PathBuf {
    if let Ok(dir) = env::var("YOETZ_DIR") {
        return PathBuf::from(dir).join("sessions");
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz/sessions");
    }
    PathBuf::from(".yoetz/sessions")
}

pub fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let data = serde_json::to_string_pretty(value)?;
    fs::write(path, data).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn write_text(path: &Path, text: &str) -> Result<()> {
    fs::write(path, text).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn list_sessions() -> Result<Vec<SessionInfo>> {
    let base = session_base_dir();
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(&base).with_context(|| format!("read {}", base.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let id = entry.file_name().to_string_lossy().to_string();
            items.push(SessionInfo {
                id,
                path: entry.path(),
            });
        }
    }
    items.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(items)
}
