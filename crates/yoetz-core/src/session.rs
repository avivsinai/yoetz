use crate::paths::home_dir;
use crate::types::SessionInfo;
use anyhow::{Context, Result};
use rand::{distr::Alphanumeric, RngExt};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

static TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]_[hour][minute][second]");

/// Create a new timestamped session directory under `~/.yoetz/sessions/`.
pub fn create_session_dir() -> Result<SessionInfo> {
    let root = yoetz_root_dir();
    create_session_dir_in(&root)
}

pub fn create_session_dir_in(root: &Path) -> Result<SessionInfo> {
    fs::create_dir_all(root).with_context(|| format!("create yoetz dir {}", root.display()))?;
    chmod_owner_only_dir(root)?;

    let base = root.join("sessions");
    fs::create_dir_all(&base).with_context(|| format!("create sessions dir {}", base.display()))?;
    chmod_owner_only_dir(&base)?;

    let id = new_session_id();
    let path = base.join(&id);
    fs::create_dir_all(&path).with_context(|| format!("create session {}", path.display()))?;
    chmod_owner_only_dir(&path)?;

    Ok(SessionInfo { id, path })
}

/// Generate a run id in the same `YYYYMMDD_HHMMSS_xxxxxx` format used for
/// session directories, without creating anything on disk.
pub fn new_session_id() -> String {
    let ts = OffsetDateTime::now_utc()
        .format(TS_FORMAT)
        .unwrap_or_else(|_| "unknown".to_string());
    let rand: String = rand::rng()
        .sample_iter(Alphanumeric)
        .take(6)
        .map(char::from)
        .collect();
    format!("{ts}_{rand}")
}

pub fn session_base_dir() -> PathBuf {
    yoetz_root_dir().join("sessions")
}

/// Prune old/excess session directories under `~/.yoetz/sessions/`.
///
/// No-op when both limits are `None`. Never creates the root or sessions
/// directory. Returns the number of session directories removed.
pub fn prune_sessions(max_age_days: Option<u64>, max_count: Option<usize>) -> Result<usize> {
    prune_sessions_in(&session_base_dir(), max_age_days, max_count)
}

pub fn prune_sessions_in(
    base: &Path,
    max_age_days: Option<u64>,
    max_count: Option<usize>,
) -> Result<usize> {
    if max_age_days.is_none() && max_count.is_none() {
        return Ok(0);
    }
    // Refuse to prune through a symlinked (or non-directory) sessions root:
    // read_dir would follow the link and remove_dir_all could then delete
    // real directories outside the yoetz root.
    let base_meta = match fs::symlink_metadata(base) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        other => other.with_context(|| format!("stat sessions base {}", base.display()))?,
    };
    if !base_meta.is_dir() {
        anyhow::bail!(
            "refusing to prune sessions: {} is not a real directory",
            base.display()
        );
    }

    // (path, id, mtime) for real directories only; symlinks and files are
    // never followed or removed.
    let mut dirs: Vec<(PathBuf, String, std::time::SystemTime)> = Vec::new();
    for entry in fs::read_dir(base).with_context(|| format!("read {}", base.display()))? {
        let entry = entry?;
        // DirEntry::file_type does not follow symlinks, so a symlink to a
        // directory is excluded here.
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let meta = entry
            .metadata()
            .with_context(|| format!("stat {}", entry.path().display()))?;
        // An unknown mtime must never look "infinitely old" (guaranteed
        // deletion); surface the error instead.
        let mtime = meta
            .modified()
            .with_context(|| format!("read mtime of {}", entry.path().display()))?;
        let id = entry.file_name().to_string_lossy().to_string();
        dirs.push((entry.path(), id, mtime));
    }

    let mut doomed: Vec<PathBuf> = Vec::new();

    if let Some(days) = max_age_days {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(days.saturating_mul(86_400)))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let (old, kept): (Vec<_>, Vec<_>) = dirs.into_iter().partition(|(_, _, m)| *m < cutoff);
        doomed.extend(old.into_iter().map(|(p, _, _)| p));
        dirs = kept;
    }

    if let Some(count) = max_count {
        if dirs.len() > count {
            // Newest first by mtime, id as a stable tie-break.
            dirs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)));
            doomed.extend(dirs.split_off(count).into_iter().map(|(p, _, _)| p));
        }
    }

    let mut removed = 0usize;
    for path in doomed {
        fs::remove_dir_all(&path).with_context(|| format!("prune session {}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn yoetz_root_dir() -> PathBuf {
    if let Ok(dir) = env::var("YOETZ_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz");
    }
    PathBuf::from(".yoetz")
}

#[cfg(unix)]
fn chmod_owner_only_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("chmod 700 {}", path.display()))
}

#[cfg(not(unix))]
fn chmod_owner_only_dir(_path: &Path) -> Result<()> {
    Ok(())
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
    list_sessions_in(&session_base_dir())
}

