use crate::paths::home_dir;
use crate::types::SessionInfo;
use anyhow::{Context, Result};
use rand::{distr::Alphanumeric, RngExt};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use time::{format_description::FormatItem, macros::format_description, OffsetDateTime};

static TS_FORMAT: &[FormatItem<'static>] =
    format_description!("[year][month][day]_[hour][minute][second]");
pub const SESSION_LEASE_FILENAME: &str = ".session.lock";
const LEGACY_SESSION_ADOPTION_FLOOR: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug)]
pub struct SessionLease {
    file: File,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

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
    let lease = try_acquire_session_lease(&path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "new session {} unexpectedly already has an active writer",
            path.display()
        )
    })?;

    Ok(SessionInfo {
        id,
        path,
        _lease: Some(Arc::new(lease)),
    })
}

/// Try to hold the writer lease for an existing session directory.
///
/// `Ok(None)` means another process is actively writing that session. The
/// lease file is created for legacy sessions that predate this mechanism.
pub fn try_acquire_session_lease(session_dir: &Path) -> Result<Option<SessionLease>> {
    let lock_path = session_dir.join(SESSION_LEASE_FILENAME);
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .with_context(|| format!("open session lease {}", lock_path.display()))?;
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(SessionLease { file })),
        Err(err) if is_lock_contended(&err) => Ok(None),
        Err(err) => Err(err).with_context(|| format!("lock session lease {}", lock_path.display())),
    }
}

enum ExistingSessionLease {
    Acquired(SessionLease),
    Busy,
    Missing,
}

fn probe_existing_session_lease(session_dir: &Path) -> Result<ExistingSessionLease> {
    let lock_path = session_dir.join(SESSION_LEASE_FILENAME);
    let file = match OpenOptions::new().read(true).write(true).open(&lock_path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(ExistingSessionLease::Missing)
        }
        Err(err) => {
            return Err(err).with_context(|| format!("open session lease {}", lock_path.display()))
        }
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(ExistingSessionLease::Acquired(SessionLease { file })),
        Err(err) if is_lock_contended(&err) => Ok(ExistingSessionLease::Busy),
        Err(err) => Err(err).with_context(|| format!("lock session lease {}", lock_path.display())),
    }
}

