use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process;
#[cfg(unix)]
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;

use crate::chatgpt_recipe::{
    ChatgptModelSelectionStatus, ChatgptRecipeDiagnostics, ChatgptRecipeSpec, ChatgptTransportPhase,
};
use crate::claude_recipe::ClaudeRecipeSpec;
use crate::web_recipe::BuiltinWebRecipe;
use yoetz_core::output::{write_jsonl, OutputFormat};
use yoetz_core::paths::home_dir;

pub const TRANSPORT_NAME: &str = "chrome-extension-native";
pub const PROTOCOL_VERSION: u32 = 1;
pub const YOETZ_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");
pub const EXTENSION_ID: &str = "njdakhppfigmloihiikbjmheejfndbfa";
pub const EXTENSION_KEY: &str = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAujviQNA7EjHnfqpn3TM5IfgmHzOnvtu5pXg3Y1rS5koNJBT2PSG7FTGi9wD4oqNLVFehKm5h46vq1u1ACsMjAUrqMMUVvf7RUeqieUmfbtKRmx24N2blfz4b8KYpMlNUhf8IZ5TAFbvzy9NEO2KHAHCV6pP84E4lLBW2OQIDhqJd0FfS3Ecn91pbsH3tcsU6Gu+WiPEHLXZjPj85KcgQ+8qL0Xz83V5hEXIocMlCQ0RnMOfQIp5qUEIKgZ7qKqEjW2czNz48s5Fdgzbv95Lf09vat1NWiDHXZtDPWIa6TRjlKAAXIwsz5A/DJibzWiCgKiuOWmCgQPJgDidoyj/7RQIDAQAB";
pub const NATIVE_HOST_NAME: &str = "com.yoetz.chatgpt_native";
pub const SOCKET_FILENAME: &str = "chatgpt-native.sock";
pub const TOKEN_FILENAME: &str = "chatgpt-native.token";
pub const STATUS_FILENAME: &str = "chatgpt-native-status.json";
pub const WRAPPER_FILENAME: &str = "yoetz-chrome-native-host";
pub const INSTANCES_DIRNAME: &str = "instances";
const EXTENSION_LIFECYCLE_LOCK_FILENAME: &str = "extension-lifecycle.lock";
pub const CHROME_EXTENSIONS_URL: &str = "chrome://extensions/";
pub const CHATGPT_EXTENSION_DIR_ENV: &str = "YOETZ_CHATGPT_NATIVE_EXTENSION_DIR";
pub const MAX_CHROME_NATIVE_EXTENSION_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES: usize = 1024 * 1024;
pub const MAX_FRAME_BYTES: usize = MAX_CHROME_NATIVE_EXTENSION_MESSAGE_BYTES;
pub const MAX_BUNDLE_BYTES: u64 = 10 * 1024 * 1024;
const CHUNK_BYTES: usize = 192 * 1024;
const CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(10);
const RECIPE_READ_GRACE: Duration = Duration::from_secs(60);
const EXTENSION_RELOAD_VERIFY_TIMEOUT: Duration = Duration::from_secs(20);
const EXTENSION_RELOAD_VERIFY_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(unix)]
const MAX_UNIX_SOCKET_PATH_BYTES: usize = 100;

fn default_extension_recipes() -> Vec<String> {
    vec!["chatgpt".to_string()]
}

#[derive(Debug, Error)]
#[error("frame is too large: {len} bytes, max {max} bytes")]
struct FrameTooLargeError {
    len: usize,
    max: usize,
}

#[derive(Debug, Error)]
#[error("{message}")]
struct ConversationJobError {
    message: String,
}

pub(crate) fn is_conversation_job_error(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|cause| cause.downcast_ref::<ConversationJobError>().is_some())
}

pub(crate) fn with_thread_conversation_recovery_hint(
    err: anyhow::Error,
    thread_label: Option<&str>,
) -> anyhow::Error {
    match (thread_label, is_conversation_job_error(&err)) {
        (Some(label), true) => err.context(format!(
            "thread `{label}` could not resume its saved conversation; start a new conversation with `--thread {label} --fresh`"
        )),
        _ => err,
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct InstallHostResult {
    pub status: &'static str,
    pub native_host_name: &'static str,
    pub extension_id: &'static str,
    pub manifest_path: PathBuf,
    pub wrapper_path: PathBuf,
    pub socket_path: PathBuf,
    pub token_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ExtensionStatus {
    pub status: &'static str,
    pub native_host_name: &'static str,
    pub extension_id: &'static str,
    pub hello_seen: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_profile_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_profile_id: Option<String>,
    pub manifest_path: PathBuf,
    pub manifest_installed: bool,
    pub wrapper_path: PathBuf,
    pub wrapper_installed: bool,
    pub socket_path: PathBuf,
    pub socket_reachable: bool,
    pub token_path: PathBuf,
    pub token_present: bool,
    pub status_path: PathBuf,
    pub status_file_present: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub connected_instances: Vec<ExtensionInstanceStatus>,
    pub recipes: Vec<String>,
    pub claude_ready: bool,
    pub protocol_version: u32,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExtensionInstanceStatus {
    pub native_instance_id: String,
    pub socket_path: PathBuf,
    pub pid: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extension_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default = "default_extension_recipes")]
    pub recipes: Vec<String>,
    pub protocol_version: u32,
    pub last_seen_ms: u128,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtensionRecipeResult {
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: ChatgptModelSelectionStatus,
    pub warnings: Vec<String>,
    pub warning_details: Vec<Value>,
    pub conversation_id: Option<String>,
    pub conversation_url: Option<String>,
    pub diagnostics: ChatgptRecipeDiagnostics,
}

impl ExtensionRecipeResult {
    fn warning_values(&self) -> Vec<Value> {
        self.warnings
            .iter()
            .cloned()
            .map(Value::String)
            .chain(self.warning_details.iter().cloned())
            .collect()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ManagedExtensionUpdateResult {
    pub status: &'static str,
    pub source_dir: PathBuf,
    pub source_version: String,
    pub source_provenance: &'static str,
    pub extension_dir: PathBuf,
    pub loaded_extension_dirs: Vec<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_manifest_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_version: Option<String>,
    pub copied_files: usize,
}

#[derive(Debug)]
struct ExtensionLifecycleLock {
    _file: File,
}

impl Drop for ExtensionLifecycleLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self._file);
    }
}

pub(crate) struct ExtensionRecipeLease {
    _lifecycle_lock: ExtensionLifecycleLock,
    paths: ExtensionPaths,
    instance: ExtensionInstanceStatus,
    recipe: BuiltinWebRecipe,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolEnvelope {
    pub protocol_version: u32,
    pub transport: String,
    pub request_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capability_token: Option<String>,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub payload: Value,
}

impl ProtocolEnvelope {
    fn new(
        kind: impl Into<String>,
        job_id: Option<String>,
        run_id: Option<String>,
        payload: Value,
    ) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            transport: TRANSPORT_NAME.to_string(),
            request_id: new_id("req"),
            job_id,
            run_id,
            workspace_id: workspace_id().ok(),
            capability_token: None,
            kind: kind.into(),
            payload,
        }
    }

    fn with_token(mut self, token: String) -> Self {
        self.capability_token = Some(token);
        self
    }
}

fn extension_lifecycle_lock_path() -> Result<PathBuf> {
    Ok(extension_paths()?
        .state_dir
        .join(EXTENSION_LIFECYCLE_LOCK_FILENAME))
}

fn open_extension_lifecycle_lock() -> Result<(File, PathBuf)> {
    let path = extension_lifecycle_lock_path()?;
    let parent = path
        .parent()
        .context("extension lifecycle lock must have a parent directory")?;
    #[cfg(unix)]
    ensure_private_dir(parent)?;
    #[cfg(not(unix))]
    fs::create_dir_all(parent).with_context(|| {
        format!(
            "create extension lifecycle lock directory {}",
            parent.display()
        )
    })?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open extension lifecycle lock {}", path.display()))?;
    Ok((file, path))
}

fn acquire_extension_lifecycle_shared(action: &str) -> Result<ExtensionLifecycleLock> {
    let (file, path) = open_extension_lifecycle_lock()?;
    match FileExt::try_lock_shared(&file) {
        Ok(()) => Ok(ExtensionLifecycleLock { _file: file }),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => bail!(
            "extension_lifecycle_busy: cannot start {action} while extension setup, update, reload, or auto-heal is in progress ({})",
            path.display()
        ),
        Err(err) => Err(err)
            .with_context(|| format!("lock extension lifecycle shared {}", path.display())),
    }
}

fn acquire_extension_lifecycle_exclusive(action: &str) -> Result<ExtensionLifecycleLock> {
    let (file, path) = open_extension_lifecycle_lock()?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(ExtensionLifecycleLock { _file: file }),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => bail!(
            "extension_lifecycle_busy: cannot {action} while native extension recipes or another lifecycle mutation are active ({})",
            path.display()
        ),
        Err(err) => Err(err)
            .with_context(|| format!("lock extension lifecycle exclusive {}", path.display())),
    }
}

pub fn install_host() -> Result<InstallHostResult> {
    let _lifecycle_lock = acquire_extension_lifecycle_exclusive("install native host")?;
    install_host_unlocked()
}

fn install_host_unlocked() -> Result<InstallHostResult> {
    #[cfg(unix)]
    {
        let paths = extension_paths()?;
        ensure_private_dir(&paths.state_dir)?;
        fs::create_dir_all(
            paths
                .manifest_path
                .parent()
                .context("native host manifest path must have a parent")?,
        )?;

        ensure_capability_token(&paths.token_path)?;
        write_wrapper(&paths.wrapper_path)?;
        let manifest = native_host_manifest(&paths.wrapper_path)?;
        fs::write(
            &paths.manifest_path,
            serde_json::to_string_pretty(&manifest)? + "\n",
        )
        .with_context(|| {
            format!(
                "write native host manifest {}",
                paths.manifest_path.display()
            )
        })?;

        Ok(InstallHostResult {
            status: "installed",
            native_host_name: NATIVE_HOST_NAME,
            extension_id: EXTENSION_ID,
            manifest_path: paths.manifest_path,
            wrapper_path: paths.wrapper_path,
            socket_path: paths.socket_path,
            token_path: paths.token_path,
        })
    }
    #[cfg(not(unix))]
    {
        bail!("chrome-extension-native install-host is currently supported on macOS/Linux only")
    }
}

pub fn setup_extension() -> Result<(InstallHostResult, ManagedExtensionUpdateResult)> {
    let _lifecycle_lock = acquire_extension_lifecycle_exclusive("set up native extension")?;
    Ok((
        install_host_unlocked()?,
        prepare_managed_chatgpt_extension_unlocked()?,
    ))
}

pub fn chatgpt_extension_source_dir() -> Option<PathBuf> {
    chatgpt_extension_source_dir_candidates()
        .into_iter()
        .find_map(|candidate| {
            if !is_chatgpt_extension_source_dir(&candidate) {
                return None;
            }
            Some(candidate.canonicalize().unwrap_or(candidate))
        })
}

pub fn chatgpt_extension_source_dir_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = env::var(CHATGPT_EXTENSION_DIR_ENV) {
        let path = path.trim();
        if !path.is_empty() {
            candidates.push(PathBuf::from(path));
        }
    }
    if let Ok(cwd) = env::current_dir() {
        candidates.push(cwd.join("extensions").join("chatgpt-native"));
    }

    let crate_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    candidates.push(
        crate_dir
            .join("..")
            .join("..")
            .join("extensions")
            .join("chatgpt-native"),
    );

    if let Ok(exe) = env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            candidates.push(
                prefix
                    .join("share")
                    .join("yoetz")
                    .join("extensions")
                    .join("chatgpt-native"),
            );
            candidates.push(prefix.join("share").join("yoetz").join("chatgpt-native"));
        }
    }

    candidates
}

fn is_chatgpt_extension_source_dir(path: &Path) -> bool {
    path.join("manifest.json").is_file()
        && path.join("src").join("service-worker.js").is_file()
        && path.join("src").join("content-script.js").is_file()
}

pub fn managed_chatgpt_extension_dir() -> Result<PathBuf> {
    Ok(yoetz_state_dir()?.join("chatgpt-native-extension"))
}

fn legacy_loaded_chatgpt_extension_dir() -> Result<PathBuf> {
    Ok(yoetz_state_dir()?
        .join("chrome-extension-native")
        .join("unpacked"))
}

fn prepare_managed_chatgpt_extension_unlocked() -> Result<ManagedExtensionUpdateResult> {
    let source_dir = chatgpt_extension_source_dir().with_context(|| {
        format!("could not find ChatGPT native extension source; set {CHATGPT_EXTENSION_DIR_ENV}")
    })?;
    let extension_dir = managed_chatgpt_extension_dir()?;
    let mut result = sync_managed_chatgpt_extension_from(&source_dir, &extension_dir)?;
    let legacy_dir = legacy_loaded_chatgpt_extension_dir()?;
    if legacy_dir.exists() && !paths_refer_to_same_location(&legacy_dir, &extension_dir) {
        sync_extension_dir_exact(&extension_dir, &legacy_dir).with_context(|| {
            format!(
                "sync legacy loaded ChatGPT native extension directory {}",
                legacy_dir.display()
            )
        })?;
        result.loaded_extension_dirs.push(legacy_dir);
    }
    Ok(result)
}

fn sync_managed_chatgpt_extension_from(
    source_dir: &Path,
    extension_dir: &Path,
) -> Result<ManagedExtensionUpdateResult> {
    if !is_chatgpt_extension_source_dir(source_dir) {
        bail!(
            "ChatGPT native extension source is incomplete: {}",
            source_dir.display()
        );
    }
    let source_version = required_extension_manifest_version(source_dir)?;
    let source_provenance = extension_source_provenance(source_dir);
    ensure_extension_source_matches_cli(source_dir, &source_version, source_provenance)?;
    let previous_manifest_version = extension_manifest_version(extension_dir);
    if paths_refer_to_same_location(source_dir, extension_dir) {
        return Ok(ManagedExtensionUpdateResult {
            status: "current",
            source_dir: source_dir.to_path_buf(),
            source_version,
            source_provenance,
            extension_dir: extension_dir.to_path_buf(),
            loaded_extension_dirs: vec![extension_dir.to_path_buf()],
            previous_manifest_version: previous_manifest_version.clone(),
            manifest_version: previous_manifest_version,
            copied_files: count_regular_files(extension_dir)?,
        });
    }

    if !managed_chatgpt_extension_needs_sync(source_dir, extension_dir)? {
        return Ok(ManagedExtensionUpdateResult {
            status: "current",
            source_dir: source_dir.to_path_buf(),
            source_version,
            source_provenance,
            extension_dir: extension_dir.to_path_buf(),
            loaded_extension_dirs: vec![extension_dir.to_path_buf()],
            previous_manifest_version: previous_manifest_version.clone(),
            manifest_version: previous_manifest_version,
            copied_files: 0,
        });
    }

    let stamped_version =
        next_managed_extension_version(&source_version, previous_manifest_version.as_deref())?;
    let status = if is_chatgpt_extension_source_dir(extension_dir)
        && previous_manifest_version.as_deref() == Some(source_version.as_str())
    {
        "restamped"
    } else {
        "updated"
    };
    let copied_files =
        copy_extension_dir_atomically(source_dir, extension_dir, Some(stamped_version.as_str()))?;

    Ok(ManagedExtensionUpdateResult {
        status,
        source_dir: source_dir.to_path_buf(),
        source_version,
        source_provenance,
        extension_dir: extension_dir.to_path_buf(),
        loaded_extension_dirs: vec![extension_dir.to_path_buf()],
        previous_manifest_version,
        manifest_version: Some(stamped_version),
        copied_files,
    })
}

fn sync_extension_dir_exact(source_dir: &Path, extension_dir: &Path) -> Result<usize> {
    if !is_chatgpt_extension_source_dir(source_dir) {
        bail!(
            "ChatGPT native extension source is incomplete: {}",
            source_dir.display()
        );
    }
    if paths_refer_to_same_location(source_dir, extension_dir) {
        return Ok(0);
    }
    if is_chatgpt_extension_source_dir(extension_dir)
        && extension_dir_fingerprint(source_dir, None)?
            == extension_dir_fingerprint(extension_dir, None)?
    {
        return Ok(0);
    }
    copy_extension_dir_atomically(source_dir, extension_dir, None)
}

fn copy_extension_dir_atomically(
    source_dir: &Path,
    extension_dir: &Path,
    stamped_version: Option<&str>,
) -> Result<usize> {
    let parent = extension_dir
        .parent()
        .context("managed extension directory must have a parent")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    ensure_replaceable_extension_dir(extension_dir)?;
    let name = extension_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("chatgpt-native-extension");
    let temp_dir = parent.join(format!(".{name}.tmp-{}", new_id("sync")));
    let backup_dir = parent.join(format!(".{name}.old-{}", new_id("sync")));
    if temp_dir.exists() {
        fs::remove_dir_all(&temp_dir)
            .with_context(|| format!("remove stale temp {}", temp_dir.display()))?;
    }
    if backup_dir.exists() {
        fs::remove_dir_all(&backup_dir)
            .with_context(|| format!("remove stale backup {}", backup_dir.display()))?;
    }

    let copied_files = copy_extension_dir_contents(source_dir, &temp_dir)
        .with_context(|| format!("copy extension source from {}", source_dir.display()))?;
    if let Some(version) = stamped_version {
        write_extension_manifest_version(&temp_dir, version)?;
    }
    if !is_chatgpt_extension_source_dir(&temp_dir) {
        let _ = fs::remove_dir_all(&temp_dir);
        bail!(
            "copied ChatGPT native extension is incomplete: {}",
            temp_dir.display()
        );
    }

    let had_existing = extension_dir.exists();
    if had_existing {
        fs::rename(extension_dir, &backup_dir).with_context(|| {
            format!(
                "move existing managed extension {} aside",
                extension_dir.display()
            )
        })?;
    }
    if let Err(err) = fs::rename(&temp_dir, extension_dir) {
        if had_existing {
            let _ = fs::rename(&backup_dir, extension_dir);
        }
        return Err(err).with_context(|| {
            format!(
                "activate managed extension copy at {}",
                extension_dir.display()
            )
        });
    }
    if had_existing {
        fs::remove_dir_all(&backup_dir)
            .with_context(|| format!("remove old managed extension {}", backup_dir.display()))?;
    }
    Ok(copied_files)
}

fn managed_chatgpt_extension_needs_sync(source_dir: &Path, extension_dir: &Path) -> Result<bool> {
    if !is_chatgpt_extension_source_dir(extension_dir) {
        return Ok(true);
    }
    let source_version = required_extension_manifest_version(source_dir)?;
    if managed_extension_counter(
        &source_version,
        extension_manifest_version(extension_dir).as_deref(),
    )
    .is_none()
    {
        return Ok(true);
    }
    Ok(
        extension_dir_fingerprint(source_dir, Some(&source_version))?
            != extension_dir_fingerprint(extension_dir, Some(&source_version))?,
    )
}

fn paths_refer_to_same_location(left: &Path, right: &Path) -> bool {
    let Ok(left) = left.canonicalize() else {
        return false;
    };
    let Ok(right) = right.canonicalize() else {
        return false;
    };
    left == right
}

fn ensure_replaceable_extension_dir(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() {
        bail!(
            "managed extension directory must not be a symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!(
            "managed extension path exists but is not a directory: {}",
            path.display()
        );
    }
    Ok(())
}

