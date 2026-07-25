use anyhow::{anyhow, bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::thread;
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

#[cfg(test)]
use crate::chatgpt_web::{self, ChatgptConversation};
use crate::web_recipe::{BuiltinWebRecipe, WebConversation};
use yoetz_core::session::list_sessions_in;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FollowupMetadata {
    pub session_id: String,
    #[serde(default)]
    pub recipe: BuiltinWebRecipe,
    #[serde(default)]
    pub thread_label: Option<String>,
    #[serde(default)]
    pub thread_revision: u64,
    #[serde(default)]
    pub forked_from_label: Option<String>,
    #[serde(default)]
    pub forked_from_conversation_id: Option<String>,
    pub conversation_id: String,
    pub conversation_url: String,
    pub prompt_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFollowup {
    pub recipe: BuiltinWebRecipe,
    pub conversation: WebConversation,
    pub source_session_id: Option<String>,
    pub prior_prompt_hash: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) enum ThreadConflictPolicy {
    #[default]
    Fail,
    Wait(Option<Duration>),
    Fork,
}

impl FromStr for ThreadConflictPolicy {
    type Err = String;

    fn from_str(raw: &str) -> std::result::Result<Self, Self::Err> {
        match raw {
            "fail" => Ok(Self::Fail),
            "wait" => Ok(Self::Wait(None)),
            "fork" => Ok(Self::Fork),
            _ => {
                let Some(raw_duration) = raw.strip_prefix("wait:") else {
                    return Err(thread_conflict_policy_error(raw));
                };
                parse_thread_wait_duration(raw_duration)
                    .map(|duration| Self::Wait(Some(duration)))
                    .ok_or_else(|| thread_conflict_policy_error(raw))
            }
        }
    }
}

impl fmt::Display for ThreadConflictPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fail => formatter.write_str("fail"),
            Self::Wait(None) => formatter.write_str("wait"),
            Self::Wait(Some(duration)) => write!(formatter, "wait:{}ms", duration.as_millis()),
            Self::Fork => formatter.write_str("fork"),
        }
    }
}

fn thread_conflict_policy_error(raw: &str) -> String {
    format!(
        "invalid thread conflict policy `{raw}`; expected fail, wait, wait:<duration>, or fork (duration units: ms, s, m, h)"
    )
}

fn parse_thread_wait_duration(raw: &str) -> Option<Duration> {
    let (number, unit) = if let Some(number) = raw.strip_suffix("ms") {
        (number, "ms")
    } else if let Some(number) = raw.strip_suffix('s') {
        (number, "s")
    } else if let Some(number) = raw.strip_suffix('m') {
        (number, "m")
    } else {
        let number = raw.strip_suffix('h')?;
        (number, "h")
    };
    let value = number.parse::<u64>().ok()?;
    if value == 0 {
        return None;
    }
    match unit {
        "ms" => Some(Duration::from_millis(value)),
        "s" => Some(Duration::from_secs(value)),
        "m" => value.checked_mul(60).map(Duration::from_secs),
        "h" => value.checked_mul(60 * 60).map(Duration::from_secs),
        _ => None,
    }
}

#[derive(Debug)]
pub(crate) enum PreparedThreadDisposition {
    Labeled {
        label: String,
        resolved: Option<ResolvedFollowup>,
    },
    Forked {
        from_label: String,
        from_conversation_id: Option<String>,
    },
}

#[derive(Debug)]
pub(crate) struct PreparedThreadRun {
    _lease: Option<ThreadLabelLease>,
    disposition: PreparedThreadDisposition,
}

impl PreparedThreadRun {
    pub(crate) fn disposition(&self) -> &PreparedThreadDisposition {
        &self.disposition
    }

    pub(crate) fn thread_label_for_metadata(&self) -> Option<&str> {
        match &self.disposition {
            PreparedThreadDisposition::Labeled { label, .. } => Some(label),
            PreparedThreadDisposition::Forked { .. } => None,
        }
    }

    pub(crate) fn forked_from_label(&self) -> Option<&str> {
        match &self.disposition {
            PreparedThreadDisposition::Labeled { .. } => None,
            PreparedThreadDisposition::Forked { from_label, .. } => Some(from_label),
        }
    }