fn is_lock_contended(err: &io::Error) -> bool {
    err.kind() == io::ErrorKind::WouldBlock
        || err.raw_os_error() == fs2::lock_contended_error().raw_os_error()
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

    // (path, id, mtime, existing lease) for removable directories only. The
    // first phase never creates lease files, because doing so would refresh a
    // legacy directory's mtime and defeat age retention.
    let now = std::time::SystemTime::now();
    let mut dirs: Vec<(PathBuf, String, std::time::SystemTime, Option<SessionLease>)> = Vec::new();
    for entry in fs::read_dir(base).with_context(|| format!("read {}", base.display()))? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("read entry in {}", base.display()))
            }
        };
        // DirEntry::file_type does not follow symlinks, so a symlink to a
        // directory is excluded here.
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => {
                return Err(err).with_context(|| format!("inspect {}", entry.path().display()))
            }
        };
        if !file_type.is_dir() {
            continue;
        }
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
            Err(err) => return Err(err).with_context(|| format!("stat {}", path.display())),
        };
        // An unknown mtime must never look "infinitely old" (guaranteed
        // deletion); surface the error instead.
        let mtime = meta
            .modified()
            .with_context(|| format!("read mtime of {}", entry.path().display()))?;
        let lease = match probe_existing_session_lease(&path)? {
            ExistingSessionLease::Acquired(lease) => Some(lease),
            ExistingSessionLease::Busy => continue,
            ExistingSessionLease::Missing => {
                // A writer creates the directory just before its lease file.
                // Never adopt a fresh missing-lease directory in that window.
                let old_enough = now
                    .duration_since(mtime)
                    .is_ok_and(|age| age >= LEGACY_SESSION_ADOPTION_FLOOR);
                if !old_enough {
                    continue;
                }
                None
            }
        };
        let id = entry.file_name().to_string_lossy().to_string();
        dirs.push((path, id, mtime, lease));
    }

    let mut doomed: Vec<(PathBuf, String, std::time::SystemTime, Option<SessionLease>)> =
        Vec::new();

    if let Some(days) = max_age_days {
        let cutoff = std::time::SystemTime::now()
            .checked_sub(std::time::Duration::from_secs(days.saturating_mul(86_400)))
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let (old, kept): (Vec<_>, Vec<_>) = dirs.into_iter().partition(|(_, _, m, _)| *m < cutoff);
        doomed.extend(old);
        dirs = kept;
    }

    if let Some(count) = max_count {
        if dirs.len() > count {
            // Newest first by mtime, id as a stable tie-break.
            dirs.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)));
            doomed.extend(dirs.split_off(count));
        }
    }

    // Release leases for survivors before a potentially long removal pass so
    // browser recipes can reopen them without a spurious session_busy error.
    drop(dirs);

    let mut removed = 0usize;
    for (path, _, _, lease) in doomed {
        let lease = match lease {
            Some(lease) => lease,
            None => match try_acquire_session_lease(&path) {
                Ok(Some(lease)) => lease,
                Ok(None) => continue,
                Err(err)
                    if err
                        .downcast_ref::<io::Error>()
                        .is_some_and(|source| source.kind() == io::ErrorKind::NotFound) =>
                {
                    continue
                }
                Err(err) => return Err(err),
            },
        };
        // Keep the lease through removal so no writer can enter after
        // selection. Some older Windows/FAT/SMB combinations may reject
        // deleting the open lease file; that fails safe with a warning.
        fs::remove_dir_all(&path).with_context(|| format!("prune session {}", path.display()))?;
        drop(lease);
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
                _lease: None,
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

    fn mkdir_completed(base: &Path, name: &str) -> PathBuf {
        let path = mkdir(base, name);
        drop(try_acquire_session_lease(&path).unwrap().unwrap());
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

    #[test]
    fn created_session_holds_writer_lease_until_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let session = create_session_dir_in(tmp.path()).unwrap();

        assert!(try_acquire_session_lease(&session.path).unwrap().is_none());

        let path = session.path.clone();
        drop(session);
        assert!(try_acquire_session_lease(&path).unwrap().is_some());
    }

    #[test]
    fn fs2_platform_contention_error_is_recognized() {
        assert!(is_lock_contended(&fs2::lock_contended_error()));
    }

    #[test]
    fn prune_count_zero_skips_live_session_and_removes_unlocked_session() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let live = mkdir(&base, "20250102_000000_liveee");
        let unlocked = mkdir_completed(&base, "20250101_000000_doneee");
        let _live_lease = try_acquire_session_lease(&live).unwrap().unwrap();

        let removed = prune_sessions_in(&base, None, Some(0)).unwrap();

        assert_eq!(removed, 1);
        assert!(live.exists());
        assert!(!unlocked.exists());
    }

    #[test]
    fn prune_count_limit_applies_only_to_unlocked_sessions() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let live = mkdir(&base, "20250103_000000_liveee");
        let older = mkdir_completed(&base, "20250101_000000_olderr");
        let newer = mkdir_completed(&base, "20250102_000000_newerr");
        let _live_lease = try_acquire_session_lease(&live).unwrap().unwrap();

        let removed = prune_sessions_in(&base, None, Some(1)).unwrap();

        assert_eq!(removed, 1);
        assert!(live.exists());
        assert!(!older.exists());
        assert!(newer.exists());
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
        let old = mkdir_completed(&base, "20200101_000000_oldddd");
        let fresh = mkdir_completed(&base, "20990101_000000_freshh");
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
        let a = mkdir_completed(&base, "a");
        let b = mkdir_completed(&base, "b");
        let c = mkdir_completed(&base, "c");
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
        let a = mkdir_completed(&base, "20250101_000000_aaaaaa");
        let b = mkdir_completed(&base, "20250102_000000_bbbbbb");
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
        let ancient = mkdir_completed(&base, "w");
        let old = mkdir_completed(&base, "x");
        let mid = mkdir_completed(&base, "y");
        let fresh = mkdir_completed(&base, "z");
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

    #[cfg(unix)]
    #[test]
    fn count_scan_does_not_refresh_legacy_mtime_before_age_prune() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let legacy = mkdir(&base, "20200101_000000_legacy");
        let old_mtime = SystemTime::now() - Duration::from_secs(10 * 86_400);
        set_mtime_to(&legacy, old_mtime);

        assert_eq!(prune_sessions_in(&base, None, Some(10)).unwrap(), 0);
        assert!(!legacy.join(SESSION_LEASE_FILENAME).exists());
        assert_eq!(
            fs::metadata(&legacy).unwrap().modified().unwrap(),
            old_mtime
        );

        assert_eq!(prune_sessions_in(&base, Some(7), None).unwrap(), 1);
        assert!(!legacy.exists());
    }

    #[test]
    fn prune_does_not_adopt_fresh_directory_without_lease() {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().join("sessions");
        let fresh = mkdir(&base, "20250101_000000_starting");

        assert_eq!(prune_sessions_in(&base, None, Some(0)).unwrap(), 0);
        assert!(fresh.exists());
        assert!(!fresh.join(SESSION_LEASE_FILENAME).exists());
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