fn copy_extension_dir_contents(source_dir: &Path, target_dir: &Path) -> Result<usize> {
    fs::create_dir_all(target_dir).with_context(|| format!("create {}", target_dir.display()))?;
    let mut copied = 0;
    for entry in
        fs::read_dir(source_dir).with_context(|| format!("read {}", source_dir.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target_dir.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .with_context(|| format!("inspect {}", source_path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "extension source must not contain symlinks: {}",
                source_path.display()
            );
        }
        if metadata.is_dir() {
            copied += copy_extension_dir_contents(&source_path, &target_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copy extension file {} to {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
            copied += 1;
        } else {
            bail!(
                "extension source contains unsupported file type: {}",
                source_path.display()
            );
        }
    }
    Ok(copied)
}

fn count_regular_files(root: &Path) -> Result<usize> {
    Ok(extension_file_paths(root)?.len())
}

fn extension_dir_fingerprint(
    root: &Path,
    manifest_version_override: Option<&str>,
) -> Result<Vec<(PathBuf, String)>> {
    let mut files = Vec::new();
    for relative in extension_file_paths(root)? {
        let path = root.join(&relative);
        let mut bytes = fs::read(&path)
            .with_context(|| format!("read extension file {}", root.join(&relative).display()))?;
        if relative == Path::new("manifest.json") {
            let mut manifest = serde_json::from_slice::<Value>(&bytes)
                .with_context(|| format!("parse extension manifest {}", path.display()))?;
            if let Some(version) = manifest_version_override {
                manifest["version"] = Value::String(version.to_string());
            }
            bytes = serde_json::to_vec(&manifest)
                .with_context(|| format!("normalize extension manifest {}", path.display()))?;
        }
        let mut hash = Sha256::new();
        hash.update(&bytes);
        files.push((relative, hex::encode(hash.finalize())));
    }
    Ok(files)
}

fn extension_file_paths(root: &Path) -> Result<Vec<PathBuf>> {
    fn walk(base: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
        for entry in fs::read_dir(current).with_context(|| format!("read {}", current.display()))? {
            let entry = entry?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .with_context(|| format!("inspect {}", path.display()))?;
            if metadata.file_type().is_symlink() {
                bail!(
                    "extension directory must not contain symlinks: {}",
                    path.display()
                );
            }
            if metadata.is_dir() {
                walk(base, &path, files)?;
            } else if metadata.is_file() {
                files.push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
            } else {
                bail!(
                    "extension directory contains unsupported file type: {}",
                    path.display()
                );
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    files.sort();
    Ok(files)
}

fn extension_manifest_version(extension_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(extension_dir.join("manifest.json")).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    value
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn required_extension_manifest_version(extension_dir: &Path) -> Result<String> {
    extension_manifest_version(extension_dir).with_context(|| {
        format!(
            "extension manifest has no version: {}",
            extension_dir.join("manifest.json").display()
        )
    })
}

fn extension_source_provenance(source_dir: &Path) -> &'static str {
    if env::var(CHATGPT_EXTENSION_DIR_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|value| paths_refer_to_same_location(source_dir, Path::new(value.trim())))
    {
        return "environment_override";
    }
    if env::current_dir()
        .ok()
        .map(|cwd| cwd.join("extensions").join("chatgpt-native"))
        .is_some_and(|candidate| paths_refer_to_same_location(source_dir, &candidate))
    {
        return "working_directory";
    }
    let crate_source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("extensions")
        .join("chatgpt-native");
    if paths_refer_to_same_location(source_dir, &crate_source) {
        return "source_checkout";
    }
    if let Ok(exe) = env::current_exe() {
        if let Some(prefix) = exe.parent().and_then(Path::parent) {
            for candidate in [
                prefix
                    .join("share")
                    .join("yoetz")
                    .join("extensions")
                    .join("chatgpt-native"),
                prefix.join("share").join("yoetz").join("chatgpt-native"),
            ] {
                if paths_refer_to_same_location(source_dir, &candidate) {
                    return "installed_share";
                }
            }
        }
    }
    "explicit_path"
}

fn ensure_extension_source_matches_cli(
    source_dir: &Path,
    source_version: &str,
    source_provenance: &str,
) -> Result<()> {
    let source_parts = chrome_extension_version_parts(source_version).with_context(|| {
        format!("extension source version `{source_version}` is not a valid Chrome version")
    })?;
    let cli_parts = chrome_extension_version_parts(YOETZ_CLI_VERSION).with_context(|| {
        format!("yoetz CLI version `{YOETZ_CLI_VERSION}` is not a valid Chrome version")
    })?;
    if source_parts.len() != 3 {
        bail!(
            "refusing to sync extension source version `{source_version}` from {} ({source_provenance}); source versions must have exactly three components for yoetz CLI {YOETZ_CLI_VERSION}",
            source_dir.display()
        );
    }
    if cli_parts.len() < 3 {
        bail!("yoetz CLI version `{YOETZ_CLI_VERSION}` must have at least three components");
    }
    if source_parts[..3] == cli_parts[..3] {
        return Ok(());
    }
    let relationship = if source_parts[..3] < cli_parts[..3] {
        "older than"
    } else {
        "newer than"
    };
    bail!(
        "refusing to sync extension source version {source_version} from {} ({source_provenance}); it is {relationship} yoetz CLI {YOETZ_CLI_VERSION}",
        source_dir.display()
    )
}

fn chrome_extension_version_parts(version: &str) -> Option<Vec<u16>> {
    let parts = version
        .trim()
        .split('.')
        .map(str::parse::<u16>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .ok()?;
    (1..=4).contains(&parts.len()).then_some(parts)
}

fn cli_version_prefix() -> Option<[u16; 3]> {
    let parts = chrome_extension_version_parts(YOETZ_CLI_VERSION)?;
    (parts.len() >= 3).then(|| [parts[0], parts[1], parts[2]])
}

fn extension_version_is_cli_compatible(version: &str) -> bool {
    let Some(expected) = cli_version_prefix() else {
        return version.trim() == YOETZ_CLI_VERSION;
    };
    let Some(parts) = chrome_extension_version_parts(version) else {
        return false;
    };
    parts.len() >= 3 && parts[..3] == expected
}

fn managed_extension_counter(source_version: &str, managed_version: Option<&str>) -> Option<u16> {
    let source_parts = chrome_extension_version_parts(source_version)?;
    if source_parts.len() != 3 {
        return None;
    }
    let managed_parts = chrome_extension_version_parts(managed_version?)?;
    (managed_parts.len() == 4 && managed_parts[..3] == source_parts[..3]).then(|| managed_parts[3])
}

fn next_managed_extension_version(
    source_version: &str,
    current_managed_version: Option<&str>,
) -> Result<String> {
    let source_parts = chrome_extension_version_parts(source_version).with_context(|| {
        format!("extension version `{source_version}` is not a valid Chrome extension version")
    })?;
    if source_parts.len() != 3 {
        bail!(
            "managed extension source version must have exactly three components, got `{source_version}`"
        );
    }
    let counter = managed_extension_counter(source_version, current_managed_version)
        .unwrap_or(0)
        .checked_add(1)
        .context("managed extension version counter exhausted at 65535")?;
    Ok(format!("{source_version}.{counter}"))
}

fn write_extension_manifest_version(extension_dir: &Path, version: &str) -> Result<()> {
    let manifest_path = extension_dir.join("manifest.json");
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read extension manifest {}", manifest_path.display()))?;
    let mut manifest = serde_json::from_str::<Value>(&text)
        .with_context(|| format!("parse extension manifest {}", manifest_path.display()))?;
    manifest["version"] = Value::String(version.to_string());
    let mut rendered = serde_json::to_string_pretty(&manifest)
        .with_context(|| format!("render extension manifest {}", manifest_path.display()))?;
    rendered.push('\n');
    fs::write(&manifest_path, rendered)
        .with_context(|| format!("write extension manifest {}", manifest_path.display()))
}

fn extension_version_skew_message(
    extension_version: Option<&str>,
    site_flag: Option<&str>,
) -> Option<String> {
    let extension_version = extension_version?.trim();
    if extension_version.is_empty() || extension_version_is_cli_compatible(extension_version) {
        return None;
    }
    let update_hint = match site_flag {
        Some(site_flag) => format!("`yoetz browser extension update {site_flag}`"),
        None => "`yoetz browser extension update --chatgpt` or `yoetz browser extension update --claude`".to_string(),
    };
    Some(format!(
        "loaded chrome-extension-native extension version {extension_version} does not match yoetz CLI {YOETZ_CLI_VERSION}; run {update_hint}"
    ))
}

fn managed_extension_identity_message(
    observed_version: Option<&str>,
    expected_version: Option<&str>,
    managed_dir: &Path,
) -> Option<String> {
    let observed = observed_version?.trim();
    let expected = expected_version?.trim();
    if observed.is_empty() || expected.is_empty() {
        return None;
    }
    let observed_parts = chrome_extension_version_parts(observed);
    let expected_parts = chrome_extension_version_parts(expected);
    if expected_parts.as_ref().is_none_or(|parts| parts.len() != 4) {
        return Some(format!(
            "managed extension copy at {} has no stamped sync identity; run `yoetz browser extension update --chatgpt` or `yoetz browser extension update --claude` to initialize it",
            managed_dir.display()
        ));
    }
    if observed == expected {
        return None;
    }
    if observed_parts
        .as_ref()
        .is_some_and(|parts| parts.len() == 3)
        && expected_parts
            .as_ref()
            .is_some_and(|parts| parts.len() == 4)
    {
        return Some(format!(
            "Chrome is running a non-managed copy of the Yoetz extension; one-time migration: remove the Yoetz card in chrome://extensions, then Load unpacked {}",
            managed_dir.display()
        ));
    }
    if extension_version_is_cli_compatible(observed)
        && extension_version_is_cli_compatible(expected)
    {
        return Some(format!(
            "Chrome is running a stale managed copy of the Yoetz extension (loaded {observed}, expected {expected}); run `yoetz browser extension update --chatgpt` or `yoetz browser extension update --claude`, or reload {} in chrome://extensions",
            managed_dir.display()
        ));
    }
    Some(format!(
        "loaded Yoetz extension version {observed} does not match managed copy {expected} at {}; run `yoetz browser extension update --chatgpt` or `yoetz browser extension update --claude`",
        managed_dir.display()
    ))
}

pub fn status() -> Result<ExtensionStatus> {
    let paths = extension_paths()?;
    let token_present = paths.token_path.exists();
    let manifest_installed = paths.manifest_path.exists();
    let wrapper_installed = paths.wrapper_path.exists();
    let status_file_present = paths.status_path.exists();
    let connected_instances = connected_extension_instances(&paths);
    let socket_reachable = socket_reachable(&paths.socket_path) || !connected_instances.is_empty();
    let status_value = read_status_file(&paths.status_path);
    let latest_instance_with_hello = connected_instances
        .iter()
        .filter(|instance| instance_has_extension_hello(instance))
        .max_by_key(|instance| instance.last_seen_ms);
    let extension_value = status_value
        .as_ref()
        .and_then(|value| value.get("extension"))
        .and_then(Value::as_object);
    let legacy_hello_seen = connected_instances.is_empty()
        && socket_reachable
        && status_file_has_extension_hello(extension_value);
    let hello_seen = latest_instance_with_hello.is_some() || legacy_hello_seen;
    let extension_version = latest_instance_with_hello
        .and_then(|instance| instance.extension_version.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "extension_version")
        });
    let extension_instance_id = latest_instance_with_hello
        .and_then(|instance| instance.extension_instance_id.clone())
        .or_else(|| {
            legacy_extension_status_string(
                legacy_hello_seen,
                extension_value,
                "extension_instance_id",
            )
        });
    let extension_profile_email = latest_instance_with_hello
        .and_then(|instance| instance.profile_email.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "profile_email")
        });
    let extension_profile_id = latest_instance_with_hello
        .and_then(|instance| instance.profile_id.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "profile_id")
        });
    let recipes = latest_instance_with_hello
        .map(|instance| instance.recipes.clone())
        .unwrap_or_else(|| legacy_extension_recipes(extension_value));
    let claude_ready =
        socket_reachable && hello_seen && recipes.iter().any(|recipe| recipe == "claude");
    let protocol_version_mismatch = status_value
        .as_ref()
        .and_then(|value| value.get("version_mismatch"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let extension_version_mismatch =
        extension_version_skew_message(extension_version.as_deref(), None);
    let version_mismatch = protocol_version_mismatch
        .clone()
        .or_else(|| extension_version_mismatch.clone());
    let managed_extension_dir = managed_chatgpt_extension_dir()?;
    let managed_extension_version = extension_manifest_version(&managed_extension_dir);
    let managed_copy_mismatch = hello_seen
        .then(|| {
            managed_extension_identity_message(
                extension_version.as_deref(),
                managed_extension_version.as_deref(),
                &managed_extension_dir,
            )
        })
        .flatten();
    let manual_handoff = status_value
        .as_ref()
        .and_then(|value| value.get("last_manual_handoff"))
        .and_then(Value::as_object)
        .is_some();
    let status = if version_mismatch.is_some() {
        "version_mismatch"
    } else if managed_copy_mismatch.is_some() {
        "managed_copy_mismatch"
    } else if socket_reachable && hello_seen {
        "connected"
    } else if manual_handoff {
        "manual_handoff"
    } else if socket_reachable {
        "missing_extension"
    } else if manifest_installed && wrapper_installed && token_present {
        "disconnected"
    } else {
        "not_installed"
    };
    let detail = match status {
        "connected" => {
            if let Some(email) = &extension_profile_email {
                format!(
                    "native host socket is reachable and extension hello was observed for Chrome profile email {email}"
                )
            } else {
                "native host socket is reachable and extension hello was observed".to_string()
            }
        }
        "missing_extension" => {
            "native host socket is reachable, but no extension hello was observed".to_string()
        }
        "version_mismatch" => status_value
            .as_ref()
            .and_then(|value| value.get("version_mismatch"))
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| version_mismatch.clone())
            .unwrap_or_else(|| "extension/native protocol version mismatch".to_string()),
        "managed_copy_mismatch" => managed_copy_mismatch
            .clone()
            .unwrap_or_else(|| "loaded extension does not match the managed copy".to_string()),
        "manual_handoff" => status_value
            .as_ref()
            .and_then(|value| value.get("last_manual_handoff"))
            .and_then(|value| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("last ChatGPT job requires manual handoff")
            .to_string(),
        "disconnected" => "native host is installed but no extension connection is reachable".to_string(),
        _ => "run `yoetz browser extension install-host --chatgpt` and install/load the Chrome extension".to_string(),
    };
    Ok(ExtensionStatus {
        status,
        native_host_name: NATIVE_HOST_NAME,
        extension_id: EXTENSION_ID,
        hello_seen,
        extension_version,
        extension_instance_id,
        extension_profile_email,
        extension_profile_id,
        manifest_path: paths.manifest_path,
        manifest_installed,
        wrapper_path: paths.wrapper_path,
        wrapper_installed,
        socket_path: paths.socket_path,
        socket_reachable,
        token_path: paths.token_path,
        token_present,
        status_path: paths.status_path,
        status_file_present,
        connected_instances,
        recipes,
        claude_ready,
        protocol_version: PROTOCOL_VERSION,
        detail,
    })
}

pub fn prune_stale_instance_records() -> Result<usize> {
    let paths = extension_paths()?;
    Ok(prune_stale_instance_records_at(&paths))
}

pub fn doctor() -> Result<DoctorReport> {
    let paths = extension_paths()?;
    let status_value = read_status_file(&paths.status_path);
    let connected_instances = connected_extension_instances(&paths);
    let latest_instance_with_hello = connected_instances
        .iter()
        .filter(|instance| instance_has_extension_hello(instance))
        .max_by_key(|instance| instance.last_seen_ms);
    let extension_value = status_value
        .as_ref()
        .and_then(|value| value.get("extension"))
        .and_then(Value::as_object);
    let socket_is_reachable =
        socket_reachable(&paths.socket_path) || !connected_instances.is_empty();
    let legacy_hello_seen = connected_instances.is_empty()
        && socket_is_reachable
        && status_file_has_extension_hello(extension_value);
    let extension_status = latest_instance_with_hello
        .map(|instance| {
            let version = instance
                .extension_version
                .as_deref()
                .unwrap_or("unknown-extension-version");
            let extension_instance_id = instance
                .extension_instance_id
                .as_deref()
                .unwrap_or("unknown-extension-instance");
            match instance.profile_email.as_deref() {
                Some(email) if !email.is_empty() => {
                    format!("extension_version={version}, extension_instance_id={extension_instance_id}, chrome_profile_email={email}")
                }
                _ => format!("extension_version={version}, extension_instance_id={extension_instance_id}"),
            }
        })
        .or_else(|| {
            if !legacy_hello_seen {
                return None;
            }
            let value = extension_value?;
            let version = value.get("extension_version").and_then(Value::as_str)?;
            let extension_instance_id = value
                .get("extension_instance_id")
                .and_then(Value::as_str)
                .unwrap_or("unknown-extension-instance");
            let profile_email = value.get("profile_email").and_then(Value::as_str);
            Some(match profile_email {
                Some(email) if !email.is_empty() => {
                    format!("extension_version={version}, extension_instance_id={extension_instance_id}, chrome_profile_email={email}")
                }
                _ => format!("extension_version={version}, extension_instance_id={extension_instance_id}"),
            })
        })
        .unwrap_or_else(|| "no extension hello observed".to_string());
    let extension_protocol = latest_instance_with_hello
        .map(|instance| instance.protocol_version as u64)
        .or_else(|| {
            if !legacy_hello_seen {
                return None;
            }
            extension_value
                .and_then(|value| value.get("protocol_version"))
                .and_then(Value::as_u64)
        });
    let observed_extension_version = latest_instance_with_hello
        .and_then(|instance| instance.extension_version.clone())
        .or_else(|| {
            legacy_extension_status_string(legacy_hello_seen, extension_value, "extension_version")
        });
    let extension_version_mismatch =
        extension_version_skew_message(observed_extension_version.as_deref(), None);
    let extension_instance_id = latest_instance_with_hello
        .and_then(|instance| instance.extension_instance_id.clone())
        .or_else(|| {
            legacy_extension_status_string(
                legacy_hello_seen,
                extension_value,
                "extension_instance_id",
            )
        });
    let managed_extension_dir = managed_chatgpt_extension_dir()?;
    let managed_extension_version = extension_manifest_version(&managed_extension_dir);
    let managed_copy_mismatch = managed_extension_identity_message(
        observed_extension_version.as_deref(),
        managed_extension_version.as_deref(),
        &managed_extension_dir,
    );
    let managed_copy_check = match (
        observed_extension_version.as_deref(),
        managed_extension_version.as_deref(),
        managed_copy_mismatch.as_deref(),
    ) {
        (_, _, Some(message)) => DoctorCheck {
            name: "managed_extension_copy",
            ok: false,
            detail: message.to_string(),
        },
        (Some(observed), Some(expected), None) if observed == expected => DoctorCheck {
            name: "managed_extension_copy",
            ok: true,
            detail: format!(
                "loaded managed extension version {expected} from {}",
                managed_extension_dir.display()
            ),
        },
        (None, _, None) => DoctorCheck {
            name: "managed_extension_copy",
            ok: false,
            detail: "no extension version observed".to_string(),
        },
        (_, None, None) => DoctorCheck {
            name: "managed_extension_copy",
            ok: true,
            detail: format!(
                "managed extension copy has not been prepared at {}",
                managed_extension_dir.display()
            ),
        },
        _ => DoctorCheck {
            name: "managed_extension_copy",
            ok: false,
            detail: "loaded extension identity could not be verified".to_string(),
        },
    };
    let version_detail = status_value
        .as_ref()
        .and_then(|value| value.get("version_mismatch"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| extension_version_mismatch.clone())
        .unwrap_or_else(|| {
            observed_extension_version
                .as_deref()
                .map(|version| {
                    format!(
                        "protocol_version={PROTOCOL_VERSION}, cli_version={YOETZ_CLI_VERSION}, extension_version={version}"
                    )
                })
                .unwrap_or_else(|| format!("protocol_version={PROTOCOL_VERSION}"))
        });
    let checks = vec![
        DoctorCheck {
            name: "manifest",
            ok: paths.manifest_path.exists(),
            detail: paths.manifest_path.display().to_string(),
        },
        DoctorCheck {
            name: "wrapper",
            ok: paths.wrapper_path.exists(),
            detail: paths.wrapper_path.display().to_string(),
        },
        wrapper_target_doctor_check(&paths.wrapper_path),
        token_doctor_check(&paths.token_path),
        DoctorCheck {
            name: "socket",
            ok: socket_is_reachable,
            detail: if connected_instances.is_empty() {
                paths.socket_path.display().to_string()
            } else {
                observed_extension_profiles(&connected_instances)
            },
        },
        DoctorCheck {
            name: "extension_hello",
            ok: extension_protocol.is_some(),
            detail: extension_status,
        },
        DoctorCheck {
            name: "version_compatible",
            ok: extension_protocol == Some(PROTOCOL_VERSION as u64)
                && extension_version_mismatch.is_none(),
            detail: version_detail,
        },
        managed_copy_check,
        DoctorCheck {
            name: "extension_instance_id",
            ok: extension_instance_id.is_some(),
            detail: extension_instance_id.unwrap_or_else(|| {
                "no extension instance id observed; run `yoetz browser extension update --chatgpt` or reload the managed unpacked extension in chrome://extensions".to_string()
            }),
        },
        identity_permission_doctor_check(latest_instance_with_hello, legacy_hello_seen, extension_value),
        DoctorCheck {
            name: "stable_extension_id",
            ok: EXTENSION_ID == extension_id_from_public_key(EXTENSION_KEY)?,
            detail: EXTENSION_ID.to_string(),
        },
    ];
    let ok = checks.iter().all(|check| check.ok);
    Ok(DoctorReport { ok, checks })
}

pub fn doctor_with_auth_probe(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<DoctorReport> {
    let mut report = doctor()?;
    report.checks.push(site_auth_doctor_check(selector, recipe));
    report.ok = report.checks.iter().all(|check| check.ok);
    Ok(report)
}

fn site_auth_doctor_check(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> DoctorCheck {
    match site_auth_probe(selector, recipe) {
        Ok(payload) => auth_doctor_check_from_payload(&payload, recipe),
        Err(error) => DoctorCheck {
            name: match recipe {
                BuiltinWebRecipe::Chatgpt => "chatgpt_auth",
                BuiltinWebRecipe::Claude => "claude_auth",
            },
            ok: recipe == BuiltinWebRecipe::Chatgpt,
            detail: format!("status=probe_unavailable, auth probe unavailable: {error}"),
        },
    }
}

#[cfg(test)]
fn chatgpt_auth_doctor_check_from_payload(payload: &Value) -> DoctorCheck {
    auth_doctor_check_from_payload(payload, BuiltinWebRecipe::Chatgpt)
}

fn auth_doctor_check_from_payload(payload: &Value, recipe: BuiltinWebRecipe) -> DoctorCheck {
    let status = payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let authenticated = payload
        .get("authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(status == "authenticated");
    let mut detail = vec![format!("status={status}")];
    if let Some(message) = payload.get("message").and_then(Value::as_str) {
        detail.push(message.to_string());
    }
    if let Some(tab_id) = payload.get("tab_id").and_then(Value::as_u64) {
        detail.push(format!("tab_id={tab_id}"));
    }
    if let Some(selection) = payload.get("selection").and_then(Value::as_str) {
        detail.push(format!("selection={selection}"));
    }
    if let Some(count) = payload.get("yoetz_owned_tabs_open").and_then(Value::as_u64) {
        detail.push(format!("yoetz_owned_tabs_open={count}"));
    }
    if let Some(count) = payload
        .get("yoetz_owned_complete_tabs_open")
        .and_then(Value::as_u64)
    {
        detail.push(format!("yoetz_owned_complete_tabs_open={count}"));
    }
    if let Some(url) = payload
        .get("url")
        .or_else(|| payload.get("tab_url"))
        .and_then(Value::as_str)
    {
        detail.push(format!("url={url}"));
    }
    DoctorCheck {
        name: match recipe {
            BuiltinWebRecipe::Chatgpt => "chatgpt_auth",
            BuiltinWebRecipe::Claude => "claude_auth",
        },
        ok: authenticated || !is_confirmed_unusable_chatgpt_auth_status(status),
        detail: detail.join(", "),
    }
}

fn is_confirmed_unusable_chatgpt_auth_status(status: &str) -> bool {
    // Only hard-fail doctor on statuses that prove ChatGPT cannot be used in the profile.
    matches!(
        status,
        "login_required" | "challenge_required" | "rate_limited"
    )
}

pub fn site_auth_probe(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<Value> {
    let response = send_site_control_job(
        "reconnect",
        json!({ "intent": "doctor_auth_probe", "recipe": recipe.as_str() }),
        selector,
        recipe,
    )?;
    Ok(response.payload)
}

pub fn reconnect(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job("reconnect", json!({ "intent": "reconnect" }), selector)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn reload_extension(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let _lifecycle_lock = acquire_extension_lifecycle_exclusive("reload native extension")?;
    reload_extension_unlocked(selector)
}

fn reload_extension_unlocked(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job(
        "reconnect",
        json!({ "intent": "reload_extension" }),
        selector,
    )?;
    let reload_started =
        response.payload.get("status").and_then(Value::as_str) == Some("reloading");
    if !reload_started {
        bail!(
            "connected extension did not acknowledge reload; run `yoetz browser extension update --chatgpt` or reload the managed unpacked extension in chrome://extensions"
        );
    }
    Ok(json!({
        "status": "reloading",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

pub fn update_extension(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<Value> {
    let _lifecycle_lock = acquire_extension_lifecycle_exclusive("update native extension")?;
    let paths = extension_paths()?;
    let previous_instance = select_extension_instance(&paths, selector)?;
    let update = prepare_managed_chatgpt_extension_unlocked()?;
    let expected_version = update
        .manifest_version
        .as_deref()
        .context("managed extension copy has no stamped manifest version")?;
    ensure_reload_can_reach_managed_copy(&previous_instance, &update)?;
    let reload = reload_extension_unlocked(selector)?;
    let instance = wait_for_extension_update(
        &paths,
        selector,
        expected_version,
        recipe.as_str(),
        &previous_instance.native_instance_id,
    )
    .with_context(|| {
        format!(
            "managed extension source {} was copied, but the loaded extension did not activate the requested site capability",
            update.source_dir.display()
        )
    })?;
    Ok(json!({
        "status": "updated",
        "transport": TRANSPORT_NAME,
        "extension_dir": update.extension_dir,
        "source_dir": update.source_dir,
        "source_version": update.source_version,
        "source_provenance": update.source_provenance,
        "loaded_extension_dirs": update.loaded_extension_dirs,
        "previous_manifest_version": update.previous_manifest_version,
        "manifest_version": update.manifest_version,
        "copy_status": update.status,
        "copied_files": update.copied_files,
        "reload": reload,
        "extension_instance": instance,
    }))
}

pub fn bridge_check_for_recipe(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<Value> {
    let response = send_site_control_job(
        "reconnect",
        json!({ "intent": "bridge_check", "recipe": recipe.as_str() }),
        selector,
        recipe,
    )?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "recipe": recipe.as_str(),
        "live": false,
        "response": response.payload,
    }))
}

fn instance_advertises_recipe(
    instance: &ExtensionInstanceStatus,
    recipe: BuiltinWebRecipe,
) -> bool {
    instance
        .recipes
        .iter()
        .any(|value| value == recipe.as_str())
}

pub fn recipe_ready(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<bool> {
    let paths = extension_paths()?;
    let instance = select_extension_instance(&paths, selector)?;
    Ok(instance_advertises_recipe(&instance, recipe))
}

pub fn canary(
    live: bool,
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<Value> {
    if !live {
        return bridge_check_for_recipe(selector, recipe);
    }
    let dir = tempfile::tempdir()?;
    let bundle_path = dir
        .path()
        .join(format!("yoetz-{}-native-canary.md", recipe.as_str()));
    fs::write(&bundle_path, "Reply with exactly OK.\n")?;
    // A successful automated canary should not leave a diagnostic tab behind.
    let close_tab_on_complete = true;
    let response = match recipe {
        BuiltinWebRecipe::Chatgpt => run_chatgpt_recipe(
            &ChatgptRecipeSpec {
                bundle_path: Some(bundle_path),
                model: crate::chatgpt_recipe::CHATGPT_SOL_EXTRA_HIGH_MODEL.to_string(),
                model_strategy: crate::chatgpt_recipe::ChatgptModelStrategy::Select,
                prompt: "Reply with exactly OK.".to_string(),
                browser_context_id: None,
                profile_email: selector.profile_email.map(str::to_string),
                extension_instance_id: selector.extension_instance_id.map(str::to_string),
                extension_profile_id: selector.extension_profile_id.map(str::to_string),
                conversation_id: None,
                run_id: new_id("canary"),
                wait_timeout_ms: 180_000,
                wait_interval_ms: 1_000,
                upload_timeout_ms: 30_000,
                send_timeout_ms: 120_000,
                close_tab_on_complete,
            },
            OutputFormat::Json,
        )?,
        BuiltinWebRecipe::Claude => run_claude_recipe(
            &ClaudeRecipeSpec {
                bundle_path: Some(bundle_path),
                prompt: "Reply with exactly OK.".to_string(),
                browser_context_id: None,
                profile_email: selector.profile_email.map(str::to_string),
                extension_instance_id: selector.extension_instance_id.map(str::to_string),
                extension_profile_id: selector.extension_profile_id.map(str::to_string),
                conversation_id: None,
                run_id: new_id("canary"),
                wait_timeout_ms: 180_000,
                wait_interval_ms: 1_000,
                upload_timeout_ms: 30_000,
                attachment_stall_timeout_ms: 0,
                send_timeout_ms: 120_000,
                close_tab_on_complete,
                warnings: Vec::new(),
            },
            OutputFormat::Json,
        )?,
    };
    validate_canary_response(&response.response)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "recipe": recipe.as_str(),
        "live": true,
        "expected_response": "OK",
        "response": response.response,
        "model_used": response.model_used,
        "model_selection_status": response.model_selection_status,
        "warnings": response.warning_values(),
    }))
}

pub fn inspect_run(
    run_id: &str,
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<Value> {
    let trimmed = run_id.trim();
    if trimmed.is_empty() {
        bail!("--run-id is required");
    }
    let response = send_site_control_job(
        "inspect_run",
        json!({ "run_id": trimmed, "recipe": recipe.as_str() }),
        selector,
        recipe,
    )?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "recipe": recipe.as_str(),
        "response": response.payload,
    }))
}

pub fn grant_identity_permission(selector: ExtensionInstanceSelector<'_>) -> Result<Value> {
    let response = send_control_job("request_identity_permission", json!({}), selector)?;
    Ok(json!({
        "status": "ok",
        "transport": TRANSPORT_NAME,
        "response": response.payload,
    }))
}

fn select_recipe_instance_with_lifecycle_lock(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
    recipe_flag: &str,
    action: &str,
) -> Result<(ExtensionLifecycleLock, ExtensionInstanceStatus)> {
    let mut lifecycle_lock = acquire_extension_lifecycle_shared(action)?;
    let mut instance = select_extension_instance(paths, selector)?;
    if let Some(message) =
        extension_version_skew_message(instance.extension_version.as_deref(), Some(recipe_flag))
    {
        eprintln!("warning: {message}");
        drop(lifecycle_lock);
        match auto_heal_extension_version_skew(paths, selector) {
            Ok(Some(_)) => eprintln!(
                "info: refreshed and reloaded chrome-extension-native extension from packaged source"
            ),
            Ok(None) => {}
            Err(err) => eprintln!("warning: automatic extension update failed: {err:#}"),
        }
        lifecycle_lock = acquire_extension_lifecycle_shared(action)?;
        instance = select_extension_instance(paths, selector)?;
    }
    Ok((lifecycle_lock, instance))
}

pub(crate) fn acquire_chatgpt_recipe_lease(
    spec: &ChatgptRecipeSpec,
) -> Result<ExtensionRecipeLease> {
    let bundle_path = spec
        .bundle_path
        .as_deref()
        .context("chrome-extension-native transport requires `--bundle`")?;
    validate_bundle_path(bundle_path)?;
    acquire_extension_recipe_lease(
        ExtensionInstanceSelector {
            profile_email: spec.profile_email.as_deref(),
            extension_instance_id: spec.extension_instance_id.as_deref(),
            extension_profile_id: spec.extension_profile_id.as_deref(),
        },
        BuiltinWebRecipe::Chatgpt,
        "--chatgpt",
        "ChatGPT native extension recipe",
    )
}

pub(crate) fn acquire_claude_recipe_lease(spec: &ClaudeRecipeSpec) -> Result<ExtensionRecipeLease> {
    let bundle_path = spec
        .bundle_path
        .as_deref()
        .context("chrome-extension-native transport requires `--bundle`")?;
    validate_bundle_path(bundle_path)?;
    acquire_extension_recipe_lease(
        ExtensionInstanceSelector {
            profile_email: spec.profile_email.as_deref(),
            extension_instance_id: spec.extension_instance_id.as_deref(),
            extension_profile_id: spec.extension_profile_id.as_deref(),
        },
        BuiltinWebRecipe::Claude,
        "--claude",
        "Claude native extension recipe",
    )
}

fn acquire_extension_recipe_lease(
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
    recipe_flag: &str,
    action: &str,
) -> Result<ExtensionRecipeLease> {
    let paths = extension_paths()?;
    let (lifecycle_lock, instance) =
        select_recipe_instance_with_lifecycle_lock(&paths, selector, recipe_flag, action)?;
    ensure_instance_matches_managed_copy(&instance)?;
    if recipe == BuiltinWebRecipe::Claude {
        ensure_instance_supports_recipe(&instance, "claude")?;
    }
    Ok(ExtensionRecipeLease {
        _lifecycle_lock: lifecycle_lock,
        paths,
        instance,
        recipe,
    })
}

pub fn run_chatgpt_recipe(
    spec: &ChatgptRecipeSpec,
    format: OutputFormat,
) -> Result<ExtensionRecipeResult> {
    let lease = acquire_chatgpt_recipe_lease(spec)?;
    run_chatgpt_recipe_with_lease(spec, format, &lease)
}

pub(crate) fn run_chatgpt_recipe_with_lease(
    spec: &ChatgptRecipeSpec,
    format: OutputFormat,
    lease: &ExtensionRecipeLease,
) -> Result<ExtensionRecipeResult> {
    if lease.recipe != BuiltinWebRecipe::Chatgpt {
        bail!("native extension recipe lease does not belong to ChatGPT");
    }
    let bundle_path = spec
        .bundle_path
        .as_deref()
        .context("chrome-extension-native transport requires `--bundle`")?;
    let bundle = validate_bundle_path(bundle_path)?;
    let token = read_capability_token(&lease.paths.token_path)?;
    let mut stream = connect_socket(&lease.instance.socket_path).with_context(|| {
        format!(
            "chrome-extension-native bridge is not connected at {}. Run `yoetz browser extension doctor --chatgpt`, then open Chrome with the Yoetz extension enabled.",
            lease.instance.socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(
        Duration::from_millis(spec.wait_timeout_ms).saturating_add(RECIPE_READ_GRACE),
    ))?;

    let job_id = new_id("job");
    let start = ProtocolEnvelope::new(
        "job_start",
        Some(job_id.clone()),
        Some(spec.run_id.clone()),
        chatgpt_job_start_payload(spec, &bundle),
    )
    .with_token(token);
    write_json_frame(&mut stream, &start)?;

    loop {
        let envelope = read_json_frame(&mut stream)?;
        validate_inbound_envelope(&envelope)?;
        match envelope.kind.as_str() {
            "job_progress" => emit_progress(format, &envelope)?,
            "job_complete" => return parse_recipe_result(envelope),
            "job_error" => return Err(job_error(envelope)),
            other => {
                if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                    eprintln!("info: ignored chrome-extension-native event `{other}`");
                }
            }
        }
    }
}

pub fn run_claude_recipe(
    spec: &ClaudeRecipeSpec,
    format: OutputFormat,
) -> Result<ExtensionRecipeResult> {
    let lease = acquire_claude_recipe_lease(spec)?;
    run_claude_recipe_with_lease(spec, format, &lease)
}

pub(crate) fn run_claude_recipe_with_lease(
    spec: &ClaudeRecipeSpec,
    format: OutputFormat,
    lease: &ExtensionRecipeLease,
) -> Result<ExtensionRecipeResult> {
    if lease.recipe != BuiltinWebRecipe::Claude {
        bail!("native extension recipe lease does not belong to Claude");
    }
    let bundle_path = spec
        .bundle_path
        .as_deref()
        .context("chrome-extension-native transport requires `--bundle`")?;
    let bundle = validate_bundle_path(bundle_path)?;
    let token = read_capability_token(&lease.paths.token_path)?;
    let mut stream = connect_socket(&lease.instance.socket_path).with_context(|| {
        format!(
            "chrome-extension-native bridge is not connected at {}. Run `yoetz browser extension doctor --claude`, then open Chrome with the Yoetz extension enabled.",
            lease.instance.socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(
        Duration::from_millis(spec.wait_timeout_ms).saturating_add(RECIPE_READ_GRACE),
    ))?;

    let job_id = new_id("job");
    let start = ProtocolEnvelope::new(
        "job_start",
        Some(job_id),
        Some(spec.run_id.clone()),
        claude_job_start_payload(spec, &bundle),
    )
    .with_token(token);
    write_json_frame(&mut stream, &start)?;

    loop {
        let envelope = read_json_frame(&mut stream)?;
        validate_inbound_envelope(&envelope)?;
        match envelope.kind.as_str() {
            "job_progress" => emit_progress(format, &envelope)?,
            "job_complete" => return parse_recipe_result(envelope),
            "job_error" => return Err(job_error_for_recipe(envelope, BuiltinWebRecipe::Claude)),
            other => {
                if matches!(format, OutputFormat::Text | OutputFormat::Markdown) {
                    eprintln!("info: ignored chrome-extension-native event `{other}`");
                }
            }
        }
    }
}

pub fn serve_native_host_chatgpt() -> Result<()> {
    #[cfg(unix)]
    {
        native_host_unix::serve()
    }
    #[cfg(not(unix))]
    {
        bail!("chrome-extension-native native host is currently supported on macOS/Linux only")
    }
}

fn chatgpt_job_start_payload(spec: &ChatgptRecipeSpec, bundle: &BundleInfo) -> Value {
    json!({
        "recipe": "chatgpt",
        "bundle_path": bundle.path,
        "file_name": bundle.file_name,
        "bundle_size": bundle.size,
        "mime": bundle.mime,
        "prompt": spec.prompt,
        "model": spec.model,
        "model_strategy": spec.model_strategy,
        "browser_context_id": spec.browser_context_id,
        "profile_email": spec.profile_email,
        "extension_instance_id": spec.extension_instance_id,
        "extension_profile_id": spec.extension_profile_id,
        "conversation_id": spec.conversation_id,
        "wait_timeout_ms": spec.wait_timeout_ms,
        "wait_interval_ms": spec.wait_interval_ms,
        "upload_timeout_ms": spec.upload_timeout_ms,
        "send_timeout_ms": spec.send_timeout_ms,
        "close_tab_on_complete": spec.close_tab_on_complete,
    })
}

fn claude_job_start_payload(spec: &ClaudeRecipeSpec, bundle: &BundleInfo) -> Value {
    json!({
        "recipe": "claude",
        "bundle_path": bundle.path,
        "file_name": bundle.file_name,
        "bundle_size": bundle.size,
        "mime": bundle.mime,
        "prompt": spec.prompt,
        "model": crate::claude_recipe::CLAUDE_FABLE_MAX_MODEL,
        "model_strategy": "select",
        "browser_context_id": spec.browser_context_id,
        "profile_email": spec.profile_email,
        "extension_instance_id": spec.extension_instance_id,
        "extension_profile_id": spec.extension_profile_id,
        "conversation_id": spec.conversation_id,
        "wait_timeout_ms": spec.wait_timeout_ms,
        "wait_interval_ms": spec.wait_interval_ms,
        "upload_timeout_ms": spec.upload_timeout_ms,
        "attachment_stall_timeout_ms": spec.attachment_stall_timeout_ms,
        "send_timeout_ms": spec.send_timeout_ms,
        "close_tab_on_complete": spec.close_tab_on_complete,
    })
}

#[derive(Clone, Debug)]
struct ExtensionPaths {
    state_dir: PathBuf,
    instances_dir: PathBuf,
    manifest_path: PathBuf,
    wrapper_path: PathBuf,
    socket_path: PathBuf,
    token_path: PathBuf,
    status_path: PathBuf,
}

#[derive(Clone, Debug)]
struct BundleInfo {
    path: PathBuf,
    file_name: String,
    size: u64,
    mime: String,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ExtensionInstanceSelector<'a> {
    pub profile_email: Option<&'a str>,
    pub extension_instance_id: Option<&'a str>,
    pub extension_profile_id: Option<&'a str>,
}

fn extension_paths() -> Result<ExtensionPaths> {
    let state_dir = yoetz_state_dir()?.join("chrome-extension-native");
    let manifest_dir = chrome_native_messaging_manifest_dir()?;
    let socket_path = env::var("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|_| default_socket_path(&state_dir));
    Ok(ExtensionPaths {
        manifest_path: manifest_dir.join(format!("{NATIVE_HOST_NAME}.json")),
        wrapper_path: state_dir.join(WRAPPER_FILENAME),
        socket_path,
        token_path: state_dir.join(TOKEN_FILENAME),
        status_path: state_dir.join(STATUS_FILENAME),
        instances_dir: state_dir.join(INSTANCES_DIRNAME),
        state_dir,
    })
}

fn yoetz_state_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("YOETZ_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(home) = home_dir() {
        return Ok(home.join(".yoetz"));
    }
    Ok(PathBuf::from(".yoetz"))
}

fn chrome_native_messaging_manifest_dir() -> Result<PathBuf> {
    if let Ok(dir) = env::var("YOETZ_CHROME_NATIVE_MESSAGING_DIR") {
        return Ok(PathBuf::from(dir));
    }
    #[cfg(unix)]
    let home = home_dir().context("could not resolve home directory")?;
    #[cfg(target_os = "macos")]
    {
        Ok(home
            .join("Library")
            .join("Application Support")
            .join("Google")
            .join("Chrome")
            .join("NativeMessagingHosts"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Ok(home
            .join(".config")
            .join("google-chrome")
            .join("NativeMessagingHosts"))
    }
    #[cfg(not(unix))]
    {
        bail!("Chrome native messaging manifest install is currently supported on macOS/Linux only")
    }
}

fn read_status_file(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn extension_status_string(
    extension_value: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<String> {
    extension_value
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn legacy_extension_status_string(
    legacy_hello_seen: bool,
    extension_value: Option<&serde_json::Map<String, Value>>,
    key: &str,
) -> Option<String> {
    if legacy_hello_seen {
        extension_status_string(extension_value, key)
    } else {
        None
    }
}

fn legacy_extension_recipes(
    extension_value: Option<&serde_json::Map<String, Value>>,
) -> Vec<String> {
    let Some(value) = extension_value.and_then(|value| value.get("recipes")) else {
        return default_extension_recipes();
    };
    let Some(values) = value.as_array() else {
        return Vec::new();
    };
    values
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect()
}

fn status_file_has_extension_hello(
    extension_value: Option<&serde_json::Map<String, Value>>,
) -> bool {
    extension_status_string(extension_value, "extension_instance_id").is_some()
        || extension_status_string(extension_value, "extension_version").is_some()
        || extension_status_string(extension_value, "extension_id").is_some()
}

fn identity_permission_doctor_check(
    latest_instance_with_hello: Option<&ExtensionInstanceStatus>,
    legacy_hello_seen: bool,
    extension_value: Option<&serde_json::Map<String, Value>>,
) -> DoctorCheck {
    let granted_from_instance = latest_instance_with_hello.map(|instance| {
        let has_email = instance
            .profile_email
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        let has_id = instance
            .profile_id
            .as_deref()
            .is_some_and(|value| !value.is_empty());
        has_email || has_id
    });
    let granted_from_legacy = if legacy_hello_seen {
        extension_value.map(|value| {
            let has_email = value
                .get("profile_email")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            let has_id = value
                .get("profile_id")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty());
            has_email || has_id
        })
    } else {
        None
    };
    let granted = granted_from_instance.or(granted_from_legacy);
    let detail = match granted {
        Some(true) => {
            "identity.email optional permission granted; profile_email/profile_id available as opt-in routing verifiers"
                .to_string()
        }
        Some(false) => {
            "identity.email optional permission not granted; routing relies on extension_instance_id only. Run `yoetz browser extension grant-identity --chatgpt` to opt in to profile_email verification."
                .to_string()
        }
        None => {
            "no extension hello observed yet; identity.email permission status unknown".to_string()
        }
    };
    // identity.email is an optional permission — its absence is expected and never fails the doctor.
    DoctorCheck {
        name: "identity_permission_granted",
        ok: true,
        detail,
    }
}

fn instance_has_extension_hello(instance: &ExtensionInstanceStatus) -> bool {
    instance
        .extension_instance_id
        .as_deref()
        .is_some_and(|value| !value.is_empty())
        || instance
            .extension_version
            .as_deref()
            .is_some_and(|value| !value.is_empty())
}

fn connected_extension_instances(paths: &ExtensionPaths) -> Vec<ExtensionInstanceStatus> {
    let Ok(entries) = fs::read_dir(&paths.instances_dir) else {
        return Vec::new();
    };
    let mut instances = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let text = fs::read_to_string(path).ok()?;
            serde_json::from_str::<ExtensionInstanceStatus>(&text).ok()
        })
        .filter(|instance| instance.protocol_version == PROTOCOL_VERSION)
        .filter(|instance| process_alive(instance.pid))
        .filter(|instance| socket_reachable(&instance.socket_path))
        .collect::<Vec<_>>();
    instances.sort_by(|a, b| a.native_instance_id.cmp(&b.native_instance_id));
    instances
}

fn prune_stale_instance_records_at(paths: &ExtensionPaths) -> usize {
    let Ok(entries) = fs::read_dir(&paths.instances_dir) else {
        return 0;
    };
    let mut pruned = 0;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let stale = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<ExtensionInstanceStatus>(&text).ok())
            .is_none_or(|instance| {
                instance.protocol_version != PROTOCOL_VERSION
                    || !process_alive(instance.pid)
                    || !socket_reachable(&instance.socket_path)
            });
        if stale && fs::remove_file(&path).is_ok() {
            pruned += 1;
        }
    }
    pruned
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    matches!(
        io::Error::last_os_error().raw_os_error(),
        Some(code) if code == libc::EPERM
    )
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    true
}

fn select_extension_instance(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
) -> Result<ExtensionInstanceStatus> {
    let instances = connected_extension_instances(paths);
    let requested_email = non_empty_selector(selector.profile_email);
    let requested_extension_instance_id = non_empty_selector(selector.extension_instance_id);
    let requested_extension_profile_id = non_empty_selector(selector.extension_profile_id);

    if requested_email.is_some()
        || requested_extension_instance_id.is_some()
        || requested_extension_profile_id.is_some()
    {
        let matches = instances
            .iter()
            .filter(|instance| {
                selector_matches_instance(
                    instance,
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        return match matches.len() {
            1 => Ok(matches[0].clone()),
            0 => {
                let observed = observed_extension_profiles(&instances);
                let selector = describe_instance_selector(
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                );
                bail!(
                    "chrome-extension-native cannot route requested {selector}; no connected extension instance matched. Connected instances: {observed}"
                )
            }
            _ => bail!(
                "chrome-extension-native found multiple connected extension instances for {}; reload duplicate extension profiles and retry",
                describe_instance_selector(
                    requested_email,
                    requested_extension_instance_id,
                    requested_extension_profile_id,
                )
            ),
        };
    }

    match instances.len() {
        1 => Ok(instances[0].clone()),
        0 => connect_legacy_socket_instance(paths),
        _ => bail!(
            "chrome-extension-native found multiple connected extension profiles; pass --profile-email=<chrome-profile-email> or --extension-instance-id=<id> (recipe callers can use --var profile_email=<chrome-profile-email> or --var extension_instance_id=<id>) so Yoetz can route the job safely. Connected instances: {}",
            observed_extension_profiles(&instances)
        ),
    }
}

fn ensure_instance_supports_recipe(instance: &ExtensionInstanceStatus, recipe: &str) -> Result<()> {
    if instance.recipes.iter().any(|value| value == recipe) {
        return Ok(());
    }
    bail!(
        "selected chrome-extension-native instance {} does not advertise recipe `{recipe}`; refusing before job_start. Run `yoetz browser extension update --{recipe}` and reload the selected Chrome profile",
        instance
            .extension_instance_id
            .as_deref()
            .unwrap_or(instance.native_instance_id.as_str())
    )
}

fn auto_heal_extension_version_skew(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
) -> Result<Option<ExtensionInstanceStatus>> {
    if chatgpt_extension_source_dir().is_none() {
        return Ok(None);
    }
    let _lifecycle_lock = acquire_extension_lifecycle_exclusive("auto-heal native extension")?;
    let previous_instance = select_extension_instance(paths, selector)?;
    if extension_version_skew_message(previous_instance.extension_version.as_deref(), None)
        .is_none()
    {
        return Ok(None);
    }
    let update = prepare_managed_chatgpt_extension_unlocked()?;
    let expected_version = update
        .manifest_version
        .as_deref()
        .context("managed extension copy has no stamped manifest version")?;
    ensure_reload_can_reach_managed_copy(&previous_instance, &update)?;
    reload_extension_unlocked(selector)?;
    wait_for_extension_version(paths, selector, expected_version).map(Some)
}

fn ensure_reload_can_reach_managed_copy(
    instance: &ExtensionInstanceStatus,
    update: &ManagedExtensionUpdateResult,
) -> Result<()> {
    let expected_version = update
        .manifest_version
        .as_deref()
        .context("managed extension copy has no stamped manifest version")?;
    let observed_parts = instance
        .extension_version
        .as_deref()
        .and_then(chrome_extension_version_parts);
    let expected_parts = chrome_extension_version_parts(expected_version);
    if observed_parts
        .as_ref()
        .is_some_and(|parts| parts.len() == 3)
        && expected_parts
            .as_ref()
            .is_some_and(|parts| parts.len() == 4)
    {
        // A 3-part live version that exactly matches the managed path before
        // this restamp is the clobbered-managed case: reload still reaches the
        // same path, so do not misclassify it as a separately loaded source copy.
        let restamped_loaded_managed_copy = update.status == "restamped"
            && instance.extension_version.as_deref() == update.previous_manifest_version.as_deref();
        if restamped_loaded_managed_copy {
            return Ok(());
        }
        if let Some(message) = managed_extension_identity_message(
            instance.extension_version.as_deref(),
            Some(expected_version),
            &update.extension_dir,
        ) {
            bail!(message);
        }
    }
    Ok(())
}

fn ensure_instance_matches_managed_copy(instance: &ExtensionInstanceStatus) -> Result<()> {
    let managed_dir = managed_chatgpt_extension_dir()?;
    let expected_version = extension_manifest_version(&managed_dir);
    if let Some(message) = managed_extension_identity_message(
        instance.extension_version.as_deref(),
        expected_version.as_deref(),
        &managed_dir,
    ) {
        bail!(message);
    }
    Ok(())
}

fn wait_for_extension_version(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
    expected_version: &str,
) -> Result<ExtensionInstanceStatus> {
    let deadline = Instant::now() + EXTENSION_RELOAD_VERIFY_TIMEOUT;
    loop {
        let current_state = match select_extension_instance(paths, selector) {
            Ok(instance)
                if instance.extension_version.as_deref() == Some(expected_version)
                    && instance_has_extension_hello(&instance) =>
            {
                return Ok(instance)
            }
            Ok(instance) => format!(
                "observed extension version {}",
                instance.extension_version.as_deref().unwrap_or("<unknown>")
            ),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for chrome-extension-native extension version {expected_version}; last state: {current_state}"
            );
        }
        thread::sleep(EXTENSION_RELOAD_VERIFY_INTERVAL);
    }
}

fn wait_for_extension_update(
    paths: &ExtensionPaths,
    selector: ExtensionInstanceSelector<'_>,
    expected_version: &str,
    expected_recipe: &str,
    previous_native_instance_id: &str,
) -> Result<ExtensionInstanceStatus> {
    let deadline = Instant::now() + EXTENSION_RELOAD_VERIFY_TIMEOUT;
    loop {
        let current_state = match select_extension_instance(paths, selector) {
            Ok(instance)
                if extension_update_is_active(
                    &instance,
                    expected_version,
                    expected_recipe,
                    previous_native_instance_id,
                ) =>
            {
                return Ok(instance)
            }
            Ok(instance) => format!(
                "native_instance_id={}, extension_version={}, recipes={:?}",
                instance.native_instance_id,
                instance.extension_version.as_deref().unwrap_or("<unknown>"),
                instance.recipes
            ),
            Err(err) => err.to_string(),
        };
        if Instant::now() >= deadline {
            bail!(
                "timed out waiting for a new chrome-extension-native instance to advertise recipe `{expected_recipe}` at extension version {expected_version}; last state: {current_state}. Chrome may retain a same-version unpacked module cache: open chrome://extensions, click Reload for `Yoetz Native Transport`, then run `yoetz browser extension status --{expected_recipe}`"
            );
        }
        thread::sleep(EXTENSION_RELOAD_VERIFY_INTERVAL);
    }
}

fn extension_update_is_active(
    instance: &ExtensionInstanceStatus,
    expected_version: &str,
    expected_recipe: &str,
    previous_native_instance_id: &str,
) -> bool {
    instance.native_instance_id != previous_native_instance_id
        && instance.extension_version.as_deref() == Some(expected_version)
        && instance_has_extension_hello(instance)
        && instance
            .recipes
            .iter()
            .any(|recipe| recipe == expected_recipe)
}

fn non_empty_selector(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn selector_matches_instance(
    instance: &ExtensionInstanceStatus,
    requested_email: Option<&str>,
    requested_extension_instance_id: Option<&str>,
    requested_extension_profile_id: Option<&str>,
) -> bool {
    requested_email.is_none_or(|email| {
        instance
            .profile_email
            .as_deref()
            .is_some_and(|actual| actual.eq_ignore_ascii_case(email))
    }) && requested_extension_instance_id.is_none_or(|id| {
        instance
            .extension_instance_id
            .as_deref()
            .is_some_and(|actual| actual == id)
    }) && requested_extension_profile_id.is_none_or(|id| {
        instance
            .profile_id
            .as_deref()
            .is_some_and(|actual| actual == id)
    })
}

fn describe_instance_selector(
    profile_email: Option<&str>,
    extension_instance_id: Option<&str>,
    extension_profile_id: Option<&str>,
) -> String {
    let mut parts = Vec::new();
    if let Some(value) = profile_email {
        parts.push(format!("profile_email {value}"));
    }
    if let Some(value) = extension_instance_id {
        parts.push(format!("extension_instance_id {value}"));
    }
    if let Some(value) = extension_profile_id {
        parts.push(format!("extension_profile_id {value}"));
    }
    parts.join(" + ")
}

fn connect_legacy_socket_instance(paths: &ExtensionPaths) -> Result<ExtensionInstanceStatus> {
    if !socket_reachable(&paths.socket_path) {
        bail!(
            "chrome-extension-native bridge is not connected at {}. Run `yoetz browser extension doctor --chatgpt`, then open Chrome with the Yoetz extension enabled.",
            paths.socket_path.display()
        );
    }
    Ok(ExtensionInstanceStatus {
        native_instance_id: "legacy".to_string(),
        socket_path: paths.socket_path.clone(),
        pid: 0,
        extension_instance_id: None,
        extension_version: None,
        profile_email: None,
        profile_id: None,
        recipes: default_extension_recipes(),
        protocol_version: PROTOCOL_VERSION,
        last_seen_ms: 0,
    })
}

fn observed_extension_profiles(instances: &[ExtensionInstanceStatus]) -> String {
    if instances.is_empty() {
        return "none".to_string();
    }
    instances
        .iter()
        .map(|instance| {
            let email = instance.profile_email.as_deref().unwrap_or("<unknown>");
            let profile_id = instance.profile_id.as_deref().unwrap_or("<unknown>");
            let extension_instance_id = instance
                .extension_instance_id
                .as_deref()
                .unwrap_or("<no-extension-instance-id>");
            format!(
                "{}:{} (email={email}, profile_id={profile_id})",
                instance.native_instance_id, extension_instance_id
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_status_file(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_string_pretty(value)? + "\n")
        .with_context(|| format!("write native host status {}", path.display()))
}

fn merge_status_file(path: &Path, patch: Value) -> Result<()> {
    let mut value = read_status_file(path).unwrap_or_else(|| json!({}));
    let target = value
        .as_object_mut()
        .context("status file must be a JSON object")?;
    let patch = patch
        .as_object()
        .context("status patch must be a JSON object")?;
    for (key, value) in patch {
        target.insert(key.clone(), value.clone());
    }
    write_status_file(path, &value)
}

fn native_host_manifest(wrapper_path: &Path) -> Result<Value> {
    Ok(json!({
        "name": NATIVE_HOST_NAME,
        "description": "Yoetz ChatGPT native bridge",
        "path": wrapper_path.canonicalize().unwrap_or_else(|_| wrapper_path.to_path_buf()),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{EXTENSION_ID}/")],
    }))
}

fn default_socket_path(state_dir: &Path) -> PathBuf {
    let state_socket = state_dir.join(SOCKET_FILENAME);
    #[cfg(unix)]
    {
        if unix_socket_path_fits(&state_socket) {
            state_socket
        } else {
            short_socket_path(state_dir)
        }
    }
    #[cfg(not(unix))]
    {
        state_socket
    }
}

#[cfg(unix)]
fn unix_socket_path_fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().len() < MAX_UNIX_SOCKET_PATH_BYTES
}

#[cfg(unix)]
fn short_socket_path(state_dir: &Path) -> PathBuf {
    let digest = socket_fallback_digest(state_dir);
    socket_fallback_dir(state_dir).join(format!("{}.sock", &digest[..16]))
}

#[cfg(unix)]
fn socket_fallback_dir(state_dir: &Path) -> PathBuf {
    let digest = socket_fallback_digest(state_dir);
    env::temp_dir().join(format!("yoetz-cen-{}", &digest[..8]))
}

#[cfg(unix)]
fn socket_fallback_digest(state_dir: &Path) -> String {
    let mut hash = Sha256::new();
    hash.update(state_dir.to_string_lossy().as_bytes());
    hex::encode(hash.finalize())
}

#[cfg(unix)]
fn write_wrapper(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let exe = env::current_exe().context("resolve current yoetz executable")?;
    let mut script = "#!/bin/sh\n".to_string();
    for key in ["YOETZ_DIR", "YOETZ_CHROME_EXTENSION_NATIVE_SOCKET"] {
        if let Ok(value) = env::var(key) {
            script.push_str(&format!("export {key}={}\n", shell_quote(&value)));
        }
    }
    script.push_str(&format!(
        "exec {} browser chrome-native-host --chatgpt\n",
        shell_quote(&exe.to_string_lossy())
    ));
    fs::write(path, script).with_context(|| format!("write wrapper {}", path.display()))?;
    set_private_file_mode(path, 0o700)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_file_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn ensure_private_dir(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect directory {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!(
            "private directory must not be a symlink: {}",
            path.display()
        );
    }
    if !metadata.is_dir() {
        bail!("private directory must be a directory: {}", path.display());
    }
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        bail!(
            "private directory {} is owned by uid {}, current uid is {}",
            path.display(),
            metadata.uid(),
            current_uid
        );
    }
    set_private_file_mode(path, 0o700)?;
    let mode = fs::symlink_metadata(path)?.permissions().mode() & 0o777;
    if mode != 0o700 {
        bail!(
            "private directory {} mode is {:o}, expected 700",
            path.display(),
            mode
        );
    }
    Ok(())
}

fn ensure_capability_token(path: &Path) -> Result<String> {
    if path.exists() {
        return read_capability_token(path);
    }
    if let Some(parent) = path.parent() {
        #[cfg(unix)]
        ensure_private_dir(parent)?;
        #[cfg(not(unix))]
        fs::create_dir_all(parent)?;
    }
    let token = hex::encode(rand::random::<[u8; 32]>());
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("create capability token {}", path.display()))?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    #[cfg(unix)]
    set_private_file_mode(path, 0o600)?;
    Ok(token)
}

fn read_capability_token(path: &Path) -> Result<String> {
    validate_private_token_file(path)?;
    let token = fs::read_to_string(path)
        .with_context(|| format!("read capability token {}", path.display()))?
        .trim()
        .to_string();
    if token.len() < 32 {
        bail!("capability token at {} is invalid", path.display());
    }
    Ok(token)
}

fn wrapper_target_doctor_check(path: &Path) -> DoctorCheck {
    let current_exe = match env::current_exe() {
        Ok(path) => path,
        Err(err) => {
            return DoctorCheck {
                name: "wrapper_target",
                ok: false,
                detail: format!("could not resolve current yoetz executable: {err}"),
            };
        }
    };
    let expected_line = format!(
        "exec {} browser chrome-native-host --chatgpt",
        shell_quote(&current_exe.to_string_lossy())
    );
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            return DoctorCheck {
                name: "wrapper_target",
                ok: false,
                detail: format!("{}: {err}", path.display()),
            };
        }
    };
    if text.lines().any(|line| line == expected_line) {
        return DoctorCheck {
            name: "wrapper_target",
            ok: true,
            detail: current_exe.display().to_string(),
        };
    }
    let actual = text
        .lines()
        .find(|line| line.starts_with("exec "))
        .unwrap_or("<missing exec line>");
    DoctorCheck {
        name: "wrapper_target",
        ok: false,
        detail: format!(
            "wrapper targets `{actual}`; rerun `yoetz browser extension install-host --chatgpt` with {}",
            current_exe.display()
        ),
    }
}

fn token_doctor_check(path: &Path) -> DoctorCheck {
    match validate_private_token_file(path) {
        Ok(()) => DoctorCheck {
            name: "capability_token",
            ok: true,
            detail: path.display().to_string(),
        },
        Err(err) => DoctorCheck {
            name: "capability_token",
            ok: false,
            detail: format!("{}: {err}", path.display()),
        },
    }
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn validate_private_token_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("inspect capability token {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("capability token must not be a symlink");
    }
    if !metadata.is_file() {
        bail!("capability token must be a regular file");
    }
    let current_uid = unsafe { libc::geteuid() };
    if metadata.uid() != current_uid {
        bail!(
            "capability token is owned by uid {}, current uid is {}",
            metadata.uid(),
            current_uid
        );
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        bail!(
            "capability token permissions are {mode:03o}; run `chmod 600 {}`",
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_token_file(path: &Path) -> Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        bail!("capability token must be a regular file")
    }
}

fn validate_bundle_path(path: &Path) -> Result<BundleInfo> {
    let canonical_path =
        fs::canonicalize(path).with_context(|| format!("resolve bundle {}", path.display()))?;
    let metadata = fs::metadata(&canonical_path)
        .with_context(|| format!("read bundle {}", canonical_path.display()))?;
    if !metadata.is_file() {
        bail!("bundle path is not a file: {}", canonical_path.display());
    }
    if metadata.len() > MAX_BUNDLE_BYTES {
        bail!(
            "bundle is {} bytes, above chrome-extension-native limit of {} bytes",
            metadata.len(),
            MAX_BUNDLE_BYTES
        );
    }
    let file_name = canonical_path
        .file_name()
        .and_then(|name| name.to_str())
        .context("bundle path must end in a UTF-8 filename")?
        .to_string();
    let mime = if file_name.ends_with(".md") || file_name.ends_with(".markdown") {
        "text/markdown"
    } else {
        "text/plain"
    }
    .to_string();
    Ok(BundleInfo {
        path: canonical_path,
        file_name,
        size: metadata.len(),
        mime,
    })
}

fn connect_socket(path: &Path) -> io::Result<SocketStream> {
    #[cfg(unix)]
    {
        use std::os::unix::net::UnixStream;
        Ok(SocketStream::Unix(UnixStream::connect(path)?))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "chrome-extension-native socket is only supported on macOS/Linux",
        ))
    }
}

fn socket_reachable(path: &Path) -> bool {
    connect_socket(path).is_ok()
}

fn send_control_job(
    kind: &str,
    payload: Value,
    selector: ExtensionInstanceSelector<'_>,
) -> Result<ProtocolEnvelope> {
    send_control_job_with_recipe(kind, payload, selector, None)
}

fn send_site_control_job(
    kind: &str,
    payload: Value,
    selector: ExtensionInstanceSelector<'_>,
    recipe: BuiltinWebRecipe,
) -> Result<ProtocolEnvelope> {
    send_control_job_with_recipe(kind, payload, selector, Some(recipe.as_str()))
}

fn send_control_job_with_recipe(
    kind: &str,
    payload: Value,
    selector: ExtensionInstanceSelector<'_>,
    required_recipe: Option<&str>,
) -> Result<ProtocolEnvelope> {
    let paths = extension_paths()?;
    let instance = select_extension_instance(&paths, selector)?;
    if let Some(recipe) = required_recipe {
        ensure_instance_supports_recipe(&instance, recipe)?;
    }
    let token = read_capability_token(&paths.token_path)?;
    let job_id = new_id(kind);
    let mut stream = connect_socket(&instance.socket_path).with_context(|| {
        format!(
            "chrome-extension-native bridge is not connected at {}",
            instance.socket_path.display()
        )
    })?;
    stream.set_read_timeout(Some(CONTROL_READ_TIMEOUT))?;
    let envelope = ProtocolEnvelope::new(kind, Some(job_id), None, payload).with_token(token);
    write_json_frame(&mut stream, &envelope)?;
    loop {
        let response = read_json_frame(&mut stream)
            .with_context(|| format!("timed out waiting for chrome-extension-native `{kind}`"))?;
        validate_inbound_envelope(&response)?;
        match response.kind.as_str() {
            "job_progress" | "heartbeat" => continue,
            "job_complete" => return Ok(response),
            "job_error" => return Err(job_error(response)),
            _ => return Ok(response),
        }
    }
}

pub fn chunk_payloads_for_file(
    path: &Path,
    file_name: &str,
    mime_type: &str,
) -> Result<Vec<Value>> {
    let bytes = fs::read(path).with_context(|| format!("read bundle {}", path.display()))?;
    let total_bytes = bytes.len();
    let total_chunks = total_bytes.div_ceil(CHUNK_BYTES).max(1);
    let mut chunks = Vec::with_capacity(total_chunks);
    for sequence in 0..total_chunks {
        let start = sequence * CHUNK_BYTES;
        let end = (start + CHUNK_BYTES).min(total_bytes);
        let chunk = &bytes[start..end];
        chunks.push(json!({
            "sequence": sequence,
            "total_chunks": total_chunks,
            "total_bytes": total_bytes,
            "filename": file_name,
            "mime_type": mime_type,
            "bytes_base64": base64::engine::general_purpose::STANDARD.encode(chunk),
        }));
    }
    Ok(chunks)
}

fn validate_inbound_envelope(envelope: &ProtocolEnvelope) -> Result<()> {
    if envelope.protocol_version != PROTOCOL_VERSION {
        bail!(
            "chrome-extension-native protocol version mismatch: got {}, expected {}",
            envelope.protocol_version,
            PROTOCOL_VERSION
        );
    }
    if envelope.transport != TRANSPORT_NAME {
        bail!("unexpected transport `{}`", envelope.transport);
    }
    match envelope.kind.as_str() {
        "hello"
        | "heartbeat"
        | "job_start"
        | "job_progress"
        | "job_file_chunk"
        | "job_file_chunk_ack"
        | "job_complete"
        | "job_error"
        | "job_cancel"
        | "pair_request"
        | "pair_complete"
        | "reconnect"
        | "inspect_run"
        | "request_identity_permission" => {}
        other => bail!("unsupported chrome-extension-native envelope type `{other}`"),
    }
    Ok(())
}

fn parse_recipe_result(envelope: ProtocolEnvelope) -> Result<ExtensionRecipeResult> {
    let response = envelope
        .payload
        .get("response")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let model_used = envelope
        .payload
        .get("model_used")
        .and_then(Value::as_str)
        .map(str::to_string);
    let model_selection_status = parse_model_selection_status(
        envelope
            .payload
            .get("model_selection_status")
            .and_then(Value::as_str),
    );
    let warnings = envelope
        .payload
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let warning_details = envelope
        .payload
        .get("warnings")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.is_object())
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let conversation_id = envelope
        .payload
        .get("conversation_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let conversation_url = envelope
        .payload
        .get("conversation_url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let diagnostics = ChatgptRecipeDiagnostics {
        extraction_method: envelope
            .payload
            .get("extraction_method")
            .and_then(Value::as_str)
            .map(str::to_string),
        completion_reason: envelope
            .payload
            .get("completion_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        finality_anchor: envelope
            .payload
            .get("finality_anchor")
            .and_then(Value::as_str)
            .map(str::to_string),
        stable_for_ms: envelope
            .payload
            .get("stable_for_ms")
            .and_then(Value::as_u64),
        assistant_turn_count: envelope
            .payload
            .get("assistant_turn_count")
            .and_then(Value::as_u64),
        copy_button_count: envelope
            .payload
            .get("copy_button_count")
            .and_then(Value::as_u64),
    };
    Ok(ExtensionRecipeResult {
        response,
        model_used,
        model_selection_status,
        warnings,
        warning_details,
        conversation_id,
        conversation_url,
        diagnostics,
    })
}

fn validate_canary_response(response: &str) -> Result<()> {
    let trimmed = response.trim();
    if trimmed == "OK" {
        return Ok(());
    }
    bail!(
        "chrome-extension-native live canary expected exact response `OK`, got `{}`",
        trimmed
    )
}

fn parse_model_selection_status(value: Option<&str>) -> ChatgptModelSelectionStatus {
    match value.unwrap_or("unavailable") {
        "selected" => ChatgptModelSelectionStatus::Selected,
        "kept_current" => ChatgptModelSelectionStatus::KeptCurrent,
        "current" => ChatgptModelSelectionStatus::Current,
        "mismatch" => ChatgptModelSelectionStatus::Mismatch,
        _ => ChatgptModelSelectionStatus::Unavailable,
    }
}

fn job_error(envelope: ProtocolEnvelope) -> anyhow::Error {
    job_error_for_recipe(envelope, BuiltinWebRecipe::Chatgpt)
}

fn job_error_for_recipe(envelope: ProtocolEnvelope, recipe: BuiltinWebRecipe) -> anyhow::Error {
    let message = job_error_message(&envelope.payload);
    let is_conversation_error = envelope
        .payload
        .get("code")
        .and_then(Value::as_str)
        .is_some_and(|code| code.starts_with("conversation_"));
    let phase = envelope.payload.get("phase").and_then(Value::as_str);
    let side_effect_started = envelope
        .payload
        .get("side_effect_started")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let err = if is_conversation_error {
        anyhow::Error::new(ConversationJobError { message })
    } else {
        anyhow!("{message}")
    };
    if !side_effect_started {
        return err;
    }
    let phase = match phase {
        Some("send") => ChatgptTransportPhase::Send,
        Some("wait_response") => ChatgptTransportPhase::WaitResponse,
        _ => ChatgptTransportPhase::Upload,
    };
    crate::web_recipe::mark_terminal_fallback_phase(err, recipe, phase)
}

fn job_error_message(payload: &Value) -> String {
    let mut message = payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("chrome-extension-native job failed")
        .to_string();
    let code = payload.get("code").and_then(Value::as_str).unwrap_or("");
    let mut detail = Vec::new();
    if !code.starts_with("conversation_") {
        append_job_error_detail(payload, &message, &mut detail);
        if !detail.is_empty() {
            message.push_str(". ");
            message.push_str(&detail.join("; "));
        }
        return message;
    }

    if let Some(requested) = payload
        .get("requested_conversation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if !message.contains(requested) {
            detail.push(format!("requested conversation {requested}"));
        }
    }
    if let Some(current_url) = payload
        .get("current_url")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if !message.contains(current_url) {
            detail.push(format!("current URL {current_url}"));
        }
    }
    if let Some(phase) = payload
        .get("phase")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let phase_text = format!("phase {phase}");
        if !message.contains(&phase_text) {
            detail.push(phase_text);
        }
    }
    append_job_error_detail(payload, &message, &mut detail);

    if !detail.is_empty() {
        message.push_str(". ");
        message.push_str(&detail.join("; "));
    }
    message
}

fn append_job_error_detail(payload: &Value, message: &str, detail: &mut Vec<String>) {
    if let Some(reason) = payload
        .get("failure_reason")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let reason_text = format!("failure_reason={reason}");
        if !message.contains(reason) {
            detail.push(reason_text);
        }
    }
    if let Some(tab_id) = payload
        .get("tab_id")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
    {
        let tab_text = format!("tab {tab_id}");
        if !message.contains(&tab_text) {
            detail.push(tab_text);
        }
    }
    if let Some(phase) = payload
        .get("phase")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        let phase_text = format!("phase {phase}");
        if !message.contains(&phase_text) && !detail.iter().any(|item| item == &phase_text) {
            detail.push(phase_text);
        }
    }
    if let Some(inspect_command) = payload
        .get("inspect_command")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        if !message.contains(inspect_command) {
            detail.push(format!("inspect with: {inspect_command}"));
        }
    }
    if let Some(trace) = payload.get("attachment_trace").and_then(Value::as_object) {
        if let Ok(trace) = serde_json::to_string(trace) {
            if trace.len() <= 4096 {
                detail.push(format!("attachment_trace={trace}"));
            } else {
                detail.push(format!("attachment_trace=<omitted: {} bytes>", trace.len()));
            }
        }
    }
}

fn emit_progress(format: OutputFormat, envelope: &ProtocolEnvelope) -> Result<()> {
    match format {
        OutputFormat::Jsonl => write_jsonl("browser.recipe", envelope),
        OutputFormat::Text | OutputFormat::Markdown => {
            if let Some(message) = envelope.payload.get("message").and_then(Value::as_str) {
                eprintln!("chrome-extension-native: {message}");
            }
            Ok(())
        }
        OutputFormat::Json => {
            if let Some(message) = envelope.payload.get("message").and_then(Value::as_str) {
                eprintln!("chrome-extension-native: {message}");
            }
            Ok(())
        }
    }
}

pub fn write_json_frame<W: Write>(writer: &mut W, value: &ProtocolEnvelope) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    write_frame(writer, &bytes)
}

pub fn read_json_frame<R: Read>(reader: &mut R) -> Result<ProtocolEnvelope> {
    let bytes = read_frame(reader)?;
    let envelope = serde_json::from_slice(&bytes)?;
    Ok(envelope)
}

pub fn write_frame<W: Write>(writer: &mut W, bytes: &[u8]) -> Result<()> {
    write_frame_with_limit(writer, bytes, MAX_FRAME_BYTES)
}

fn write_frame_with_limit<W: Write>(writer: &mut W, bytes: &[u8], max: usize) -> Result<()> {
    if bytes.len() > max {
        return Err(FrameTooLargeError {
            len: bytes.len(),
            max,
        }
        .into());
    }
    let len = u32::try_from(bytes.len()).context("frame length exceeds u32")?;
    writer.write_all(&len.to_ne_bytes())?;
    writer.write_all(bytes)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>> {
    let mut len_buf = [0_u8; 4];
    reader.read_exact(&mut len_buf)?;
    let len = u32::from_ne_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        let mut frame_body = reader.take(len as u64);
        io::copy(&mut frame_body, &mut io::sink())?;
        return Err(FrameTooLargeError {
            len,
            max: MAX_FRAME_BYTES,
        }
        .into());
    }
    let mut bytes = vec![0_u8; len];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn new_id(prefix: &str) -> String {
    let random = rand::random::<[u8; 8]>();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}_{now:x}_{}", hex::encode(random))
}

fn workspace_id() -> Result<String> {
    let cwd = env::current_dir()?;
    let mut hash = Sha256::new();
    hash.update(cwd.to_string_lossy().as_bytes());
    Ok(format!("workspace_{}", &hex::encode(hash.finalize())[..16]))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn extension_id_from_public_key(public_key_b64: &str) -> Result<String> {
    let bytes = base64::engine::general_purpose::STANDARD.decode(public_key_b64)?;
    let digest = Sha256::digest(bytes);
    let mut id = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        id.push((b'a' + (byte >> 4)) as char);
        id.push((b'a' + (byte & 0x0f)) as char);
    }
    Ok(id)
}

enum SocketStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(not(unix))]
    Unsupported,
}

impl SocketStream {
    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(not(unix))]
        let _ = timeout;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.set_read_timeout(timeout),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

impl Read for SocketStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        #[cfg(not(unix))]
        let _ = buf;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.read(buf),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

impl Write for SocketStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        #[cfg(not(unix))]
        let _ = buf;
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.write(buf),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            SocketStream::Unix(stream) => stream.flush(),
            #[cfg(not(unix))]
            SocketStream::Unsupported => Err(unsupported_socket_error()),
        }
    }
}

#[cfg(not(unix))]
fn unsupported_socket_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "chrome-extension-native socket is only supported on macOS/Linux",
    )
}