    pub(crate) fn forked_from_conversation_id(&self) -> Option<&str> {
        match &self.disposition {
            PreparedThreadDisposition::Labeled { .. } => None,
            PreparedThreadDisposition::Forked {
                from_conversation_id,
                ..
            } => from_conversation_id.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ThreadLockHolder {
    #[serde(rename = "holder_run_id")]
    run_id: String,
    pid: u32,
    started_at: String,
}

impl ThreadLockHolder {
    fn new(run_id: &str) -> Result<Self> {
        Ok(Self {
            run_id: run_id.to_string(),
            pid: std::process::id(),
            started_at: OffsetDateTime::now_utc()
                .format(&Rfc3339)
                .context("format thread lock start time")?,
        })
    }
}

#[derive(Debug)]
struct ThreadLabelLease {
    file: File,
}

impl Drop for ThreadLabelLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

enum ThreadLockOutcome {
    Acquired(ThreadLabelLease),
    Forked,
}

#[cfg(test)]
pub(crate) fn resolve_followup_target(raw: &str, sessions_base: &Path) -> Result<ResolvedFollowup> {
    resolve_followup_target_for_recipe(raw, sessions_base, BuiltinWebRecipe::Chatgpt, |value| {
        let conversation = chatgpt_web::normalize_conversation(value)?;
        Ok(WebConversation {
            id: conversation.id,
            url: conversation.url,
        })
    })
}

pub(crate) fn resolve_followup_target_for_recipe<F>(
    raw: &str,
    sessions_base: &Path,
    recipe: BuiltinWebRecipe,
    normalize: F,
) -> Result<ResolvedFollowup>
where
    F: Fn(&str) -> Result<WebConversation>,
{
    let sessions = list_sessions_in(sessions_base)?;
    if let Some(session) = sessions.iter().find(|session| session.id == raw) {
        let metadata = read_followup_metadata(&session.path)?.ok_or_else(|| {
            anyhow!(
                "session `{}` does not contain followup metadata; followup only works for browser recipe runs that wrote session metadata",
                session.id,
            )
        })?;
        if metadata.recipe != recipe {
            bail!(
                "session `{}` belongs to the `{}` browser recipe and cannot be used as a `{}` followup",
                session.id,
                metadata.recipe.as_str(),
                recipe.as_str(),
            );
        }
        let conversation = normalize(&metadata.conversation_url)?;
        return Ok(ResolvedFollowup {
            recipe,
            conversation,
            source_session_id: Some(metadata.session_id),
            prior_prompt_hash: Some(metadata.prompt_hash),
        });
    }

    let conversation = normalize(raw)?;
    let previous =
        find_latest_followup_metadata_for_conversation(recipe, &conversation.id, &sessions)?;
    Ok(ResolvedFollowup {
        recipe,
        conversation,
        source_session_id: previous
            .as_ref()
            .map(|metadata| metadata.session_id.clone()),
        prior_prompt_hash: previous.map(|metadata| metadata.prompt_hash),
    })
}

pub(crate) fn validate_thread_label(label: &str) -> Result<()> {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        bail!("thread label must match ^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$");
    };
    if !first.is_ascii_alphanumeric()
        || label.len() > 64
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    {
        bail!("invalid thread label `{label}`; expected ^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$");
    }
    Ok(())
}

pub(crate) fn resolve_thread_target(
    label: &str,
    sessions_base: &Path,
    recipe: BuiltinWebRecipe,
) -> Result<Option<ResolvedFollowup>> {
    validate_thread_label(label)?;
    let mut latest: Option<FollowupMetadata> = None;
    for session in list_sessions_in(sessions_base)? {
        let metadata = match read_followup_metadata(&session.path) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => continue,
            Err(err) => {
                eprintln!(
                    "warning: skipping unreadable followup metadata in session `{}`: {err:#}",
                    session.id
                );
                continue;
            }
        };
        if metadata.recipe != recipe || metadata.thread_label.as_deref() != Some(label) {
            continue;
        }
        if latest
            .as_ref()
            .is_some_and(|current| current.thread_revision >= metadata.thread_revision)
        {
            continue;
        }
        latest = Some(metadata);
    }
    Ok(latest.map(|metadata| ResolvedFollowup {
        recipe,
        conversation: WebConversation {
            id: metadata.conversation_id,
            url: metadata.conversation_url,
        },
        source_session_id: Some(metadata.session_id),
        prior_prompt_hash: Some(metadata.prompt_hash),
    }))
}

pub(crate) fn prepare_thread_run_in(
    label: &str,
    sessions_base: &Path,
    recipe: BuiltinWebRecipe,
    run_id: &str,
    fresh: bool,
    conflict_policy: &ThreadConflictPolicy,
) -> Result<PreparedThreadRun> {
    validate_thread_label(label)?;
    let yoetz_root = sessions_base
        .parent()
        .ok_or_else(|| anyhow!("managed sessions directory has no Yoetz root parent"))?;
    let lock_outcome =
        acquire_thread_label_lock(yoetz_root, recipe, label, run_id, conflict_policy)?;

    match lock_outcome {
        ThreadLockOutcome::Acquired(lease) => {
            let resolved = if fresh {
                None
            } else {
                resolve_thread_target(label, sessions_base, recipe)?
            };
            Ok(PreparedThreadRun {
                _lease: Some(lease),
                disposition: PreparedThreadDisposition::Labeled {
                    label: label.to_string(),
                    resolved,
                },
            })
        }
        ThreadLockOutcome::Forked => {
            let from_conversation_id = resolve_thread_target(label, sessions_base, recipe)?
                .map(|resolved| resolved.conversation.id);
            Ok(PreparedThreadRun {
                _lease: None,
                disposition: PreparedThreadDisposition::Forked {
                    from_label: label.to_string(),
                    from_conversation_id,
                },
            })
        }
    }
}

fn acquire_thread_label_lock(
    yoetz_root: &Path,
    recipe: BuiltinWebRecipe,
    label: &str,
    run_id: &str,
    conflict_policy: &ThreadConflictPolicy,
) -> Result<ThreadLockOutcome> {
    let threads_root = yoetz_root.join("threads");
    ensure_private_thread_dir(&threads_root)?;
    let thread_dir = threads_root.join(recipe.as_str());
    ensure_private_thread_dir(&thread_dir)?;
    let lock_path = thread_dir.join(format!("{label}.lock"));
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&lock_path)
        .with_context(|| format!("open thread lock {}", lock_path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o600))
            .with_context(|| format!("chmod 600 {}", lock_path.display()))?;
    }
    let holder = ThreadLockHolder::new(run_id)?;

    match conflict_policy {
        ThreadConflictPolicy::Fail | ThreadConflictPolicy::Fork => {
            match FileExt::try_lock_exclusive(&file) {
                Ok(()) => {
                    write_thread_lock_holder(&mut file, &holder, &lock_path)?;
                    Ok(ThreadLockOutcome::Acquired(ThreadLabelLease { file }))
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    if matches!(conflict_policy, ThreadConflictPolicy::Fork) {
                        Ok(ThreadLockOutcome::Forked)
                    } else {
                        Err(thread_busy_error(
                            "thread_busy",
                            label,
                            recipe,
                            &mut file,
                            &lock_path,
                        ))
                    }
                }
                Err(err) => {
                    Err(err).with_context(|| format!("lock thread {}", lock_path.display()))
                }
            }
        }
        ThreadConflictPolicy::Wait(None) => {
            FileExt::lock_exclusive(&file)
                .with_context(|| format!("wait for thread lock {}", lock_path.display()))?;
            write_thread_lock_holder(&mut file, &holder, &lock_path)?;
            Ok(ThreadLockOutcome::Acquired(ThreadLabelLease { file }))
        }
        ThreadConflictPolicy::Wait(Some(timeout)) => {
            let started = Instant::now();
            loop {
                match FileExt::try_lock_exclusive(&file) {
                    Ok(()) => {
                        write_thread_lock_holder(&mut file, &holder, &lock_path)?;
                        return Ok(ThreadLockOutcome::Acquired(ThreadLabelLease { file }));
                    }
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        let elapsed = started.elapsed();
                        if elapsed >= *timeout {
                            return Err(thread_busy_error(
                                "thread_busy_timeout",
                                label,
                                recipe,
                                &mut file,
                                &lock_path,
                            ));
                        }
                        thread::sleep(
                            timeout
                                .saturating_sub(elapsed)
                                .min(Duration::from_millis(50)),
                        );
                    }
                    Err(err) => {
                        return Err(err)
                            .with_context(|| format!("lock thread {}", lock_path.display()));
                    }
                }
            }
        }
    }
}