pub fn list_sessions_in(base: &Path) -> Result<Vec<SessionInfo>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut items = Vec::new();
    for entry in fs::read_dir(base).with_context(|| format!("read {}", base.display()))? {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn mkdir(base: &Path, name: &str) -> PathBuf {
        let path = base.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(unix)]
    fn set_mtime_to(path: &Path, when: SystemTime) {
        let file = fs::File::open(path).unwrap();
        file.set_modified(when).unwrap();
    }

    #[cfg(unix)]
    fn set_mtime(path: &Path, age_secs: u64) {
        set_mtime_to(path, SystemTime::now() - Duration::from_secs(age_secs));
    }

    #[test]
    fn prune_is_noop_when_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        mkdir(&base, "20200101_000000_aaaaaa");
        let removed = prune_sessions_in(&base, None, None).unwrap();
        assert_eq!(removed, 0);
        assert!(base.join("20200101_000000_aaaaaa").exists());
    }

    #[test]
    fn prune_does_not_create_missing_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let removed = prune_sessions_in(&base, Some(1), Some(1)).unwrap();
        assert_eq!(removed, 0);
        assert!(!base.exists());
    }

    #[test]
    fn prune_skips_plain_files() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        fs::create_dir_all(&base).unwrap();
        fs::write(base.join("stray.txt"), "keep me").unwrap();
        let removed = prune_sessions_in(&base, None, Some(0)).unwrap();
        assert_eq!(removed, 0);
        assert!(base.join("stray.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_refuses_symlinked_base() {
        let tmp = tempfile::tempdir().unwrap();
        // Real directory outside the yoetz root, with a child session-like dir.
        let external = mkdir(tmp.path(), "external");
        let victim = mkdir(&external, "20200101_000000_victim");
        let base = tmp.path().join("sessions");
        std::os::unix::fs::symlink(&external, &base).unwrap();

        let err = prune_sessions_in(&base, None, Some(0)).unwrap_err();
        assert!(err.to_string().contains("not a real directory"));
        // The external target and its child are untouched.
        assert!(external.exists());
        assert!(victim.exists());
    }

    #[test]
    fn prune_refuses_file_base() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        fs::write(&base, "not a dir").unwrap();
        let err = prune_sessions_in(&base, Some(1), None).unwrap_err();
        assert!(err.to_string().contains("not a real directory"));
        assert!(base.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_skips_symlinked_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        fs::create_dir_all(&base).unwrap();
        let target = mkdir(tmp.path(), "outside");
        std::os::unix::fs::symlink(&target, base.join("linked")).unwrap();
        let removed = prune_sessions_in(&base, None, Some(0)).unwrap();
        assert_eq!(removed, 0);
        assert!(base.join("linked").exists());
        assert!(target.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_by_age_keeps_recent_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let old = mkdir(&base, "20200101_000000_oldddd");
        let fresh = mkdir(&base, "20990101_000000_freshh");
        set_mtime(&old, 10 * 86_400);
        set_mtime(&fresh, 60);
        let removed = prune_sessions_in(&base, Some(7), None).unwrap();
        assert_eq!(removed, 1);
        assert!(!old.exists());
        assert!(fresh.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_by_count_keeps_newest_by_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let a = mkdir(&base, "a");
        let b = mkdir(&base, "b");
        let c = mkdir(&base, "c");
        set_mtime(&a, 300);
        set_mtime(&b, 200);
        set_mtime(&c, 100);
        let removed = prune_sessions_in(&base, None, Some(2)).unwrap();
        assert_eq!(removed, 1);
        assert!(!a.exists());
        assert!(b.exists());
        assert!(c.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_count_tie_breaks_on_id() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let a = mkdir(&base, "20250101_000000_aaaaaa");
        let b = mkdir(&base, "20250102_000000_bbbbbb");
        // Identical mtimes: the lexically larger (newer-looking) id survives.
        let when = SystemTime::now() - Duration::from_secs(100);
        set_mtime_to(&a, when);
        set_mtime_to(&b, when);
        let removed = prune_sessions_in(&base, None, Some(1)).unwrap();
        assert_eq!(removed, 1);
        assert!(!a.exists());
        assert!(b.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_applies_age_then_count_to_survivors() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let ancient = mkdir(&base, "w");
        let old = mkdir(&base, "x");
        let mid = mkdir(&base, "y");
        let fresh = mkdir(&base, "z");
        set_mtime(&ancient, 30 * 86_400);
        set_mtime(&old, 10 * 86_400);
        set_mtime(&mid, 3_600);
        set_mtime(&fresh, 60);
        // Age prunes ancient+old; count=1 then prunes mid from the survivors.
        let removed = prune_sessions_in(&base, Some(7), Some(1)).unwrap();
        assert_eq!(removed, 3);
        assert!(!ancient.exists());
        assert!(!old.exists());
        assert!(!mid.exists());
        assert!(fresh.exists());
    }

    #[test]
    fn new_session_id_has_expected_shape() {
        let id = new_session_id();
        let parts: Vec<&str> = id.split('_').collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert_eq!(parts[2].len(), 6);
    }
}