#[cfg(unix)]
mod native_host_unix {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;

    type Clients = Arc<Mutex<HashMap<String, ClientJob>>>;

    struct ClientJob {
        stream: UnixStream,
        job_id: String,
        run_id: Option<String>,
        chunks: Vec<Value>,
        next_chunk: usize,
        side_effect_started: bool,
        fallback_phase: Option<&'static str>,
        cancel_on_disconnect: bool,
    }

    struct RouteDelivery {
        stream: UnixStream,
        job_id: String,
        run_id: Option<String>,
        fallback_phase: Option<&'static str>,
        side_effect_started: bool,
        client_error: Option<ProtocolEnvelope>,
        next_chunk: Option<ProtocolEnvelope>,
        cancel_on_write_error: Option<ProtocolEnvelope>,
    }

    struct SocketFileGuard {
        path: PathBuf,
    }

    struct InstanceFileGuard {
        path: PathBuf,
    }

    struct NativeHostRuntime {
        native_instance_id: String,
        socket_path: PathBuf,
        instance_path: PathBuf,
    }

    impl SocketFileGuard {
        fn new(path: PathBuf) -> Self {
            Self { path }
        }
    }

    impl Drop for SocketFileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    impl Drop for InstanceFileGuard {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.path);
        }
    }

    pub(super) fn serve() -> Result<()> {
        let paths = extension_paths()?;
        ensure_private_dir(&paths.state_dir)?;
        ensure_private_dir(&paths.instances_dir)?;
        let token = ensure_capability_token(&paths.token_path)?;
        let native_instance_id = new_id("native");
        let (listener, socket_path) = bind_native_host_listener(&paths, &native_instance_id)?;
        let instance_path = paths
            .instances_dir
            .join(format!("{native_instance_id}.json"));
        let runtime = NativeHostRuntime {
            native_instance_id,
            socket_path,
            instance_path,
        };
        let _socket_guard = SocketFileGuard::new(runtime.socket_path.clone());
        let _instance_guard = InstanceFileGuard {
            path: runtime.instance_path.clone(),
        };
        fs::set_permissions(&runtime.socket_path, fs::Permissions::from_mode(0o600))?;

        let status = json!({
            "bridge_state": "native_host_started",
            "protocol_version": PROTOCOL_VERSION,
            "transport": TRANSPORT_NAME,
            "native_host_name": NATIVE_HOST_NAME,
            "extension_id": EXTENSION_ID,
            "pid": process::id(),
            "native_instance_id": runtime.native_instance_id,
            "socket_path": runtime.socket_path,
            "connected_at_ms": now_millis(),
        });
        write_status_file(&paths.status_path, &status)?;
        write_instance_status(&runtime, json!({}))?;

        let stdout = Arc::new(Mutex::new(io::stdout()));
        let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
        let accept_stdout = Arc::clone(&stdout);
        let accept_clients = Arc::clone(&clients);
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let token = token.clone();
                        let stdout = Arc::clone(&accept_stdout);
                        let clients = Arc::clone(&accept_clients);
                        thread::spawn(move || {
                            if let Err(err) = handle_client(stream, &token, stdout, clients) {
                                eprintln!("yoetz chrome native client error: {err:#}");
                            }
                        });
                    }
                    Err(err) => {
                        eprintln!("yoetz chrome native accept error: {err}");
                        break;
                    }
                }
            }
        });

        let mut stdin = io::stdin();
        loop {
            match read_json_frame(&mut stdin) {
                Ok(envelope) => {
                    if let Err(err) = validate_inbound_envelope(&envelope) {
                        record_protocol_mismatch(&paths, &envelope, &err)?;
                        continue;
                    }
                    route_extension_message(envelope, &clients, &stdout, &paths, &runtime)?;
                }
                Err(err) if is_disconnect_error(&err) => {
                    notify_clients_transport_lost(&clients);
                    let _ = fs::remove_file(&runtime.instance_path);
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "bridge_state": "native_host_stopped",
                            "disconnected_at_ms": now_millis(),
                        }),
                    )?;
                    return Ok(());
                }
                Err(err) if is_recoverable_input_error(&err) => {
                    eprintln!(
                        "yoetz chrome native ignored malformed Chrome native messaging frame: {err:#}"
                    );
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "last_native_host_input_error": {
                                "message": err.to_string(),
                                "seen_at_ms": now_millis(),
                                "recoverable": true,
                            },
                        }),
                    )?;
                    continue;
                }
                Err(err) => {
                    notify_clients_transport_lost(&clients);
                    let _ = fs::remove_file(&runtime.instance_path);
                    merge_status_file(
                        &paths.status_path,
                        json!({
                            "bridge_state": "native_host_input_error",
                            "last_native_host_input_error": {
                                "message": err.to_string(),
                                "seen_at_ms": now_millis(),
                            },
                        }),
                    )?;
                    return Err(err).context("read Chrome native messaging frame");
                }
            }
        }
    }

    pub(super) fn bind_native_host_listener(
        paths: &ExtensionPaths,
        native_instance_id: &str,
    ) -> Result<(UnixListener, PathBuf)> {
        let explicit_socket = env::var("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET").is_ok();
        ensure_socket_parent_path(paths, &paths.socket_path)?;
        if explicit_socket || !active_socket_exists(&paths.socket_path) {
            remove_stale_socket(&paths.socket_path)?;
            let listener = UnixListener::bind(&paths.socket_path)
                .with_context(|| format!("bind {}", paths.socket_path.display()))?;
            return Ok((listener, paths.socket_path.clone()));
        }

        let socket_path = instance_socket_path(paths, native_instance_id);
        ensure_socket_parent_path(paths, &socket_path)?;
        remove_stale_socket(&socket_path)?;
        let listener = UnixListener::bind(&socket_path)
            .with_context(|| format!("bind {}", socket_path.display()))?;
        Ok((listener, socket_path))
    }

    fn active_socket_exists(path: &Path) -> bool {
        path.exists() && UnixStream::connect(path).is_ok()
    }

    fn instance_socket_path(paths: &ExtensionPaths, native_instance_id: &str) -> PathBuf {
        let state_socket = paths
            .instances_dir
            .join(format!("{native_instance_id}.sock"));
        if unix_socket_path_fits(&state_socket) {
            return state_socket;
        }
        socket_fallback_dir(&paths.state_dir).join(format!("{native_instance_id}.sock"))
    }

    pub(super) fn ensure_socket_parent_path(
        paths: &ExtensionPaths,
        socket_path: &Path,
    ) -> Result<()> {
        let Some(parent) = socket_path.parent() else {
            return Ok(());
        };
        if parent == paths.state_dir
            || parent == paths.instances_dir
            || parent == socket_fallback_dir(&paths.state_dir)
        {
            return ensure_private_dir(parent);
        }
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))
    }

    pub(super) fn remove_stale_socket(path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        if UnixStream::connect(path).is_ok() {
            bail!(
                "chrome-extension-native socket already has an active native host at {}",
                path.display()
            );
        }
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("read socket metadata {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            bail!(
                "refusing to remove symlink at chrome-extension-native socket path {}",
                path.display()
            );
        }
        if !metadata.file_type().is_socket() {
            bail!(
                "refusing to remove non-socket file at chrome-extension-native socket path {}",
                path.display()
            );
        }
        fs::remove_file(path).with_context(|| format!("remove stale socket {}", path.display()))
    }

    fn handle_client(
        mut stream: UnixStream,
        token: &str,
        stdout: Arc<Mutex<io::Stdout>>,
        clients: Clients,
    ) -> Result<()> {
        let envelope = match read_json_frame(&mut stream) {
            Ok(envelope) => envelope,
            Err(err) if is_disconnect_error(&err) => return Ok(()),
            Err(err) => return Err(err),
        };
        validate_inbound_envelope(&envelope)?;
        if envelope.capability_token.as_deref() != Some(token) {
            bail!("capability token mismatch");
        }
        let job_id = envelope
            .job_id
            .clone()
            .context("local client message must include job_id")?;
        let (forwarded, chunks) = match prepare_local_message(envelope.clone()) {
            Ok(prepared) => prepared,
            Err(err) if envelope.kind == "job_start" => {
                let error = client_error_envelope_from_parts(
                    &job_id,
                    envelope.run_id.clone(),
                    "bundle_validation_failed",
                    &format!("native host could not prepare job bundle: {err:#}"),
                    Some("upload"),
                    false,
                );
                write_json_frame(&mut stream, &error)
                    .context("write bundle validation error to local client")?;
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let client = ClientJob {
            stream: stream.try_clone()?,
            job_id: job_id.clone(),
            run_id: envelope.run_id.clone(),
            chunks,
            next_chunk: 0,
            side_effect_started: false,
            fallback_phase: None,
            cancel_on_disconnect: envelope.kind == "job_start",
        };
        clients.lock().unwrap().insert(job_id.clone(), client);

        let forward_result = match envelope.kind.as_str() {
            "job_start"
            | "job_cancel"
            | "pair_request"
            | "reconnect"
            | "inspect_run"
            | "request_identity_permission" => forward_to_extension(&stdout, &forwarded),
            other => Err(anyhow!("unsupported local client message `{other}`")),
        };
        if let Err(err) = forward_result {
            if let Some(mut client) = clients.lock().unwrap().remove(&job_id) {
                let error = client_error_envelope(
                    &client,
                    "forward_to_extension_failed",
                    &format!("native host could not forward job to extension: {err}"),
                );
                let _ = write_json_frame(&mut client.stream, &error);
            }
            return Err(err);
        }

        stream.set_read_timeout(Some(Duration::from_secs(1)))?;
        loop {
            match read_json_frame(&mut stream) {
                Ok(control) => {
                    validate_inbound_envelope(&control)?;
                    if control.capability_token.as_deref() != Some(token) {
                        bail!("capability token mismatch");
                    }
                    if control.job_id.as_ref() != Some(&job_id) {
                        bail!("local client control message changed job_id");
                    }
                    let (forwarded, _) = prepare_local_message(control.clone())?;
                    match control.kind.as_str() {
                        "job_cancel"
                        | "reconnect"
                        | "inspect_run"
                        | "request_identity_permission" => {
                            if let Err(err) = forward_to_extension(&stdout, &forwarded) {
                                if let Some(mut client) = clients.lock().unwrap().remove(&job_id) {
                                    let error = client_error_envelope(
                                        &client,
                                        "forward_to_extension_failed",
                                        &format!(
                                            "native host could not forward control message to extension: {err}"
                                        ),
                                    );
                                    let _ = write_json_frame(&mut client.stream, &error);
                                }
                                return Err(err);
                            }
                        }
                        other => {
                            eprintln!("yoetz chrome native ignored local control event `{other}`")
                        }
                    }
                }
                Err(err) if is_timeout_error(&err) => {
                    if !clients.lock().unwrap().contains_key(&job_id) {
                        return Ok(());
                    }
                }
                Err(err) if is_disconnect_error(&err) => break,
                Err(err) => return Err(err).context("read local client control frame"),
            }
        }
        let still_active = clients.lock().unwrap().remove(&job_id).is_some();
        if still_active && envelope.kind == "job_start" {
            let cancel = ProtocolEnvelope::new(
                "job_cancel",
                Some(job_id),
                envelope.run_id.clone(),
                json!({
                    "reason": "local_client_disconnected"
                }),
            );
            forward_to_extension(&stdout, &cancel)?;
        }
        Ok(())
    }

    fn route_extension_message(
        envelope: ProtocolEnvelope,
        clients: &Clients,
        stdout: &Arc<Mutex<io::Stdout>>,
        paths: &ExtensionPaths,
        runtime: &NativeHostRuntime,
    ) -> Result<()> {
        let Some(job_id) = envelope.job_id.clone() else {
            record_unrouted_extension_message(paths, &envelope)?;
            record_instance_activity(runtime, &envelope)?;
            return Ok(());
        };
        let mut remove_client = matches!(
            envelope.kind.as_str(),
            "job_complete" | "job_error" | "job_cancel" | "pair_complete"
        );
        let delivery = {
            let mut clients = clients.lock().unwrap();
            if let Some(client) = clients.get_mut(&job_id) {
                update_client_effect_state(client, &envelope);
                if should_replay_upload_from_start(&envelope) {
                    client.next_chunk = 0;
                }
                let stream = match client.stream.try_clone() {
                    Ok(stream) => stream,
                    Err(err) => {
                        eprintln!(
                            "yoetz chrome native local client clone failed for {job_id}: {err:#}"
                        );
                        clients.remove(&job_id);
                        return Ok(());
                    }
                };
                let mut client_error = None;
                let mut next_chunk = None;
                match should_send_next_chunk(client, &envelope) {
                    Ok(true) => {
                        next_chunk = next_bundle_chunk_envelope(client)?;
                    }
                    Ok(false) => {}
                    Err(err) => {
                        client_error = Some(client_error_envelope(
                            client,
                            "chunk_ack_mismatch",
                            &format!("invalid extension chunk acknowledgement: {err}"),
                        ));
                        remove_client = true;
                    }
                }
                let cancel_on_write_error = if client.cancel_on_disconnect && !remove_client {
                    Some(local_client_disconnected_cancel(client))
                } else {
                    None
                };
                let delivery_job_id = client.job_id.clone();
                let delivery_run_id = client.run_id.clone();
                let delivery_fallback_phase = client.fallback_phase;
                let delivery_side_effect_started = client.side_effect_started;
                if remove_client {
                    clients.remove(&job_id);
                }
                Some(RouteDelivery {
                    stream,
                    job_id: delivery_job_id,
                    run_id: delivery_run_id,
                    fallback_phase: delivery_fallback_phase,
                    side_effect_started: delivery_side_effect_started,
                    client_error,
                    next_chunk,
                    cancel_on_write_error,
                })
            } else {
                None
            }
        };

        if let Some(mut delivery) = delivery {
            if let Err(err) = write_json_frame(&mut delivery.stream, &envelope) {
                eprintln!("yoetz chrome native local client write failed for {job_id}: {err:#}");
                if let Some(cancel) = delivery.cancel_on_write_error {
                    let _ = forward_to_extension(stdout, &cancel);
                }
                clients.lock().unwrap().remove(&job_id);
            } else if let Some(error) = delivery.client_error {
                let _ = write_json_frame(&mut delivery.stream, &error);
                remove_client = true;
            } else if let Some(chunk) = delivery.next_chunk {
                if let Err(err) = forward_to_extension(stdout, &chunk) {
                    eprintln!("yoetz chrome native chunk send failed for {job_id}: {err:#}");
                    let error = client_error_envelope_from_parts(
                        &delivery.job_id,
                        delivery.run_id.clone(),
                        "forward_to_extension_failed",
                        &format!("native host could not forward file chunk to extension: {err}"),
                        delivery.fallback_phase,
                        delivery.side_effect_started,
                    );
                    let _ = write_json_frame(&mut delivery.stream, &error);
                    remove_client = true;
                }
            }

            if remove_client {
                clients.lock().unwrap().remove(&job_id);
            }
        }

        if is_manual_handoff(&envelope) {
            record_manual_handoff(paths, &envelope)?;
        }
        record_instance_activity(runtime, &envelope)?;
        Ok(())
    }

    fn next_bundle_chunk_envelope(client: &mut ClientJob) -> Result<Option<ProtocolEnvelope>> {
        let Some(payload) = client.chunks.get(client.next_chunk).cloned() else {
            return Ok(None);
        };
        let chunk = ProtocolEnvelope::new(
            "job_file_chunk",
            Some(client.job_id.clone()),
            client.run_id.clone(),
            payload,
        );
        client.next_chunk += 1;
        Ok(Some(chunk))
    }

    fn prepare_local_message(
        mut envelope: ProtocolEnvelope,
    ) -> Result<(ProtocolEnvelope, Vec<Value>)> {
        let chunks = if envelope.kind == "job_start" {
            let bundle = validate_local_job_bundle(&envelope)?;
            if let Some(payload) = envelope.payload.as_object_mut() {
                payload.remove("bundle_path");
                payload.insert("file_name".to_string(), json!(bundle.file_name.clone()));
                payload.insert("bundle_size".to_string(), json!(bundle.size));
                payload.insert("mime".to_string(), json!(bundle.mime.clone()));
            }
            chunk_payloads_for_file(&bundle.path, &bundle.file_name, &bundle.mime)?
        } else {
            Vec::new()
        };
        Ok((without_token(envelope), chunks))
    }

    fn validate_local_job_bundle(envelope: &ProtocolEnvelope) -> Result<BundleInfo> {
        let bundle_path = envelope
            .payload
            .get("bundle_path")
            .and_then(Value::as_str)
            .context("job_start payload missing bundle_path")?;
        let bundle = validate_bundle_path(Path::new(bundle_path))?;
        if let Some(size) = envelope.payload.get("bundle_size").and_then(Value::as_u64) {
            if size != bundle.size {
                bail!(
                    "job_start bundle_size {} does not match current file size {}",
                    size,
                    bundle.size
                );
            }
        }
        Ok(bundle)
    }

    fn should_send_next_chunk(client: &ClientJob, envelope: &ProtocolEnvelope) -> Result<bool> {
        if envelope.kind == "job_progress" {
            if envelope.payload.get("phase").and_then(Value::as_str) == Some("ready_for_file") {
                return Ok(client.next_chunk == 0);
            }
            return Ok(false);
        }
        if envelope.kind == "job_file_chunk_ack" {
            if envelope
                .payload
                .get("complete")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Ok(false);
            }
            let sequence = envelope
                .payload
                .get("sequence")
                .and_then(Value::as_u64)
                .context("chunk ack missing sequence")?;
            if client.next_chunk == 0 {
                bail!("chunk ack arrived before any bundle chunk was sent");
            }
            let expected = (client.next_chunk - 1) as u64;
            if sequence < expected {
                return Ok(false);
            }
            if sequence > expected {
                bail!("chunk ack sequence {sequence} is ahead of expected {expected}");
            }
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn should_replay_upload_from_start(envelope: &ProtocolEnvelope) -> bool {
        envelope.kind == "job_progress"
            && envelope
                .payload
                .get("phase")
                .and_then(Value::as_str)
                .is_some_and(|phase| phase == "ready_for_file")
            && envelope
                .payload
                .get("restored")
                .and_then(Value::as_bool)
                .unwrap_or(false)
    }

    fn local_client_disconnected_cancel(client: &ClientJob) -> ProtocolEnvelope {
        ProtocolEnvelope::new(
            "job_cancel",
            Some(client.job_id.clone()),
            client.run_id.clone(),
            json!({"reason": "local_client_disconnected"}),
        )
    }

    fn is_manual_handoff(envelope: &ProtocolEnvelope) -> bool {
        envelope
            .payload
            .get("phase")
            .and_then(Value::as_str)
            .is_some_and(|phase| phase == "manual_handoff")
            || envelope
                .payload
                .get("code")
                .and_then(Value::as_str)
                .is_some_and(|code| code == "manual_handoff")
    }

    fn update_client_effect_state(client: &mut ClientJob, envelope: &ProtocolEnvelope) {
        if envelope.kind != "job_progress" {
            return;
        }
        match envelope.payload.get("phase").and_then(Value::as_str) {
            Some("tab_opened" | "tab_grouped" | "ready_for_file" | "file_uploaded") => {
                client.side_effect_started = true;
                client.fallback_phase = Some("upload");
            }
            Some("model_selection") => {
                client.side_effect_started = true;
                client.fallback_phase = Some("model_selection");
            }
            Some("prompt_sent") => {
                client.side_effect_started = true;
                client.fallback_phase = Some("send");
            }
            Some("manual_handoff") => {
                client.side_effect_started = true;
                client.fallback_phase.get_or_insert("upload");
            }
            _ => {}
        }
    }

    fn notify_clients_transport_lost(clients: &Clients) {
        let drained: Vec<ClientJob> = clients
            .lock()
            .unwrap()
            .drain()
            .map(|(_, client)| client)
            .collect();
        for mut client in drained {
            let error = client_error_envelope(
                &client,
                "native_host_disconnected",
                "Chrome native messaging connection closed before the job finished",
            );
            let _ = write_json_frame(&mut client.stream, &error);
        }
    }

    fn client_error_envelope(client: &ClientJob, code: &str, message: &str) -> ProtocolEnvelope {
        client_error_envelope_from_parts(
            &client.job_id,
            client.run_id.clone(),
            code,
            message,
            client.fallback_phase,
            client.side_effect_started,
        )
    }

    fn client_error_envelope_from_parts(
        job_id: &str,
        run_id: Option<String>,
        code: &str,
        message: &str,
        fallback_phase: Option<&'static str>,
        side_effect_started: bool,
    ) -> ProtocolEnvelope {
        ProtocolEnvelope::new(
            "job_error",
            Some(job_id.to_string()),
            run_id,
            json!({
                "code": code,
                "message": message,
                "phase": fallback_phase.unwrap_or("upload"),
                "side_effect_started": side_effect_started,
            }),
        )
    }

    fn record_manual_handoff(paths: &ExtensionPaths, envelope: &ProtocolEnvelope) -> Result<()> {
        merge_status_file(
            &paths.status_path,
            json!({
                "last_manual_handoff": {
                    "job_id": envelope.job_id,
                    "run_id": envelope.run_id,
                    "state": envelope.payload.get("state").cloned().unwrap_or(Value::Null),
                    "message": envelope.payload.get("message").cloned().unwrap_or(Value::Null),
                    "seen_at_ms": now_millis(),
                }
            }),
        )
    }

    fn record_unrouted_extension_message(
        paths: &ExtensionPaths,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        match envelope.kind.as_str() {
            "hello" => merge_status_file(
                &paths.status_path,
                json!({
                    "extension": {
                        "extension_id": envelope.payload.get("extension_id").cloned().unwrap_or(Value::Null),
                        "extension_version": envelope.payload.get("extension_version").cloned().unwrap_or(Value::Null),
                        "protocol_version": envelope.payload.get("protocol_version").cloned().unwrap_or(Value::Null),
                        "extension_instance_id": envelope.payload.get("extension_instance_id").cloned().unwrap_or(Value::Null),
                        "profile_email": envelope.payload.get("profile_email").cloned().unwrap_or(Value::Null),
                        "profile_id": envelope.payload.get("profile_id").cloned().unwrap_or(Value::Null),
                        "recipes": envelope.payload.get("recipes").cloned().unwrap_or_else(|| json!(default_extension_recipes())),
                        "seen_at_ms": now_millis(),
                    },
                    "version_mismatch": Value::Null,
                    "last_manual_handoff": Value::Null,
                }),
            ),
            "heartbeat" => merge_status_file(
                &paths.status_path,
                json!({
                    "last_heartbeat_ms": now_millis(),
                }),
            ),
            _ => Ok(()),
        }
    }

    fn record_instance_activity(
        runtime: &NativeHostRuntime,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        match envelope.kind.as_str() {
            "hello" => write_instance_status(
                runtime,
                json!({
                    "extension_instance_id": envelope.payload.get("extension_instance_id").cloned().unwrap_or(Value::Null),
                    "extension_version": envelope.payload.get("extension_version").cloned().unwrap_or(Value::Null),
                    "profile_email": envelope.payload.get("profile_email").cloned().unwrap_or(Value::Null),
                    "profile_id": envelope.payload.get("profile_id").cloned().unwrap_or(Value::Null),
                    "recipes": envelope.payload.get("recipes").cloned().unwrap_or_else(|| json!(default_extension_recipes())),
                    "protocol_version": envelope.payload.get("protocol_version").cloned().unwrap_or(json!(PROTOCOL_VERSION)),
                }),
            ),
            "heartbeat" | "job_progress" | "job_file_chunk_ack" | "job_complete" | "job_error" => {
                write_instance_status(runtime, json!({}))
            }
            _ => Ok(()),
        }
    }

    fn write_instance_status(runtime: &NativeHostRuntime, patch: Value) -> Result<()> {
        if let Some(parent) = runtime.instance_path.parent() {
            ensure_private_dir(parent)?;
        }
        let mut value = json!({
            "native_instance_id": runtime.native_instance_id,
            "socket_path": runtime.socket_path,
            "pid": process::id(),
            "extension_instance_id": Value::Null,
            "extension_version": Value::Null,
            "profile_email": Value::Null,
            "profile_id": Value::Null,
            "recipes": default_extension_recipes(),
            "protocol_version": PROTOCOL_VERSION,
            "last_seen_ms": now_millis(),
        });
        if let Some(existing) = read_status_file(&runtime.instance_path) {
            value = existing;
            if let Some(object) = value.as_object_mut() {
                object.insert(
                    "native_instance_id".to_string(),
                    json!(runtime.native_instance_id),
                );
                object.insert("socket_path".to_string(), json!(runtime.socket_path));
                object.insert("pid".to_string(), json!(process::id()));
                object.insert("last_seen_ms".to_string(), json!(now_millis()));
            }
        }
        if let Some(object) = value.as_object_mut() {
            if let Some(patch) = patch.as_object() {
                for (key, value) in patch {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
        fs::write(
            &runtime.instance_path,
            serde_json::to_string_pretty(&value)? + "\n",
        )
        .with_context(|| {
            format!(
                "write native host instance {}",
                runtime.instance_path.display()
            )
        })
    }

    fn record_protocol_mismatch(
        paths: &ExtensionPaths,
        envelope: &ProtocolEnvelope,
        err: &anyhow::Error,
    ) -> Result<()> {
        merge_status_file(
            &paths.status_path,
            json!({
                "version_mismatch": err.to_string(),
                "last_bad_protocol": {
                    "protocol_version": envelope.protocol_version,
                    "transport": envelope.transport,
                    "type": envelope.kind,
                    "seen_at_ms": now_millis(),
                }
            }),
        )
    }

    fn without_token(mut envelope: ProtocolEnvelope) -> ProtocolEnvelope {
        envelope.capability_token = None;
        envelope
    }

    fn forward_to_extension(
        stdout: &Arc<Mutex<io::Stdout>>,
        envelope: &ProtocolEnvelope,
    ) -> Result<()> {
        let bytes = serde_json::to_vec(envelope)?;
        let mut stdout = stdout.lock().unwrap();
        write_frame_with_limit(&mut *stdout, &bytes, MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES)
    }

    fn is_timeout_error(err: &anyhow::Error) -> bool {
        matches!(
            err.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut)
        )
    }

    fn is_disconnect_error(err: &anyhow::Error) -> bool {
        matches!(
            err.downcast_ref::<io::Error>().map(io::Error::kind),
            Some(
                io::ErrorKind::UnexpectedEof
                    | io::ErrorKind::BrokenPipe
                    | io::ErrorKind::ConnectionAborted
                    | io::ErrorKind::ConnectionReset
            )
        )
    }

    fn is_recoverable_input_error(err: &anyhow::Error) -> bool {
        err.downcast_ref::<serde_json::Error>().is_some()
            || err.downcast_ref::<FrameTooLargeError>().is_some()
    }

    fn now_millis() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn invalid_local_bundle_returns_structured_job_error() {
            let (server, mut client) = UnixStream::pair().unwrap();
            let clients: Clients = Arc::new(Mutex::new(HashMap::new()));
            let handler_clients = Arc::clone(&clients);
            let token = "test-token".to_string();
            let handler_token = token.clone();
            let handler = thread::spawn(move || {
                handle_client(
                    server,
                    &handler_token,
                    Arc::new(Mutex::new(io::stdout())),
                    handler_clients,
                )
            });
            let start = ProtocolEnvelope::new(
                "job_start",
                Some("job_invalid_bundle".to_string()),
                Some("run_invalid_bundle".to_string()),
                json!({
                    "bundle_path": "/definitely/missing/yoetz-bundle.md",
                    "bundle_size": 42,
                }),
            )
            .with_token(token);

            write_json_frame(&mut client, &start).unwrap();
            let error = read_json_frame(&mut client).unwrap();

            assert_eq!(error.kind, "job_error");
            assert_eq!(error.job_id.as_deref(), Some("job_invalid_bundle"));
            assert_eq!(error.run_id.as_deref(), Some("run_invalid_bundle"));
            assert_eq!(error.payload["code"], "bundle_validation_failed");
            assert_eq!(error.payload["phase"], "upload");
            assert_eq!(error.payload["side_effect_started"], false);
            assert!(error.payload["message"]
                .as_str()
                .unwrap()
                .contains("/definitely/missing/yoetz-bundle.md"));
            assert!(handler.join().unwrap().is_ok());
            assert!(clients.lock().unwrap().is_empty());
        }

        #[test]
        fn model_selection_progress_preserves_the_diagnostic_phase() {
            let (stream, _peer) = UnixStream::pair().unwrap();
            let mut client = ClientJob {
                stream,
                job_id: "job_model".to_string(),
                run_id: Some("run_model".to_string()),
                chunks: Vec::new(),
                next_chunk: 0,
                side_effect_started: false,
                fallback_phase: None,
                cancel_on_disconnect: true,
            };
            let envelope = ProtocolEnvelope::new(
                "job_progress",
                Some("job_model".to_string()),
                Some("run_model".to_string()),
                json!({ "phase": "model_selection" }),
            );

            update_client_effect_state(&mut client, &envelope);

            assert!(client.side_effect_started);
            assert_eq!(client.fallback_phase, Some("model_selection"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        old: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let old = env::var(key).ok();
            env::set_var(key, value);
            Self { key, old }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(old) = &self.old {
                env::set_var(self.key, old);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn job_error_message_surfaces_attachment_trace() {
        let message = job_error_message(&json!({
            "code": "attachment_stalled",
            "message": "Claude attachment stalled before readiness",
            "attachment_trace": {
                "final_chunk_ack_at_ms": 100,
                "hard_timeout_pending_legs": ["matching_thumbnail"]
            }
        }));

        assert!(message.contains("attachment_trace="));
        assert!(message.contains("final_chunk_ack_at_ms"));
        assert!(message.contains("matching_thumbnail"));
    }

    #[test]
    fn job_error_message_marks_an_oversized_attachment_trace() {
        let message = job_error_message(&json!({
            "code": "attachment_stalled",
            "message": "Claude attachment stalled before readiness",
            "attachment_trace": { "diagnostic": "x".repeat(4097) }
        }));

        assert!(message.contains("attachment_trace=<omitted:"));
    }

    #[test]
    fn job_error_message_surfaces_model_selection_failure_reason() {
        let message = job_error_message(&json!({
            "code": "model_selection_failed",
            "message": "Requested ChatGPT model was not selected",
            "phase": "model_selection",
            "failure_reason": "effort_slider_move_failed"
        }));

        assert!(message.contains("failure_reason=effort_slider_move_failed"));
        assert!(message.contains("phase model_selection"));
    }

    #[test]
    fn frame_round_trips_json_envelope() {
        let envelope = ProtocolEnvelope::new(
            "job_progress",
            Some("job_1".to_string()),
            Some("run_1".to_string()),
            json!({"message": "uploading"}),
        );
        let mut buf = Vec::new();
        write_json_frame(&mut buf, &envelope).unwrap();
        assert!(buf.len() > 4);
        let decoded = read_json_frame(&mut &buf[..]).unwrap();
        assert_eq!(decoded.kind, "job_progress");
        assert_eq!(decoded.payload["message"], "uploading");
    }

    #[test]
    fn chatgpt_job_start_payload_carries_conversation_id() {
        let bundle = BundleInfo {
            path: PathBuf::from("/tmp/yoetz-bundle.md"),
            file_name: "yoetz-bundle.md".to_string(),
            size: 42,
            mime: "text/markdown".to_string(),
        };
        let spec = ChatgptRecipeSpec {
            bundle_path: Some(bundle.path.clone()),
            model: crate::chatgpt_recipe::CHATGPT_SOL_EXTRA_HIGH_MODEL.to_string(),
            model_strategy: crate::chatgpt_recipe::ChatgptModelStrategy::Select,
            prompt: "continue".to_string(),
            browser_context_id: None,
            profile_email: None,
            extension_instance_id: None,
            extension_profile_id: None,
            conversation_id: Some("conv-123".to_string()),
            run_id: "run-123".to_string(),
            wait_timeout_ms: 10_000,
            wait_interval_ms: 1_000,
            upload_timeout_ms: 2_000,
            send_timeout_ms: 3_000,
            close_tab_on_complete: true,
        };

        let payload = chatgpt_job_start_payload(&spec, &bundle);

        assert_eq!(payload["conversation_id"], "conv-123");
        assert_eq!(payload["close_tab_on_complete"], true);
    }

    #[test]
    fn claude_job_start_payload_carries_recipe_model_and_conversation() {
        let bundle = BundleInfo {
            path: PathBuf::from("/tmp/yoetz-bundle.md"),
            file_name: "yoetz-bundle.md".to_string(),
            size: 42,
            mime: "text/markdown".to_string(),
        };
        let conversation_id = "123e4567-e89b-12d3-a456-426614174000";
        let spec = crate::claude_recipe::ClaudeRecipeSpec {
            bundle_path: Some(bundle.path.clone()),
            prompt: "continue".to_string(),
            browser_context_id: None,
            profile_email: None,
            extension_instance_id: Some("ext_claude".to_string()),
            extension_profile_id: None,
            conversation_id: Some(conversation_id.to_string()),
            run_id: "run-claude".to_string(),
            wait_timeout_ms: 10_000,
            wait_interval_ms: 1_000,
            upload_timeout_ms: 2_000,
            attachment_stall_timeout_ms: 420_000,
            send_timeout_ms: 3_000,
            close_tab_on_complete: false,
            warnings: vec!["size warning".to_string()],
        };

        let payload = claude_job_start_payload(&spec, &bundle);

        assert_eq!(payload["recipe"], "claude");
        assert_eq!(
            payload["model"],
            crate::claude_recipe::CLAUDE_FABLE_MAX_MODEL
        );
        assert_eq!(payload["model_strategy"], "select");
        assert_eq!(payload["conversation_id"], conversation_id);
        assert_eq!(payload["extension_instance_id"], "ext_claude");
        assert_eq!(payload["attachment_stall_timeout_ms"], 420_000);
        assert_eq!(payload["close_tab_on_complete"], false);
    }

    #[test]
    fn selected_instance_capability_gate_is_recipe_specific_and_legacy_safe() {
        let mut instance = ExtensionInstanceStatus {
            native_instance_id: "native_claude".to_string(),
            socket_path: PathBuf::from("/tmp/claude.sock"),
            pid: 1,
            extension_instance_id: Some("ext_claude".to_string()),
            extension_version: Some("0.5.33".to_string()),
            profile_email: None,
            profile_id: None,
            recipes: vec!["chatgpt".to_string()],
            protocol_version: PROTOCOL_VERSION,
            last_seen_ms: 1,
        };

        ensure_instance_supports_recipe(&instance, "chatgpt").unwrap();
        let error = ensure_instance_supports_recipe(&instance, "claude").unwrap_err();
        assert!(error
            .to_string()
            .contains("does not advertise recipe `claude`"));
        assert!(error.to_string().contains("before job_start"));

        instance.recipes.push("claude".to_string());
        ensure_instance_supports_recipe(&instance, "claude").unwrap();
    }

    #[test]
    fn extension_update_readiness_requires_new_instance_version_and_recipe() {
        let expected_version = format!("{YOETZ_CLI_VERSION}.7");
        let mut instance = ExtensionInstanceStatus {
            native_instance_id: "native_before".to_string(),
            socket_path: PathBuf::from("/tmp/claude.sock"),
            pid: 1,
            extension_instance_id: Some("ext_claude".to_string()),
            extension_version: Some(expected_version.clone()),
            profile_email: None,
            profile_id: None,
            recipes: vec!["chatgpt".to_string(), "claude".to_string()],
            protocol_version: PROTOCOL_VERSION,
            last_seen_ms: 1,
        };

        assert!(!extension_update_is_active(
            &instance,
            &expected_version,
            "claude",
            "native_before"
        ));

        instance.native_instance_id = "native_after".to_string();
        instance.recipes = vec!["chatgpt".to_string()];
        assert!(!extension_update_is_active(
            &instance,
            &expected_version,
            "claude",
            "native_before"
        ));

        instance.recipes.push("claude".to_string());
        assert!(extension_update_is_active(
            &instance,
            &expected_version,
            "claude",
            "native_before"
        ));

        instance.extension_version = Some("0.0.0".to_string());
        assert!(!extension_update_is_active(
            &instance,
            YOETZ_CLI_VERSION,
            "claude",
            "native_before"
        ));
    }

    #[test]
    fn parse_recipe_result_carries_conversation_fields() {
        let envelope = ProtocolEnvelope::new(
            "job_complete",
            Some("job_1".to_string()),
            Some("run_1".to_string()),
            json!({
                "response": "done",
                "model_used": "GPT-5.6 Sol Extra High",
                "model_selection_status": "selected",
                "warnings": [
                    "kept current",
                    {
                        "code": "artifact_unextracted",
                        "count": 1,
                        "titles": ["Release plan"]
                    }
                ],
                "conversation_id": "conv-123",
                "conversation_url": "https://chatgpt.com/c/conv-123",
                "extraction_method": "copy_scope_dom_fallback",
                "completion_reason": "copy_button",
                "finality_anchor": "dom_only",
                "stable_for_ms": 5000,
                "assistant_turn_count": 2,
                "copy_button_count": 1,
            }),
        );

        let result = parse_recipe_result(envelope).unwrap();

        assert_eq!(result.response, "done");
        assert_eq!(result.model_used.as_deref(), Some("GPT-5.6 Sol Extra High"));
        assert_eq!(
            result.model_selection_status,
            ChatgptModelSelectionStatus::Selected
        );
        assert_eq!(result.warnings, vec!["kept current"]);
        assert_eq!(
            result.warning_details,
            vec![json!({
                "code": "artifact_unextracted",
                "count": 1,
                "titles": ["Release plan"]
            })]
        );
        assert_eq!(result.conversation_id.as_deref(), Some("conv-123"));
        assert_eq!(
            result.conversation_url.as_deref(),
            Some("https://chatgpt.com/c/conv-123")
        );
        assert_eq!(
            result.diagnostics.extraction_method.as_deref(),
            Some("copy_scope_dom_fallback")
        );
        assert_eq!(
            result.diagnostics.completion_reason.as_deref(),
            Some("copy_button")
        );
        assert_eq!(
            result.diagnostics.finality_anchor.as_deref(),
            Some("dom_only")
        );
        assert_eq!(result.diagnostics.stable_for_ms, Some(5000));
        assert_eq!(result.diagnostics.assistant_turn_count, Some(2));
        assert_eq!(result.diagnostics.copy_button_count, Some(1));
    }

    #[test]
    fn job_complete_parsing_tolerates_absent_tab_reporting_fields() {
        let envelope = ProtocolEnvelope::new(
            "job_complete",
            Some("job_old_extension".to_string()),
            Some("run_old_extension".to_string()),
            json!({
                "response": "done",
                "conversation_id": "conv-old-extension",
                "conversation_url": "https://chatgpt.com/c/conv-old-extension"
            }),
        );

        let result = parse_recipe_result(envelope).unwrap();

        assert_eq!(result.response, "done");
        assert_eq!(
            result.conversation_url.as_deref(),
            Some("https://chatgpt.com/c/conv-old-extension")
        );
    }

    #[test]
    fn oversized_frame_is_rejected() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&((MAX_FRAME_BYTES as u32) + 1).to_ne_bytes());
        let err = read_frame(&mut &bytes[..]).unwrap_err();
        assert!(err.to_string().contains("too large"));
    }

    #[test]
    fn chrome_native_stdout_limit_is_enforced_separately() {
        let mut buf = Vec::new();
        let oversized = vec![b'x'; MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES + 1];
        let err =
            write_frame_with_limit(&mut buf, &oversized, MAX_CHROME_NATIVE_HOST_MESSAGE_BYTES)
                .unwrap_err();
        assert!(err.to_string().contains("too large"));
        assert!(buf.is_empty());
    }

    #[test]
    fn malformed_json_frame_does_not_desync_next_frame() {
        let envelope = ProtocolEnvelope::new(
            "heartbeat",
            Some("job_1".to_string()),
            Some("run_1".to_string()),
            json!({"status": "alive"}),
        );
        let mut buf = Vec::new();
        write_frame(&mut buf, b"{not-json").unwrap();
        write_json_frame(&mut buf, &envelope).unwrap();

        let mut cursor = &buf[..];
        let err = read_json_frame(&mut cursor).unwrap_err();
        assert!(err.downcast_ref::<serde_json::Error>().is_some());
        let decoded = read_json_frame(&mut cursor).unwrap();
        assert_eq!(decoded.kind, "heartbeat");
        assert_eq!(decoded.payload["status"], "alive");
    }

    #[test]
    fn native_host_manifest_uses_stable_extension_origin() {
        let manifest = native_host_manifest(Path::new("/tmp/yoetz-wrapper")).unwrap();
        assert_eq!(manifest["name"], NATIVE_HOST_NAME);
        assert_eq!(manifest["type"], "stdio");
        assert_eq!(
            manifest["allowed_origins"][0],
            format!("chrome-extension://{EXTENSION_ID}/")
        );
    }

    #[test]
    fn pinned_key_derives_expected_extension_id() {
        assert_eq!(
            extension_id_from_public_key(EXTENSION_KEY).unwrap(),
            EXTENSION_ID
        );
    }

    #[test]
    #[cfg(unix)]
    fn short_state_dir_keeps_socket_under_state_dir() {
        let state_dir = PathBuf::from("/tmp/yoetz-short-state");
        assert_eq!(
            default_socket_path(&state_dir),
            state_dir.join(SOCKET_FILENAME)
        );
    }

    #[test]
    #[cfg(unix)]
    fn long_state_dir_uses_short_hashed_socket_path() {
        let state_dir = PathBuf::from("/tmp").join("a".repeat(180));
        let socket = default_socket_path(&state_dir);

        assert_ne!(socket, state_dir.join(SOCKET_FILENAME));
        let parent_name = socket
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(parent_name.starts_with("yoetz-cen-"));
        assert!(unix_socket_path_fits(&socket));
        assert_eq!(
            socket.extension().and_then(|ext| ext.to_str()),
            Some("sock")
        );
    }

    #[test]
    #[cfg(unix)]
    fn hashed_socket_parent_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let long_state = dir.path().join("a".repeat(180));
        let paths = ExtensionPaths {
            state_dir: long_state.clone(),
            instances_dir: long_state.join(INSTANCES_DIRNAME),
            manifest_path: long_state.join("manifest.json"),
            wrapper_path: long_state.join("wrapper"),
            socket_path: default_socket_path(&long_state),
            token_path: long_state.join(TOKEN_FILENAME),
            status_path: long_state.join(STATUS_FILENAME),
        };
        let socket = socket_fallback_dir(&paths.state_dir).join("native-instance.sock");

        native_host_unix::ensure_socket_parent_path(&paths, &socket).unwrap();

        let mode = fs::metadata(socket.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    #[cfg(unix)]
    fn bind_validates_hashed_socket_parent_before_stale_cleanup() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let long_state = dir.path().join("b".repeat(180));
        let paths = ExtensionPaths {
            state_dir: long_state.clone(),
            instances_dir: long_state.join(INSTANCES_DIRNAME),
            manifest_path: long_state.join("manifest.json"),
            wrapper_path: long_state.join("wrapper"),
            socket_path: default_socket_path(&long_state),
            token_path: long_state.join(TOKEN_FILENAME),
            status_path: long_state.join(STATUS_FILENAME),
        };
        let fallback_dir = socket_fallback_dir(&paths.state_dir);
        if let Ok(metadata) = fs::symlink_metadata(&fallback_dir) {
            if metadata.file_type().is_symlink() || metadata.is_file() {
                fs::remove_file(&fallback_dir).unwrap();
            } else {
                fs::remove_dir_all(&fallback_dir).unwrap();
            }
        }
        let target_dir = dir.path().join("socket-target");
        fs::create_dir_all(&target_dir).unwrap();
        let target_socket = target_dir.join(
            paths
                .socket_path
                .file_name()
                .expect("fallback socket filename"),
        );
        {
            let listener = UnixListener::bind(&target_socket).unwrap();
            drop(listener);
        }
        std::os::unix::fs::symlink(&target_dir, &fallback_dir).unwrap();

        let err = native_host_unix::bind_native_host_listener(&paths, "native-test").unwrap_err();

        assert!(err
            .to_string()
            .contains("private directory must not be a symlink"));
        assert!(target_socket.exists());
        fs::remove_file(&fallback_dir).unwrap();
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn explicit_socket_env_is_honored() {
        let dir = TempDir::new().unwrap();
        let explicit = dir.path().join("explicit.sock");
        let _socket_guard = EnvGuard::set("YOETZ_CHROME_EXTENSION_NATIVE_SOCKET", &explicit);
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));

        let paths = extension_paths().unwrap();
        assert_eq!(paths.socket_path, explicit);
    }

    #[test]
    #[cfg(unix)]
    fn stale_socket_cleanup_rejects_regular_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("not-a-socket.sock");
        fs::write(&path, "do not delete").unwrap();

        let err = native_host_unix::remove_stale_socket(&path).unwrap_err();

        assert!(err.to_string().contains("refusing to remove non-socket"));
        assert!(path.exists());
    }

    #[test]
    #[cfg(unix)]
    fn stale_socket_cleanup_rejects_symlinks() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.sock");
        let path = dir.path().join("socket-symlink.sock");
        fs::write(&target, "do not delete").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();

        let err = native_host_unix::remove_stale_socket(&path).unwrap_err();

        assert!(err.to_string().contains("refusing to remove symlink"));
        assert!(path.exists());
    }

    #[test]
    #[serial]
    fn install_host_writes_manifest_wrapper_and_token_under_isolated_home() {
        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);

        let result = install_host().unwrap();
        assert!(result.manifest_path.exists());
        assert!(result.wrapper_path.exists());
        assert!(result.token_path.exists());
        let manifest = fs::read_to_string(result.manifest_path).unwrap();
        assert!(manifest.contains(EXTENSION_ID));
        let wrapper = fs::read_to_string(result.wrapper_path).unwrap();
        assert!(wrapper.contains("YOETZ_DIR="));
        let wrapper_target = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "wrapper_target")
            .unwrap();
        assert!(wrapper_target.ok, "{}", wrapper_target.detail);
        let token = fs::read_to_string(&result.token_path).unwrap();
        assert!(token.trim().len() >= 32);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let extension_state_dir = result.token_path.parent().unwrap();
            let state_mode = fs::metadata(extension_state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let token_mode = fs::metadata(result.token_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(state_mode, 0o700);
            assert_eq!(token_mode, 0o600);
        }
    }

    #[test]
    #[serial]
    fn status_reports_not_installed_without_manifest() {
        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let payload = status().unwrap();
        assert_eq!(payload.status, "not_installed");
        assert!(!payload.manifest_installed);
        assert!(!payload.token_present);
    }

    #[test]
    fn chatgpt_auth_doctor_check_reports_login_required_probe() {
        let check = chatgpt_auth_doctor_check_from_payload(&json!({
            "status": "login_required",
            "authenticated": false,
            "message": "ChatGPT login required in this Chrome profile",
            "tab_id": 7,
            "selection": "active_non_yoetz_chatgpt_tab",
            "url": "https://chatgpt.com/auth/login",
            "yoetz_owned_tabs_open": 3,
            "yoetz_owned_complete_tabs_open": 1
        }));

        assert_eq!(check.name, "chatgpt_auth");
        assert!(!check.ok);
        assert!(check.detail.contains("login_required"));
        assert!(check.detail.contains("tab_id=7"));
        assert!(check.detail.contains("active_non_yoetz_chatgpt_tab"));
        assert!(check.detail.contains("yoetz_owned_tabs_open=3"));
        assert!(check.detail.contains("yoetz_owned_complete_tabs_open=1"));
    }

    #[test]
    fn chatgpt_auth_doctor_check_accepts_authenticated_probe() {
        let check = chatgpt_auth_doctor_check_from_payload(&json!({
            "status": "authenticated",
            "authenticated": true,
            "message": "ChatGPT authenticated in this Chrome profile",
            "tab_id": 7,
            "selection": "active_non_yoetz_chatgpt_tab",
            "url": "https://chatgpt.com/"
        }));

        assert_eq!(check.name, "chatgpt_auth");
        assert!(check.ok);
        assert!(check.detail.contains("authenticated"));
        assert!(check.detail.contains("tab_id=7"));
    }

    #[test]
    fn chatgpt_auth_doctor_check_keeps_no_tab_probe_informational() {
        let check = chatgpt_auth_doctor_check_from_payload(&json!({
            "status": "no_chatgpt_tab",
            "authenticated": false,
            "message": "No ChatGPT tab is open in this Chrome profile; open https://chatgpt.com/ and rerun doctor",
            "inspected_tabs": 0
        }));

        assert_eq!(check.name, "chatgpt_auth");
        assert!(check.ok);
        assert!(check.detail.contains("no_chatgpt_tab"));
        assert!(check.detail.contains("open https://chatgpt.com/"));
    }

    #[test]
    #[serial]
    fn stale_status_file_does_not_count_as_live_hello() {
        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        write_status_file(
            &paths.status_path,
            &json!({
                "extension": {
                    "extension_id": EXTENSION_ID,
                    "extension_version": "0.2.0",
                    "protocol_version": PROTOCOL_VERSION,
                    "extension_instance_id": "ext_123",
                    "profile_email": "work@example.com",
                    "profile_id": "gaia_123",
                    "seen_at_ms": 1234,
                }
            }),
        )
        .unwrap();

        let payload = status().unwrap();

        assert!(!payload.hello_seen);
        assert_eq!(payload.extension_version, None);
        assert_eq!(payload.extension_instance_id, None);
        assert_eq!(payload.recipes, vec!["chatgpt"]);
        assert!(!payload.claude_ready);

        let extension_hello = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "extension_hello")
            .unwrap();
        assert!(!extension_hello.ok);
        assert_eq!(extension_hello.detail, "no extension hello observed");
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn status_reports_extension_profile_fields_from_live_hello() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("work.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: socket,
                pid: process::id(),
                extension_instance_id: Some("ext_123".to_string()),
                extension_version: Some("0.2.0".to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("gaia_123".to_string()),
                recipes: vec!["chatgpt".to_string(), "claude".to_string()],
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1234,
            },
        );

        let payload = status().unwrap();

        assert!(payload.hello_seen);
        assert_eq!(payload.extension_version.as_deref(), Some("0.2.0"));
        assert_eq!(payload.extension_instance_id.as_deref(), Some("ext_123"));
        assert_eq!(
            payload.extension_profile_email.as_deref(),
            Some("work@example.com")
        );
        assert_eq!(payload.extension_profile_id.as_deref(), Some("gaia_123"));
        assert_eq!(payload.recipes, vec!["chatgpt", "claude"]);
        assert!(payload.claude_ready);

        let extension_hello = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "extension_hello")
            .unwrap();
        assert!(extension_hello.ok);
        assert_eq!(
            extension_hello.detail,
            "extension_version=0.2.0, extension_instance_id=ext_123, chrome_profile_email=work@example.com"
        );
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn live_extension_connection_supersedes_historical_manual_handoff() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let state = dir.path().join("state");
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state);
        let extension_version = format!("{YOETZ_CLI_VERSION}.1");
        write_extension_source_fixture(&state.join("chatgpt-native-extension"), &extension_version);
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        write_status_file(
            &paths.status_path,
            &json!({
                "last_manual_handoff": {
                    "job_id": "job_old",
                    "run_id": "run_old",
                    "state": "challenge_required",
                    "message": "old ChatGPT job requires manual handoff",
                    "seen_at_ms": 1234,
                }
            }),
        )
        .unwrap();
        let socket = dir.path().join("work.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: socket,
                pid: process::id(),
                extension_instance_id: Some("ext_123".to_string()),
                extension_version: Some(extension_version),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("gaia_123".to_string()),
                recipes: vec!["chatgpt".to_string()],
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 5678,
            },
        );

        let payload = status().unwrap();

        assert_eq!(payload.status, "connected");
        assert!(payload.socket_reachable);
        assert!(payload.hello_seen);
    }

    #[test]
    fn selected_instance_capability_is_recipe_exact() {
        let instance = ExtensionInstanceStatus {
            native_instance_id: "native_1".to_string(),
            socket_path: PathBuf::from("/tmp/native_1.sock"),
            pid: process::id(),
            extension_instance_id: Some("ext_1".to_string()),
            extension_version: Some("0.5.33".to_string()),
            profile_email: None,
            profile_id: None,
            recipes: vec!["chatgpt".to_string()],
            protocol_version: PROTOCOL_VERSION,
            last_seen_ms: 1,
        };
        assert!(instance_advertises_recipe(
            &instance,
            BuiltinWebRecipe::Chatgpt
        ));
        assert!(!instance_advertises_recipe(
            &instance,
            BuiltinWebRecipe::Claude
        ));
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn doctor_reports_loaded_extension_version_skew() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("work.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: socket,
                pid: process::id(),
                extension_instance_id: Some("ext_123".to_string()),
                extension_version: Some("0.5.13".to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("gaia_123".to_string()),
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1234,
            },
        );

        let payload = status().unwrap();

        assert_eq!(payload.status, "version_mismatch");
        assert!(payload.detail.contains("extension version 0.5.13"));
        assert!(payload.detail.contains(env!("CARGO_PKG_VERSION")));

        let version_compatible = doctor()
            .unwrap()
            .checks
            .into_iter()
            .find(|check| check.name == "version_compatible")
            .unwrap();
        assert!(!version_compatible.ok);
        assert!(version_compatible
            .detail
            .contains("extension version 0.5.13"));
        assert!(version_compatible
            .detail
            .contains(env!("CARGO_PKG_VERSION")));
        assert!(version_compatible
            .detail
            .contains("yoetz browser extension update --chatgpt"));
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn status_and_doctor_reject_non_managed_loaded_copy() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let state = dir.path().join("state");
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state);
        write_extension_source_fixture(
            &state.join("chatgpt-native-extension"),
            &format!("{YOETZ_CLI_VERSION}.9"),
        );
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("work.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: socket,
                pid: process::id(),
                extension_instance_id: Some("ext_123".to_string()),
                extension_version: Some(YOETZ_CLI_VERSION.to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("gaia_123".to_string()),
                recipes: vec!["chatgpt".to_string(), "claude".to_string()],
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1234,
            },
        );

        let payload = status().unwrap();
        assert_eq!(payload.status, "managed_copy_mismatch");
        assert!(payload.detail.contains("non-managed copy"));
        assert!(payload.detail.contains("remove the Yoetz card"));
        assert!(payload.detail.contains(&format!(
            "Load unpacked {}",
            managed_chatgpt_extension_dir().unwrap().display()
        )));

        let report = doctor().unwrap();
        let version_compatible = report
            .checks
            .iter()
            .find(|check| check.name == "version_compatible")
            .unwrap();
        assert!(version_compatible.ok);
        let managed_copy = report
            .checks
            .iter()
            .find(|check| check.name == "managed_extension_copy")
            .unwrap();
        assert!(!managed_copy.ok);
        assert!(managed_copy.detail.contains("non-managed copy"));
        assert!(!report.ok);
    }

    #[test]
    #[serial]
    fn managed_extension_dir_uses_stable_yoetz_state_dir() {
        let dir = TempDir::new().unwrap();
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));

        let path = managed_chatgpt_extension_dir().unwrap();

        assert_eq!(
            path,
            dir.path().join("state").join("chatgpt-native-extension")
        );
    }

    #[test]
    #[serial]
    fn lifecycle_lock_allows_parallel_recipes_and_rejects_mutation() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        let state = dir.path().join("state");
        let manifest_dir = dir.path().join("native-hosts");
        let _source_guard = EnvGuard::set(CHATGPT_EXTENSION_DIR_ENV, &source);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state);
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);

        let recipe_a = acquire_extension_lifecycle_shared("recipe A").unwrap();
        let recipe_b = acquire_extension_lifecycle_shared("recipe B").unwrap();

        let started_at = Instant::now();
        let error = setup_extension().unwrap_err();
        assert!(started_at.elapsed() < Duration::from_secs(1));
        assert!(error.to_string().contains("extension_lifecycle_busy"));
        assert!(!state.join("chatgpt-native-extension").exists());
        assert!(!manifest_dir
            .join(format!("{NATIVE_HOST_NAME}.json"))
            .exists());

        drop(recipe_b);
        drop(recipe_a);
        setup_extension().unwrap();
        assert!(state.join("chatgpt-native-extension").exists());
        assert!(manifest_dir
            .join(format!("{NATIVE_HOST_NAME}.json"))
            .exists());
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn lifecycle_lock_guard_releases_a_fork_inherited_descriptor() {
        let dir = TempDir::new().unwrap();
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let recipe = acquire_extension_lifecycle_shared("fork inheritance test").unwrap();
        let _child = crate::test_support::ForkChild::sleep_for(Duration::from_secs(5));

        drop(recipe);

        acquire_extension_lifecycle_exclusive("verify guard release").unwrap();
    }

    #[test]
    #[serial]
    fn sync_managed_extension_replaces_stale_copy_atomically() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        let target = dir.path().join("managed");
        fs::create_dir_all(target.join("src")).unwrap();
        fs::write(target.join("manifest.json"), r#"{"version":"0.5.32"}"#).unwrap();
        fs::write(target.join("src").join("stale.js"), "stale").unwrap();

        let result = sync_managed_chatgpt_extension_from(&source, &target).unwrap();

        assert_eq!(result.status, "updated");
        assert_eq!(result.source_dir, source);
        assert_eq!(result.extension_dir, target);
        assert_eq!(
            extension_manifest_version(&result.extension_dir).as_deref(),
            Some(format!("{YOETZ_CLI_VERSION}.1").as_str())
        );
        assert_eq!(
            fs::read_to_string(source.join("manifest.json")).unwrap(),
            format!(r#"{{"version":"{YOETZ_CLI_VERSION}"}}"#)
        );
        assert_eq!(
            result.manifest_version.as_deref(),
            Some(format!("{YOETZ_CLI_VERSION}.1").as_str())
        );
        assert_eq!(
            fs::read_to_string(result.extension_dir.join("src").join("service-worker.js")).unwrap(),
            format!("service-worker:{YOETZ_CLI_VERSION}")
        );
        assert!(!result.extension_dir.join("src").join("stale.js").exists());
    }

    #[test]
    #[serial]
    fn sync_managed_extension_rejects_an_older_source_before_mutation() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, "0.5.32");
        let target = dir.path().join("managed");
        write_extension_source_fixture(&target, YOETZ_CLI_VERSION);
        fs::write(
            target.join("src").join("sentinel.js"),
            "keep-newer-managed-copy",
        )
        .unwrap();

        let err = sync_managed_chatgpt_extension_from(&source, &target).unwrap_err();

        let message = format!("{err:#}");
        assert!(message.contains("refusing to sync"));
        assert!(message.contains("0.5.32"));
        assert!(message.contains(YOETZ_CLI_VERSION));
        assert!(message.contains(source.to_string_lossy().as_ref()));
        assert_eq!(
            fs::read_to_string(target.join("src").join("sentinel.js")).unwrap(),
            "keep-newer-managed-copy"
        );
    }

    #[test]
    #[serial]
    fn sync_managed_extension_classifies_an_unstamped_managed_target_as_restamped() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        fs::write(
            source.join("src").join("service-worker.js"),
            "service-worker:new-source",
        )
        .unwrap();
        let target = dir.path().join("managed");
        write_extension_source_fixture(&target, YOETZ_CLI_VERSION);

        let result = sync_managed_chatgpt_extension_from(&source, &target).unwrap();

        assert_eq!(result.status, "restamped");
        assert_eq!(
            result.manifest_version.as_deref(),
            Some(format!("{YOETZ_CLI_VERSION}.1").as_str())
        );
        assert_eq!(
            result.previous_manifest_version.as_deref(),
            Some(YOETZ_CLI_VERSION)
        );
        let loaded_before_restamp = ExtensionInstanceStatus {
            native_instance_id: "native_restamped".to_string(),
            socket_path: dir.path().join("native.sock"),
            pid: process::id(),
            extension_instance_id: Some("ext_restamped".to_string()),
            extension_version: result.previous_manifest_version.clone(),
            profile_email: None,
            profile_id: None,
            recipes: default_extension_recipes(),
            protocol_version: PROTOCOL_VERSION,
            last_seen_ms: 1,
        };
        assert!(ensure_reload_can_reach_managed_copy(&loaded_before_restamp, &result).is_ok());
    }

    #[test]
    #[serial]
    fn managed_extension_stamp_increments_only_when_source_changes() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        let target = dir.path().join("managed");
        let first_version = format!("{YOETZ_CLI_VERSION}.1");
        let second_version = format!("{YOETZ_CLI_VERSION}.2");

        let first = sync_managed_chatgpt_extension_from(&source, &target).unwrap();
        assert_eq!(
            first.manifest_version.as_deref(),
            Some(first_version.as_str())
        );

        fs::write(
            source.join("src").join("service-worker.js"),
            "service-worker:changed",
        )
        .unwrap();
        let second = sync_managed_chatgpt_extension_from(&source, &target).unwrap();
        assert_eq!(second.status, "updated");
        assert_eq!(
            second.manifest_version.as_deref(),
            Some(second_version.as_str())
        );

        let current = sync_managed_chatgpt_extension_from(&source, &target).unwrap();
        assert_eq!(current.status, "current");
        assert_eq!(
            current.manifest_version.as_deref(),
            Some(second_version.as_str())
        );
    }

    #[test]
    #[serial]
    fn prepare_managed_extension_materializes_from_discovered_source() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        let state = dir.path().join("state");
        let _source_guard = EnvGuard::set(CHATGPT_EXTENSION_DIR_ENV, &source);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state);

        let result = prepare_managed_chatgpt_extension_unlocked().unwrap();

        assert_eq!(result.source_dir, source.canonicalize().unwrap());
        assert_eq!(result.extension_dir, state.join("chatgpt-native-extension"));
        assert!(is_chatgpt_extension_source_dir(&result.extension_dir));
        let payload = serde_json::to_value(&result).unwrap();
        assert_eq!(payload["source_version"], YOETZ_CLI_VERSION);
        assert_eq!(payload["source_provenance"], "environment_override");
    }

    #[test]
    #[serial]
    fn prepare_managed_extension_refreshes_legacy_loaded_unpacked_dir_when_present() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source");
        write_extension_source_fixture(&source, YOETZ_CLI_VERSION);
        let state = dir.path().join("state");
        let legacy_loaded = state.join("chrome-extension-native").join("unpacked");
        write_extension_source_fixture(&legacy_loaded, "0.5.32");
        let _source_guard = EnvGuard::set(CHATGPT_EXTENSION_DIR_ENV, &source);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state);

        let result = prepare_managed_chatgpt_extension_unlocked().unwrap();

        assert_eq!(result.extension_dir, state.join("chatgpt-native-extension"));
        assert!(result.loaded_extension_dirs.contains(&result.extension_dir));
        assert!(result.loaded_extension_dirs.contains(&legacy_loaded));
        assert_eq!(
            fs::read_to_string(legacy_loaded.join("manifest.json")).unwrap(),
            fs::read_to_string(result.extension_dir.join("manifest.json")).unwrap()
        );
        assert_eq!(
            fs::read_to_string(legacy_loaded.join("src").join("service-worker.js")).unwrap(),
            format!("service-worker:{YOETZ_CLI_VERSION}")
        );
    }

    #[test]
    fn managed_extension_versions_are_cli_compatible_but_identity_exact() {
        let managed_version = format!("{YOETZ_CLI_VERSION}.42");
        let stale_managed_version = format!("{YOETZ_CLI_VERSION}.41");
        assert!(extension_version_skew_message(Some(&managed_version), None).is_none());
        assert!(extension_version_skew_message(Some("not-a-version"), None).is_some());

        let unstamped = managed_extension_identity_message(
            Some(YOETZ_CLI_VERSION),
            Some(YOETZ_CLI_VERSION),
            Path::new("/managed/extension"),
        )
        .unwrap();
        assert!(unstamped.contains("no stamped sync identity"));
        assert!(unstamped.contains("extension update"));

        assert!(managed_extension_identity_message(
            Some(&managed_version),
            Some(&managed_version),
            Path::new("/managed/extension")
        )
        .is_none());
        let source_copy = managed_extension_identity_message(
            Some(YOETZ_CLI_VERSION),
            Some(&managed_version),
            Path::new("/managed/extension"),
        )
        .unwrap();
        assert!(source_copy.contains("non-managed copy"));
        assert!(source_copy.contains("remove the Yoetz card"));
        assert!(source_copy.contains("Load unpacked /managed/extension"));

        let stale_copy = managed_extension_identity_message(
            Some(&stale_managed_version),
            Some(&managed_version),
            Path::new("/managed/extension"),
        )
        .unwrap();
        assert!(stale_copy.contains("stale managed copy"));
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn wait_for_extension_version_requires_current_hello() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("current.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_current".to_string(),
                socket_path: socket,
                pid: process::id(),
                extension_instance_id: Some("ext_current".to_string()),
                extension_version: Some(YOETZ_CLI_VERSION.to_string()),
                profile_email: None,
                profile_id: None,
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1,
            },
        );

        let selected = wait_for_extension_version(
            &paths,
            ExtensionInstanceSelector::default(),
            YOETZ_CLI_VERSION,
        )
        .unwrap();

        assert_eq!(selected.native_instance_id, "native_current");
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn dead_pid_instances_are_filtered_and_pruned_by_reset_path() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let _manifest_guard = EnvGuard::set(
            "YOETZ_CHROME_NATIVE_MESSAGING_DIR",
            &dir.path().join("native-hosts"),
        );
        let _state_guard = EnvGuard::set("YOETZ_DIR", &dir.path().join("state"));
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let socket = dir.path().join("stale.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        let dead_pid = i32::MAX as u32;
        assert!(!process_alive(dead_pid));
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_stale".to_string(),
                socket_path: socket,
                pid: dead_pid,
                extension_instance_id: Some("ext_stale".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: Some("stale@example.com".to_string()),
                profile_id: Some("stale_profile".to_string()),
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1234,
            },
        );

        let payload = status().unwrap();

        assert!(payload.connected_instances.is_empty());
        assert!(!payload.hello_seen);
        assert!(paths.instances_dir.join("native_stale.json").exists());

        let pruned = prune_stale_instance_records().unwrap();

        assert_eq!(pruned, 1);
        assert!(!paths.instances_dir.join("native_stale.json").exists());
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn select_extension_instance_routes_by_profile_and_fails_closed_when_ambiguous() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let work_socket = dir.path().join("work.sock");
        let personal_socket = dir.path().join("personal.sock");
        let _work_listener = UnixListener::bind(&work_socket).unwrap();
        let _personal_listener = UnixListener::bind(&personal_socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: work_socket.clone(),
                pid: process::id(),
                extension_instance_id: Some("ext_work".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: Some("work@example.com".to_string()),
                profile_id: Some("work_profile".to_string()),
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 2,
            },
        );
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_personal".to_string(),
                socket_path: personal_socket.clone(),
                pid: process::id(),
                extension_instance_id: Some("ext_personal".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: Some("personal@example.com".to_string()),
                profile_id: Some("personal_profile".to_string()),
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1,
            },
        );

        let err =
            select_extension_instance(&paths, ExtensionInstanceSelector::default()).unwrap_err();
        assert!(err
            .to_string()
            .contains("multiple connected extension profiles"));

        let selected = select_extension_instance(
            &paths,
            ExtensionInstanceSelector {
                profile_email: Some("WORK@EXAMPLE.COM"),
                ..ExtensionInstanceSelector::default()
            },
        )
        .unwrap();
        assert_eq!(selected.native_instance_id, "native_work");
        assert_eq!(selected.socket_path, work_socket);
    }

    #[test]
    #[cfg(unix)]
    #[serial]
    fn select_extension_instance_routes_by_stable_instance_id_when_email_is_unknown() {
        use std::os::unix::net::UnixListener;

        let dir = TempDir::new().unwrap();
        let manifest_dir = dir.path().join("native-hosts");
        let state_dir = dir.path().join("state");
        let _manifest_guard = EnvGuard::set("YOETZ_CHROME_NATIVE_MESSAGING_DIR", &manifest_dir);
        let _state_guard = EnvGuard::set("YOETZ_DIR", &state_dir);
        let paths = extension_paths().unwrap();
        fs::create_dir_all(&paths.instances_dir).unwrap();
        let work_socket = dir.path().join("work.sock");
        let personal_socket = dir.path().join("personal.sock");
        let _work_listener = UnixListener::bind(&work_socket).unwrap();
        let _personal_listener = UnixListener::bind(&personal_socket).unwrap();
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_work".to_string(),
                socket_path: work_socket.clone(),
                pid: process::id(),
                extension_instance_id: Some("ext_work".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: None,
                profile_id: None,
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 2,
            },
        );
        write_instance_fixture(
            &paths,
            ExtensionInstanceStatus {
                native_instance_id: "native_personal".to_string(),
                socket_path: personal_socket,
                pid: process::id(),
                extension_instance_id: Some("ext_personal".to_string()),
                extension_version: Some("0.4.0".to_string()),
                profile_email: None,
                profile_id: None,
                recipes: default_extension_recipes(),
                protocol_version: PROTOCOL_VERSION,
                last_seen_ms: 1,
            },
        );

        let selected = select_extension_instance(
            &paths,
            ExtensionInstanceSelector {
                extension_instance_id: Some("ext_work"),
                ..ExtensionInstanceSelector::default()
            },
        )
        .unwrap();

        assert_eq!(selected.native_instance_id, "native_work");
        assert_eq!(selected.socket_path, work_socket);
    }

    #[test]
    fn validate_bundle_rejects_oversized_files() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bundle.md");
        fs::write(&path, vec![b'x'; (MAX_BUNDLE_BYTES + 1) as usize]).unwrap();
        let err = validate_bundle_path(&path).unwrap_err();
        assert!(err
            .to_string()
            .contains("above chrome-extension-native limit"));
    }

    #[test]
    fn validate_bundle_canonicalizes_relative_path_before_job_start() {
        let cwd = env::current_dir().unwrap();
        let dir = tempfile::Builder::new()
            .prefix("yoetz-relative-bundle-")
            .tempdir_in(&cwd)
            .unwrap();
        let path = dir.path().join("bundle.md");
        fs::write(&path, "review me").unwrap();
        let relative = path.strip_prefix(&cwd).unwrap();

        let bundle = validate_bundle_path(relative).unwrap();

        assert!(bundle.path.is_absolute());
        assert_eq!(bundle.path, fs::canonicalize(&path).unwrap());
    }

    #[test]
    fn chunk_payload_uses_base64() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bundle.md");
        fs::write(&path, b"abc").unwrap();
        let chunks = chunk_payloads_for_file(&path, "bundle.md", "text/markdown").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0]["sequence"], 0);
        assert_eq!(chunks[0]["total_chunks"], 1);
        assert_eq!(chunks[0]["total_bytes"], 3);
        assert_eq!(chunks[0]["filename"], "bundle.md");
        assert_eq!(chunks[0]["bytes_base64"], "YWJj");
    }

    #[test]
    #[cfg(unix)]
    fn restored_ready_for_file_replays_upload_from_start() {
        let restored = ProtocolEnvelope::new(
            "job_progress",
            Some("job_restore".to_string()),
            Some("run_restore".to_string()),
            json!({
                "phase": "ready_for_file",
                "restored": true,
            }),
        );
        let fresh = ProtocolEnvelope::new(
            "job_progress",
            Some("job_fresh".to_string()),
            Some("run_fresh".to_string()),
            json!({
                "phase": "ready_for_file",
            }),
        );

        assert!(native_host_unix::should_replay_upload_from_start(&restored));
        assert!(!native_host_unix::should_replay_upload_from_start(&fresh));
    }

    #[test]
    fn canary_response_validation_requires_exact_ok() {
        validate_canary_response("OK\n").unwrap();
        let err = validate_canary_response("OK.").unwrap_err();
        assert!(err.to_string().contains("expected exact response `OK`"));
    }

    fn write_instance_fixture(paths: &ExtensionPaths, instance: ExtensionInstanceStatus) {
        let path = paths
            .instances_dir
            .join(format!("{}.json", instance.native_instance_id));
        fs::write(path, serde_json::to_string_pretty(&instance).unwrap()).unwrap();
    }

    fn write_extension_source_fixture(path: &Path, version: &str) {
        fs::create_dir_all(path.join("src")).unwrap();
        fs::create_dir_all(path.join("icons")).unwrap();
        fs::write(
            path.join("manifest.json"),
            format!(r#"{{"version":"{version}"}}"#),
        )
        .unwrap();
        fs::write(
            path.join("src").join("service-worker.js"),
            format!("service-worker:{version}"),
        )
        .unwrap();
        fs::write(
            path.join("src").join("content-script.js"),
            format!("content-script:{version}"),
        )
        .unwrap();
        fs::write(path.join("icons").join("icon-16.png"), b"icon").unwrap();
    }

    #[test]
    fn job_error_phase_is_terminal_only_after_side_effects() {
        let pre_effect = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_pre".to_string()),
            Some("run_pre".to_string()),
            json!({
                "message": "file input missing before upload",
                "phase": "upload",
                "side_effect_started": false,
            }),
        ));
        assert!(crate::chatgpt_recipe::terminal_fallback_phase(&pre_effect).is_none());

        let post_effect = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_post".to_string()),
            Some("run_post".to_string()),
            json!({
                "message": "send failed after upload",
                "phase": "send",
                "side_effect_started": true,
            }),
        ));
        assert_eq!(
            crate::chatgpt_recipe::terminal_fallback_phase(&post_effect),
            Some(ChatgptTransportPhase::Send)
        );
    }

    #[test]
    fn job_error_conversation_failures_surface_actionable_context() {
        let err = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_conv".to_string()),
            Some("run_conv".to_string()),
            json!({
                "code": "conversation_unavailable",
                "message": "ChatGPT conversation is unavailable",
                "phase": "upload",
                "side_effect_started": false,
                "requested_conversation_id": "conv-404",
                "current_url": "https://chatgpt.com/c/conv-404?_yoetz=run_conv",
                "inspect_command": "yoetz browser extension inspect --chatgpt --run-id run_conv",
            }),
        ));
        let text = format!("{err:#}");

        assert!(text.contains("requested conversation conv-404"));
        assert!(text.contains("current URL https://chatgpt.com/c/conv-404?_yoetz=run_conv"));
        assert!(text.contains("phase upload"));
        assert!(text.contains("yoetz browser extension inspect --chatgpt --run-id run_conv"));
        assert!(is_conversation_job_error(&err));
        assert!(format!(
            "{:#}",
            with_thread_conversation_recovery_hint(err, Some("review-pr-341"))
        )
        .contains("--thread review-pr-341 --fresh"));
    }

    #[test]
    fn job_error_non_conversation_failures_surface_inspect_context() {
        let err = job_error(ProtocolEnvelope::new(
            "job_error",
            Some("job_ready".to_string()),
            Some("run_ready".to_string()),
            json!({
                "code": "extension_error",
                "message": "Yoetz content script did not become ready in ChatGPT tab 920272522",
                "phase": "upload",
                "side_effect_started": true,
                "tab_id": 920272522,
                "inspect_command": "yoetz browser extension inspect --chatgpt --run-id run_ready",
            }),
        ));
        let text = format!("{err:#}");

        assert!(text.contains("Yoetz content script did not become ready"));
        assert!(text.contains("tab 920272522"));
        assert!(text.contains("phase upload"));
        assert!(text.contains("yoetz browser extension inspect --chatgpt --run-id run_ready"));
        assert!(!is_conversation_job_error(&err));
        assert_eq!(
            crate::chatgpt_recipe::terminal_fallback_phase(&err),
            Some(ChatgptTransportPhase::Upload)
        );
        assert!(!format!(
            "{:#}",
            with_thread_conversation_recovery_hint(err, Some("review-pr-341"))
        )
        .contains("--fresh"));
    }
}