fn ensure_private_thread_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", path.display()))?;
    }
    Ok(())
}

fn write_thread_lock_holder(
    file: &mut File,
    holder: &ThreadLockHolder,
    lock_path: &Path,
) -> Result<()> {
    file.set_len(0)
        .with_context(|| format!("truncate thread lock {}", lock_path.display()))?;
    file.seek(SeekFrom::Start(0))
        .with_context(|| format!("seek thread lock {}", lock_path.display()))?;
    serde_json::to_writer(&mut *file, holder)
        .with_context(|| format!("write thread lock holder {}", lock_path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("finish thread lock holder {}", lock_path.display()))?;
    file.flush()
        .with_context(|| format!("flush thread lock holder {}", lock_path.display()))?;
    file.sync_data()
        .with_context(|| format!("sync thread lock holder {}", lock_path.display()))?;
    Ok(())
}

fn thread_busy_error(
    code: &str,
    label: &str,
    recipe: BuiltinWebRecipe,
    file: &mut File,
    lock_path: &Path,
) -> anyhow::Error {
    let holder = read_thread_lock_holder(file).unwrap_or_else(|| ThreadLockHolder {
        run_id: "unknown".to_string(),
        pid: 0,
        started_at: "unknown".to_string(),
    });
    let holder_json = serde_json::to_string(&holder)
        .unwrap_or_else(|_| r#"{"holder_run_id":"unknown","pid":0,"started_at":"unknown"}"#.into());
    anyhow!(
        "{code}: thread `{label}` for recipe `{}` is active; holder={holder_json}; wait for the holder to finish (or retry with `--on-thread-conflict wait:<duration>`), or use a distinct --thread label. Lock: {}",
        recipe.as_str(),
        lock_path.display(),
    )
}

fn read_thread_lock_holder(file: &mut File) -> Option<ThreadLockHolder> {
    // The winning process writes immediately after flock succeeds. A contender can
    // observe the lock during that tiny window, so retry briefly before degrading.
    for _ in 0..20 {
        thread::sleep(Duration::from_millis(5));
        if file.seek(SeekFrom::Start(0)).is_err() {
            return None;
        }
        let mut raw = String::new();
        if file.read_to_string(&mut raw).is_ok() {
            if let Ok(holder) = serde_json::from_str(&raw) {
                return Some(holder);
            }
        }
    }
    None
}

pub(crate) fn compute_prompt_hash(prompt: &str, bundle_path: Option<&Path>) -> Result<String> {
    let bundle_hash = if let Some(bundle_path) = bundle_path {
        let bundle_bytes = fs::read(bundle_path)
            .with_context(|| format!("read bundle for followup hash {}", bundle_path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(bundle_bytes);
        hex::encode(hasher.finalize())
    } else {
        String::new()
    };

    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    hasher.update([0]);
    hasher.update(bundle_hash.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn guard_duplicate_prompt(
    current_hash: &str,
    prior_hash: Option<&str>,
    allow_duplicate_prompt: bool,
    conversation_id: &str,
    prior_session_id: Option<&str>,
) -> Result<()> {
    if allow_duplicate_prompt {
        return Ok(());
    }
    let Some(prior_hash) = prior_hash else {
        return Ok(());
    };
    if current_hash != prior_hash {
        return Ok(());
    }

    let prior_context = prior_session_id
        .map(|session_id| format!(" from prior session `{session_id}`"))
        .unwrap_or_default();
    bail!(
        "duplicate followup prompt for conversation `{conversation_id}` matches the immediately previous turn{prior_context}. Pass --allow-duplicate-prompt to override."
    )
}

pub(crate) fn validate_followup_args(
    followup: Option<&str>,
    conversation_var: Option<&str>,
    thread: Option<&str>,
    fresh: bool,
) -> Result<()> {
    let selector_count = usize::from(followup.is_some())
        + usize::from(conversation_var.is_some())
        + usize::from(thread.is_some());
    if selector_count > 1 {
        bail!("--thread, --followup, and --var conversation= are mutually exclusive");
    }
    if fresh && thread.is_none() {
        bail!("--fresh requires --thread");
    }
    if let Some(label) = thread {
        validate_thread_label(label)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_followup_metadata(
    session_dir: &Path,
    session_id: &str,
    conversation: &ChatgptConversation,
    prompt_hash: &str,
) -> Result<()> {
    write_followup_metadata_for_recipe(
        session_dir,
        session_id,
        BuiltinWebRecipe::Chatgpt,
        &WebConversation {
            id: conversation.id.clone(),
            url: conversation.url.clone(),
        },
        prompt_hash,
        None,
    )
}

#[cfg(test)]
pub(crate) fn write_followup_metadata_for_recipe(
    session_dir: &Path,
    session_id: &str,
    recipe: BuiltinWebRecipe,
    conversation: &WebConversation,
    prompt_hash: &str,
    thread_label: Option<&str>,
) -> Result<()> {
    write_followup_metadata_for_recipe_with_lineage(
        session_dir,
        session_id,
        recipe,
        conversation,
        prompt_hash,
        thread_label,
        None,
        None,
    )
}

pub(crate) fn write_followup_metadata_for_recipe_with_lineage(
    session_dir: &Path,
    session_id: &str,
    recipe: BuiltinWebRecipe,
    conversation: &WebConversation,
    prompt_hash: &str,
    thread_label: Option<&str>,
    forked_from_label: Option<&str>,
    forked_from_conversation_id: Option<&str>,
) -> Result<()> {
    if thread_label.is_some() && forked_from_label.is_some() {
        bail!("thread metadata cannot be both labeled and forked");
    }
    let path = followup_metadata_path(session_dir);
    let thread_revision = match thread_label {
        Some(label) => next_thread_revision(session_dir, recipe, label)?,
        None => 0,
    };
    let metadata = FollowupMetadata {
        session_id: session_id.to_string(),
        recipe,
        thread_label: thread_label.map(str::to_string),
        thread_revision,
        forked_from_label: forked_from_label.map(str::to_string),
        forked_from_conversation_id: forked_from_conversation_id.map(str::to_string),
        conversation_id: conversation.id.clone(),
        conversation_url: conversation.url.clone(),
        prompt_hash: prompt_hash.to_string(),
    };
    write_followup_metadata_atomically(&path, &metadata)?;
    Ok(())
}

fn next_thread_revision(
    session_dir: &Path,
    recipe: BuiltinWebRecipe,
    thread_label: &str,
) -> Result<u64> {
    let sessions_base = session_dir
        .parent()
        .ok_or_else(|| anyhow!("thread metadata session has no sessions directory parent"))?;
    let mut max_revision = 0;
    for session in list_sessions_in(sessions_base)? {
        let metadata = match read_followup_metadata(&session.path) {
            Ok(Some(metadata)) => metadata,
            Ok(None) => continue,
            Err(err) => {
                eprintln!(
                    "warning: skipping unreadable followup metadata in session `{}` while assigning thread revision: {err:#}",
                    session.id
                );
                continue;
            }
        };
        if metadata.recipe == recipe && metadata.thread_label.as_deref() == Some(thread_label) {
            max_revision = max_revision.max(metadata.thread_revision);
        }
    }
    max_revision
        .checked_add(1)
        .ok_or_else(|| anyhow!("thread `{thread_label}` revision overflow"))
}

fn write_followup_metadata_atomically(path: &Path, metadata: &FollowupMetadata) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("followup metadata path has no parent"))?;
    let mut temp = NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary followup metadata in {}", parent.display()))?;
    serde_json::to_writer_pretty(temp.as_file_mut(), metadata)
        .with_context(|| format!("serialize followup metadata {}", path.display()))?;
    temp.as_file_mut()
        .write_all(b"\n")
        .with_context(|| format!("write temporary followup metadata for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("sync temporary followup metadata for {}", path.display()))?;
    temp.persist(path)
        .map_err(|err| err.error)
        .with_context(|| format!("atomically replace followup metadata {}", path.display()))?;
    Ok(())
}

pub(crate) fn read_followup_metadata(session_dir: &Path) -> Result<Option<FollowupMetadata>> {
    let path = followup_metadata_path(session_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read followup metadata {}", path.display()))?;
    let metadata = serde_json::from_str(&raw)
        .with_context(|| format!("parse followup metadata {}", path.display()))?;
    Ok(Some(metadata))
}

fn followup_metadata_path(session_dir: &Path) -> PathBuf {
    session_dir.join("followup.json")
}

fn find_latest_followup_metadata_for_conversation(
    recipe: BuiltinWebRecipe,
    conversation_id: &str,
    sessions: &[yoetz_core::types::SessionInfo],
) -> Result<Option<FollowupMetadata>> {
    for session in sessions {
        if let Some(metadata) = read_followup_metadata(&session.path)? {
            if metadata.recipe == recipe && metadata.conversation_id == conversation_id {
                return Ok(Some(metadata));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prompt_hash_changes_with_prompt_or_bundle() {
        let dir = tempdir().unwrap();
        let bundle = dir.path().join("bundle.md");
        fs::write(&bundle, "bundle one").unwrap();

        let a = compute_prompt_hash("prompt one", Some(&bundle)).unwrap();
        let b = compute_prompt_hash("prompt two", Some(&bundle)).unwrap();
        let c = compute_prompt_hash("prompt one", None).unwrap();

        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn duplicate_guard_hits_and_can_be_overridden() {
        let err = guard_duplicate_prompt("abc", Some("abc"), false, "conv-1", Some("sess-1"))
            .unwrap_err();
        assert!(err.to_string().contains("duplicate followup prompt"));
        assert!(guard_duplicate_prompt("abc", Some("abc"), true, "conv-1", Some("sess-1")).is_ok());
        assert!(
            guard_duplicate_prompt("abc", Some("def"), false, "conv-1", Some("sess-1")).is_ok()
        );
        assert!(guard_duplicate_prompt("abc", None, false, "conv-1", None).is_ok());
    }

    #[test]
    fn conversation_selectors_are_pairwise_mutually_exclusive() {
        let selectors = [
            (
                Some("session-1"),
                Some("https://chatgpt.com/c/conv-1"),
                None,
            ),
            (Some("session-1"), None, Some("review-pr-341")),
            (
                None,
                Some("https://chatgpt.com/c/conv-1"),
                Some("review-pr-341"),
            ),
        ];
        for (followup, conversation, thread) in selectors {
            let err = validate_followup_args(followup, conversation, thread, false).unwrap_err();
            assert!(err.to_string().contains("mutually exclusive"));
        }
    }

    #[test]
    fn fresh_requires_a_valid_thread_label() {
        assert!(validate_followup_args(None, None, Some("review-pr_341.v2"), true).is_ok());
        assert!(validate_followup_args(None, None, None, true)
            .unwrap_err()
            .to_string()
            .contains("requires --thread"));
        for invalid in ["", "../escape", "-leading", "space label", "é"] {
            assert!(validate_followup_args(None, None, Some(invalid), false).is_err());
        }
        assert!(validate_followup_args(None, None, Some(&"a".repeat(65)), false).is_err());
    }

    #[test]
    fn session_id_resolution_requires_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(sessions_dir.join("20260711_000000_aaaaaa")).unwrap();
        let err = resolve_followup_target("20260711_000000_aaaaaa", &sessions_dir).unwrap_err();
        assert!(err
            .to_string()
            .contains("does not contain followup metadata"));
    }

    #[test]
    fn direct_conversation_resolution_uses_latest_session_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let old = sessions_dir.join("20260711_000000_aaaaaa");
        let new = sessions_dir.join("20260711_010000_bbbbbb");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        write_followup_metadata(
            &old,
            "20260711_000000_aaaaaa",
            &ChatgptConversation {
                id: "conv-1".to_string(),
                url: "https://chatgpt.com/c/conv-1".to_string(),
            },
            "old-hash",
        )
        .unwrap();
        write_followup_metadata(
            &new,
            "20260711_010000_bbbbbb",
            &ChatgptConversation {
                id: "conv-1".to_string(),
                url: "https://chatgpt.com/c/conv-1".to_string(),
            },
            "new-hash",
        )
        .unwrap();

        let resolved = resolve_followup_target("conv-1", &sessions_dir).unwrap();
        assert_eq!(resolved.conversation.id, "conv-1");
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("new-hash"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_bbbbbb")
        );
    }

    #[test]
    fn session_id_resolution_reads_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let session = sessions_dir.join("20260711_010000_bbbbbb");
        fs::create_dir_all(&session).unwrap();
        write_followup_metadata(
            &session,
            "20260711_010000_bbbbbb",
            &ChatgptConversation {
                id: "conv-2".to_string(),
                url: "https://chatgpt.com/c/conv-2".to_string(),
            },
            "hash-2",
        )
        .unwrap();

        let resolved = resolve_followup_target("20260711_010000_bbbbbb", &sessions_dir).unwrap();
        assert_eq!(resolved.conversation.id, "conv-2");
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("hash-2"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_bbbbbb")
        );
    }

    #[test]
    fn legacy_followup_metadata_defaults_to_chatgpt() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("followup.json"),
            r#"{
              "session_id": "legacy-session",
              "conversation_id": "conv-legacy",
              "conversation_url": "https://chatgpt.com/c/conv-legacy",
              "prompt_hash": "legacy-hash"
            }"#,
        )
        .unwrap();

        let metadata = read_followup_metadata(root.path()).unwrap().unwrap();
        assert_eq!(metadata.recipe, BuiltinWebRecipe::Chatgpt);
        assert_eq!(metadata.thread_label, None);
    }

    #[test]
    fn direct_followup_lookup_is_keyed_by_recipe_and_conversation_id() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let chatgpt_session = sessions_dir.join("20260711_000000_chatgpt");
        let claude_session = sessions_dir.join("20260711_010000_claude");
        fs::create_dir_all(&chatgpt_session).unwrap();
        fs::create_dir_all(&claude_session).unwrap();
        write_followup_metadata_for_recipe(
            &chatgpt_session,
            "20260711_000000_chatgpt",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "shared-id".to_string(),
                url: "https://chatgpt.com/c/shared-id".to_string(),
            },
            "chatgpt-hash",
            None,
        )
        .unwrap();
        write_followup_metadata_for_recipe(
            &claude_session,
            "20260711_010000_claude",
            BuiltinWebRecipe::Claude,
            &WebConversation {
                id: "shared-id".to_string(),
                url: "https://claude.ai/chat/shared-id".to_string(),
            },
            "claude-hash",
            None,
        )
        .unwrap();

        let resolved = resolve_followup_target_for_recipe(
            "shared-id",
            &sessions_dir,
            BuiltinWebRecipe::Claude,
            |raw| {
                Ok(WebConversation {
                    id: raw.to_string(),
                    url: format!("https://claude.ai/chat/{raw}"),
                })
            },
        )
        .unwrap();

        assert_eq!(resolved.recipe, BuiltinWebRecipe::Claude);
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("claude-hash"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_claude")
        );
    }

    #[test]
    fn thread_resolution_uses_newest_matching_recipe_and_skips_legacy_files() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let legacy = sessions_dir.join("20260711_020000_legacy");
        let old_chatgpt = sessions_dir.join("20260711_000000_chatgpt");
        let new_chatgpt = sessions_dir.join("20260711_030000_chatgpt");
        let claude = sessions_dir.join("20260711_040000_claude");
        for path in [&legacy, &old_chatgpt, &new_chatgpt, &claude] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(
            legacy.join("followup.json"),
            r#"{
              "session_id": "20260711_020000_legacy",
              "conversation_id": "legacy",
              "conversation_url": "https://chatgpt.com/c/legacy",
              "prompt_hash": "legacy-hash"
            }"#,
        )
        .unwrap();
        for (path, session_id, recipe, conversation_id, hash) in [
            (
                &old_chatgpt,
                "20260711_000000_chatgpt",
                BuiltinWebRecipe::Chatgpt,
                "old-chatgpt",
                "old-hash",
            ),
            (
                &new_chatgpt,
                "20260711_030000_chatgpt",
                BuiltinWebRecipe::Chatgpt,
                "new-chatgpt",
                "new-hash",
            ),
            (
                &claude,
                "20260711_040000_claude",
                BuiltinWebRecipe::Claude,
                "claude-conversation",
                "claude-hash",
            ),
        ] {
            let host = match recipe {
                BuiltinWebRecipe::Chatgpt => "https://chatgpt.com/c/",
                BuiltinWebRecipe::Claude => "https://claude.ai/chat/",
            };
            write_followup_metadata_for_recipe(
                path,
                session_id,
                recipe,
                &WebConversation {
                    id: conversation_id.to_string(),
                    url: format!("{host}{conversation_id}"),
                },
                hash,
                Some("review-pr-341"),
            )
            .unwrap();
        }

        let chatgpt =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .unwrap();
        assert_eq!(chatgpt.conversation.id, "new-chatgpt");
        assert_eq!(chatgpt.prior_prompt_hash.as_deref(), Some("new-hash"));

        let claude =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Claude)
                .unwrap()
                .unwrap();
        assert_eq!(claude.conversation.id, "claude-conversation");

        assert!(
            resolve_thread_target("unknown-thread", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn thread_resolution_skips_corrupt_unrelated_session_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let corrupt = sessions_dir.join("20260711_040000_corrupt");
        let matching = sessions_dir.join("20260711_030000_matching");
        fs::create_dir_all(&corrupt).unwrap();
        fs::create_dir_all(&matching).unwrap();
        fs::write(corrupt.join("followup.json"), "{not-json").unwrap();
        write_followup_metadata_for_recipe(
            &matching,
            "20260711_030000_matching",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "conv-match".to_string(),
                url: "https://chatgpt.com/c/conv-match".to_string(),
            },
            "matching-hash",
            Some("review-pr-341"),
        )
        .unwrap();

        let resolved =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .unwrap();

        assert_eq!(resolved.conversation.id, "conv-match");
    }

    #[test]
    fn followup_metadata_write_replaces_existing_file_atomically() {
        let root = tempdir().unwrap();
        let session = root.path().join("sessions").join("session-atomic");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("followup.json"), "{partial").unwrap();

        write_followup_metadata_for_recipe(
            &session,
            "session-atomic",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "conv-atomic".to_string(),
                url: "https://chatgpt.com/c/conv-atomic".to_string(),
            },
            "atomic-hash",
            Some("review-pr-341"),
        )
        .unwrap();

        let metadata = read_followup_metadata(&session).unwrap().unwrap();
        assert_eq!(metadata.session_id, "session-atomic");
        assert_eq!(metadata.conversation_id, "conv-atomic");
        assert!(fs::read_dir(&session).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains(".tmp")));
    }

    #[test]
    fn rewriting_older_session_repoints_thread_label() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let older = sessions_dir.join("20260711_010000_older");
        let newer = sessions_dir.join("20260711_020000_newer");
        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();

        for (session, id, conversation) in [
            (&older, "20260711_010000_older", "conv-old"),
            (&newer, "20260711_020000_newer", "conv-newer"),
            (&older, "20260711_010000_older", "conv-fresh"),
        ] {
            write_followup_metadata_for_recipe(
                session,
                id,
                BuiltinWebRecipe::Chatgpt,
                &WebConversation {
                    id: conversation.to_string(),
                    url: format!("https://chatgpt.com/c/{conversation}"),
                },
                &format!("{conversation}-hash"),
                Some("review-pr-341"),
            )
            .unwrap();
        }

        let resolved =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .unwrap();

        assert_eq!(resolved.conversation.id, "conv-fresh");
        assert_eq!(
            resolved.prior_prompt_hash.as_deref(),
            Some("conv-fresh-hash")
        );
    }

    #[test]
    fn thread_conflict_policy_parses_fail_wait_and_fork() {
        assert_eq!(
            "fail".parse::<ThreadConflictPolicy>().unwrap(),
            ThreadConflictPolicy::Fail
        );
        assert_eq!(
            "wait".parse::<ThreadConflictPolicy>().unwrap(),
            ThreadConflictPolicy::Wait(None)
        );
        assert_eq!(
            "wait:250ms".parse::<ThreadConflictPolicy>().unwrap(),
            ThreadConflictPolicy::Wait(Some(std::time::Duration::from_millis(250)))
        );
        assert_eq!(
            "wait:2m".parse::<ThreadConflictPolicy>().unwrap(),
            ThreadConflictPolicy::Wait(Some(std::time::Duration::from_secs(120)))
        );
        assert_eq!(
            "fork".parse::<ThreadConflictPolicy>().unwrap(),
            ThreadConflictPolicy::Fork
        );

        for invalid in ["", "queue", "wait:", "wait:0s", "wait:1", "wait:1fortnight"] {
            assert!(
                invalid.parse::<ThreadConflictPolicy>().is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn same_recipe_and_label_fail_closed_with_actionable_holder_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let first = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-first",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let err = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-second",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap_err();
        let message = format!("{err:#}");

        assert!(message.contains("thread_busy"));
        assert!(message.contains("\"holder_run_id\":\"run-first\""));
        assert!(message.contains(&format!("\"pid\":{}", std::process::id())));
        assert!(message.contains("\"started_at\":"));
        assert!(message.contains("wait for the holder"));
        assert!(message.contains("distinct --thread label"));

        drop(first);
        assert!(prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-third",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .is_ok());
    }

    #[test]
    fn thread_locks_are_scoped_by_recipe_and_label() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let _chatgpt = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-chatgpt",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let _other_label = prepare_thread_run_in(
            "review-pr-342",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-other-label",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let _claude = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Claude,
            "run-claude",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [
                root.path().join("threads"),
                root.path().join("threads/chatgpt"),
                root.path().join("threads/claude"),
            ] {
                let mode = fs::metadata(path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700);
            }
            let lock_mode = fs::metadata(root.path().join("threads/chatgpt/review-pr-341.lock"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(lock_mode, 0o600);
        }
    }

    #[test]
    fn bounded_wait_times_out_without_falling_through_to_fork() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();

        let _first = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-first",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let err = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-waiter",
            false,
            &ThreadConflictPolicy::Wait(Some(std::time::Duration::from_millis(40))),
        )
        .unwrap_err();

        assert!(format!("{err:#}").contains("thread_busy_timeout"));
        assert!(started.elapsed() >= std::time::Duration::from_millis(40));
    }

    #[test]
    fn bounded_wait_resolves_the_newest_thread_after_the_holder_releases() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let completed_session = sessions_dir.join("20260725_170000_holder");
        fs::create_dir_all(&completed_session).unwrap();

        let first = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-first",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            write_followup_metadata_for_recipe(
                &completed_session,
                "20260725_170000_holder",
                BuiltinWebRecipe::Chatgpt,
                &WebConversation {
                    id: "conversation-from-holder".to_string(),
                    url: "https://chatgpt.com/c/conversation-from-holder".to_string(),
                },
                "holder-hash",
                Some("review-pr-341"),
            )
            .unwrap();
            drop(first);
        });

        let waiter = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-waiter",
            false,
            &ThreadConflictPolicy::Wait(Some(std::time::Duration::from_secs(1))),
        )
        .unwrap();
        release.join().unwrap();

        match waiter.disposition() {
            PreparedThreadDisposition::Labeled {
                resolved: Some(resolved),
                ..
            } => assert_eq!(resolved.conversation.id, "conversation-from-holder"),
            other => panic!("unexpected disposition: {other:?}"),
        }
    }

    #[test]
    fn fork_is_opt_in_fresh_and_never_repoints_the_original_label() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let original_session = sessions_dir.join("20260725_160000_original");
        let fork_session = sessions_dir.join("20260725_170000_fork");
        fs::create_dir_all(&original_session).unwrap();
        fs::create_dir_all(&fork_session).unwrap();
        write_followup_metadata_for_recipe(
            &original_session,
            "20260725_160000_original",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "original-conversation".to_string(),
                url: "https://chatgpt.com/c/original-conversation".to_string(),
            },
            "original-hash",
            Some("review-pr-341"),
        )
        .unwrap();

        let _first = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-first",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let forked = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-fork",
            false,
            &ThreadConflictPolicy::Fork,
        )
        .unwrap();

        let (forked_from_label, forked_from_conversation_id) = match forked.disposition() {
            PreparedThreadDisposition::Forked {
                from_label,
                from_conversation_id,
            } => (from_label.as_str(), from_conversation_id.as_deref()),
            other => panic!("unexpected disposition: {other:?}"),
        };
        assert_eq!(forked_from_label, "review-pr-341");
        assert_eq!(forked_from_conversation_id, Some("original-conversation"));

        write_followup_metadata_for_recipe_with_lineage(
            &fork_session,
            "20260725_170000_fork",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "fork-conversation".to_string(),
                url: "https://chatgpt.com/c/fork-conversation".to_string(),
            },
            "fork-hash",
            None,
            Some(forked_from_label),
            forked_from_conversation_id,
        )
        .unwrap();

        let fork_metadata = read_followup_metadata(&fork_session).unwrap().unwrap();
        assert_eq!(fork_metadata.thread_label, None);
        assert_eq!(
            fork_metadata.forked_from_label.as_deref(),
            Some("review-pr-341")
        );
        assert_eq!(
            fork_metadata.forked_from_conversation_id.as_deref(),
            Some("original-conversation")
        );
        let original =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .unwrap();
        assert_eq!(original.conversation.id, "original-conversation");
    }

    #[test]
    #[cfg(unix)]
    fn thread_lock_release_is_not_pinned_by_a_forked_child_descriptor() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(&sessions_dir).unwrap();
        let lease = prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-first",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .unwrap();
        let _child = crate::test_support::ForkChild::sleep_for(std::time::Duration::from_secs(5));

        drop(lease);

        assert!(prepare_thread_run_in(
            "review-pr-341",
            &sessions_dir,
            BuiltinWebRecipe::Chatgpt,
            "run-second",
            false,
            &ThreadConflictPolicy::Fail,
        )
        .is_ok());
    }
}
