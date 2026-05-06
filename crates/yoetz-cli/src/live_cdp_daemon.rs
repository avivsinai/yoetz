use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yoetz_core::paths::home_dir;

const EMBEDDED_DAEMON: &str = include_str!("../assets/live-cdp-daemon.mjs");
const DAEMON_TOKEN_PATH_ANCHOR: &str =
    r#"var PID_PATH = path.join(BASE_DIR, "live-cdp-daemon.pid");"#;
const DAEMON_VERSION_ANCHOR: &str = "var YOETZ_DAEMON_VERSION = await computeDaemonVersion();";
const DAEMON_COMPUTE_VERSION_ANCHOR: &str = r#"async function computeDaemonVersion() {
  const source = await readFile(fileURLToPath(import.meta.url));
  return createHash("sha256").update(source).digest("hex");
}"#;
const DAEMON_REQUEST_GUARD_ANCHOR: &str = "  if (shuttingDown && request.type !== \"stop\") {";
const DAEMON_FILENAME: &str = "live-cdp-daemon.mjs";
const DAEMON_SOCKET_FILENAME: &str = "live-cdp-daemon.sock";
const DAEMON_PID_FILENAME: &str = "live-cdp-daemon.pid";
const DAEMON_LOCK_FILENAME: &str = "live-cdp-daemon.lock";
const DAEMON_LOG_FILENAME: &str = "live-cdp-daemon.log";
const DAEMON_TOKEN_FILENAME: &str = "live-cdp-daemon.token";
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_START_POLL_MS: u64 = 100;
// The child daemon enforces the script timeout exactly; the Rust parent keeps a
// short grace window so queued stdout/stderr and the final complete/error frame
// can still be read after script cancellation.
const RESPONSE_READ_TIMEOUT_GRACE_SECS: u64 = 20;
const DAEMON_OUTPUT_BUFFER_LIMIT_BYTES: usize = 1024 * 1024;
pub(crate) const YOETZ_LIVE_CDP_DAEMON_ENV: &str = "YOETZ_LIVE_CDP_DAEMON";
static HARDENED_DAEMON_SOURCE: OnceLock<String> = OnceLock::new();
static DAEMON_TOKEN_CACHE: OnceLock<Mutex<Option<CachedDaemonToken>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LiveCdpDaemonMode {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DaemonPaths {
    base_dir: PathBuf,
    daemon_path: PathBuf,
    socket_path: PathBuf,
    pid_path: PathBuf,
    lock_path: PathBuf,
    log_path: PathBuf,
    token_path: PathBuf,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DaemonStatus {
    version: Option<String>,
    pid: Option<u64>,
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DaemonTokenCacheKey {
    token_path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
    #[cfg(unix)]
    uid: u32,
    #[cfg(unix)]
    mode: u32,
}

#[derive(Clone, Eq, PartialEq)]
struct CachedDaemonToken {
    key: DaemonTokenCacheKey,
    token: String,
}

impl std::fmt::Debug for CachedDaemonToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CachedDaemonToken")
            .field("key", &self.key)
            .field("token", &"<redacted>")
            .finish()
    }
}

#[derive(Debug)]
struct DaemonConnectError(io::Error);

impl std::fmt::Display for DaemonConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for DaemonConnectError {}

#[derive(Debug)]
struct DaemonAuthError(String);

impl std::fmt::Display for DaemonAuthError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for DaemonAuthError {}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum DaemonResponse {
    #[serde(rename = "stdout")]
    Stdout { data: String },
    #[serde(rename = "stderr")]
    Stderr { data: String },
    #[serde(rename = "result")]
    Result { data: Value },
    #[serde(rename = "complete")]
    Complete { success: bool },
    #[serde(rename = "error")]
    Error { message: String },
}

pub(crate) fn is_enabled() -> bool {
    resolve_mode(env::var(YOETZ_LIVE_CDP_DAEMON_ENV).ok().as_deref()) == LiveCdpDaemonMode::Enabled
}

pub(crate) fn is_available() -> bool {
    is_enabled() && node_is_available()
}

pub(crate) fn run_script_connect(
    script: &str,
    timeout_secs: u64,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
) -> Result<String> {
    if !is_enabled() {
        return Err(anyhow!(
            "yoetz live-CDP daemon is disabled by {YOETZ_LIVE_CDP_DAEMON_ENV}=0"
        ));
    }
    ensure_daemon()?;
    send_execute_request(script, timeout_secs, browser_name, cdp_endpoint)
}

pub(crate) fn stop_live_cdp_daemon() -> Result<bool> {
    let paths = daemon_paths()?;
    ensure_private_runtime_dir(&paths)?;
    let Ok(mut stream) = connect_to_daemon(&paths) else {
        let terminated = terminate_daemon_from_pid(&paths)?;
        if !terminated || wait_until_daemon_down(&paths, Duration::from_secs(5)) {
            cleanup_stale_files(&paths);
        }
        return Ok(terminated);
    };
    let token = match read_daemon_token_for_request(&paths) {
        Ok(token) => token,
        Err(error) => {
            let terminated = terminate_daemon_from_pid(&paths)?;
            if terminated {
                let _ = wait_until_daemon_down(&paths, Duration::from_secs(5));
                cleanup_stale_files(&paths);
                return Ok(true);
            }
            return Err(error.context(format!(
                "read yoetz live-CDP daemon capability token from {}",
                paths.token_path.display()
            )));
        }
    };

    let request = json!({
        "id": request_id("stop"),
        "type": "stop",
        "version": daemon_version(),
        "capabilityToken": token,
    });
    write_request(&mut stream, &request).context("send yoetz live-CDP daemon stop request")?;
    let mut reader = BufReader::new(stream);
    let _ = read_until_complete(&mut reader, Duration::from_secs(5));

    if wait_until_daemon_down(&paths, Duration::from_secs(5)) {
        cleanup_stale_files(&paths);
        return Ok(true);
    }
    if terminate_daemon_from_pid(&paths)? {
        let _ = wait_until_daemon_down(&paths, Duration::from_secs(5));
        cleanup_stale_files(&paths);
    }
    Ok(true)
}

fn ensure_daemon() -> Result<()> {
    let paths = daemon_paths()?;
    ensure_private_runtime_dir(&paths)?;

    let _lock = acquire_spawn_lock(&paths)?;
    if daemon_is_current(&paths)? {
        return Ok(());
    }

    let _ = request_daemon_stop(&paths);
    let _ = terminate_daemon_from_pid(&paths);
    let _ = wait_until_daemon_down(&paths, Duration::from_secs(5));
    cleanup_stale_files(&paths);
    let _ = create_new_daemon_token(&paths)?;

    ensure_daemon_asset(&paths)?;
    spawn_daemon(&paths)?;

    let deadline = std::time::Instant::now() + DAEMON_START_TIMEOUT;
    while std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(DAEMON_START_POLL_MS));
        if daemon_is_current(&paths)? {
            return Ok(());
        }
    }
    Err(anyhow!(
        "yoetz live-CDP daemon failed to start within {} seconds; see {}",
        DAEMON_START_TIMEOUT.as_secs(),
        paths.log_path.display()
    ))
}

fn send_execute_request(
    script: &str,
    timeout_secs: u64,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
) -> Result<String> {
    let paths = daemon_paths()?;
    ensure_private_runtime_dir(&paths)?;
    let token = read_daemon_token_for_request(&paths)?;
    let mut stream = connect_to_daemon(&paths).context("connect to yoetz live-CDP daemon")?;
    let timeout =
        Duration::from_secs(timeout_secs.saturating_add(RESPONSE_READ_TIMEOUT_GRACE_SECS));
    set_stream_timeouts(&stream, timeout)?;

    let request = build_execute_request(script, timeout_secs, browser_name, cdp_endpoint, &token);
    write_request(&mut stream, &request).context("send yoetz live-CDP daemon execute request")?;
    let mut reader = BufReader::new(stream);
    match read_until_complete(&mut reader, timeout) {
        Err(error) if is_daemon_auth_error(&error) => {
            clear_daemon_token_cache();
            Err(error)
        }
        result => result,
    }
}

fn acquire_spawn_lock(paths: &DaemonPaths) -> Result<File> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock_path)
        .with_context(|| format!("open {}", paths.lock_path.display()))?;
    lock.lock_exclusive()
        .with_context(|| format!("lock {}", paths.lock_path.display()))?;
    Ok(lock)
}

fn daemon_is_current(paths: &DaemonPaths) -> Result<bool> {
    match request_daemon_status(paths) {
        Ok(status) => Ok(status.version.as_deref() == Some(daemon_version().as_str())),
        Err(error) if is_daemon_connect_error(&error) => Ok(false),
        Err(error) if is_daemon_auth_error(&error) => {
            clear_daemon_token_cache();
            Ok(false)
        }
        Err(first_error) => {
            thread::sleep(Duration::from_millis(DAEMON_START_POLL_MS));
            match request_daemon_status(paths) {
                Ok(status) => Ok(status.version.as_deref() == Some(daemon_version().as_str())),
                Err(error) if is_daemon_connect_error(&error) => Ok(false),
                Err(error) if is_daemon_auth_error(&error) => {
                    clear_daemon_token_cache();
                    Ok(false)
                }
                Err(error) => Err(error.context(format!(
                    "yoetz live-CDP daemon status failed after retry; first error: {first_error:#}"
                ))),
            }
        }
    }
}

fn request_daemon_status(paths: &DaemonPaths) -> Result<DaemonStatus> {
    let token = read_daemon_token_for_request(paths)?;
    let mut stream = connect_to_daemon(paths)
        .map_err(DaemonConnectError)
        .context("connect to yoetz live-CDP daemon")?;
    set_stream_timeouts(&stream, Duration::from_secs(5))?;
    let request = json!({
        "id": request_id("status"),
        "type": "status",
        "version": daemon_version(),
        "capabilityToken": token,
    });
    write_request(&mut stream, &request).context("send yoetz live-CDP daemon status request")?;
    let mut reader = BufReader::new(stream);
    read_status_until_complete(&mut reader, Duration::from_secs(5))
}

fn is_daemon_connect_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<DaemonConnectError>().is_some())
}

fn is_daemon_auth_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.downcast_ref::<DaemonAuthError>().is_some())
}

fn request_daemon_stop(paths: &DaemonPaths) -> Result<()> {
    let token = read_daemon_token_for_request(paths)?;
    let mut stream = connect_to_daemon(paths).context("connect to yoetz live-CDP daemon")?;
    set_stream_timeouts(&stream, Duration::from_secs(5))?;
    let request = json!({
        "id": request_id("stop"),
        "type": "stop",
        "version": daemon_version(),
        "capabilityToken": token,
    });
    write_request(&mut stream, &request).context("send yoetz live-CDP daemon stop request")?;
    let mut reader = BufReader::new(stream);
    let _ = read_until_complete(&mut reader, Duration::from_secs(5));
    Ok(())
}

fn read_status_until_complete<R: BufRead>(
    reader: &mut R,
    timeout: Duration,
) -> Result<DaemonStatus> {
    let started = std::time::Instant::now();
    let mut status = DaemonStatus {
        version: None,
        pid: None,
    };
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!("yoetz live-CDP daemon status request timed out"));
        }

        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .context("read yoetz live-CDP daemon status response")?;
        if bytes_read == 0 {
            return Err(anyhow!(
                "yoetz live-CDP daemon connection closed unexpectedly"
            ));
        }
        let response: DaemonResponse = serde_json::from_str(line.trim_end())
            .with_context(|| format!("parse yoetz live-CDP daemon status response: {line}"))?;
        match response {
            DaemonResponse::Result { data } => {
                status.version = data
                    .get("version")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                status.pid = data.get("pid").and_then(Value::as_u64);
            }
            DaemonResponse::Complete { success } if success => return Ok(status),
            DaemonResponse::Complete { .. } => {
                return Err(anyhow!("yoetz live-CDP daemon status failed"));
            }
            DaemonResponse::Error { message } => {
                if is_daemon_unauthorized_message(&message) {
                    return Err(DaemonAuthError(message).into());
                }
                return Err(anyhow!("yoetz live-CDP daemon status failed: {message}"));
            }
            DaemonResponse::Stdout { .. } | DaemonResponse::Stderr { .. } => {}
        }
    }
}

fn read_until_complete<R: BufRead>(reader: &mut R, timeout: Duration) -> Result<String> {
    let started = std::time::Instant::now();
    let mut stdout = String::new();
    let mut stderr = String::new();
    loop {
        if started.elapsed() > timeout {
            return Err(anyhow!(
                "yoetz live-CDP daemon timed out after {}s while waiting for script output",
                timeout.as_secs()
            ));
        }

        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .context("read yoetz live-CDP daemon response")?;
        if bytes_read == 0 {
            return Err(anyhow!(
                "yoetz live-CDP daemon connection closed unexpectedly"
            ));
        }
        let response: DaemonResponse = serde_json::from_str(line.trim_end())
            .with_context(|| format!("parse yoetz live-CDP daemon response: {line}"))?;
        match response {
            DaemonResponse::Stdout { data } => append_bounded_output(&mut stdout, &data),
            DaemonResponse::Stderr { data } => {
                eprint!("{data}");
                append_bounded_output(&mut stderr, &data);
            }
            DaemonResponse::Result { data } => {
                if !data.is_null() {
                    append_bounded_output(&mut stdout, &data.to_string());
                    append_bounded_output(&mut stdout, "\n");
                }
            }
            DaemonResponse::Complete { success } => {
                if success {
                    return Ok(stdout);
                }
                return Err(anyhow!(
                    "yoetz live-CDP daemon reported incomplete execution"
                ));
            }
            DaemonResponse::Error { message } => {
                if is_daemon_unauthorized_message(&message) {
                    return Err(DaemonAuthError(message).into());
                }
                if stderr.trim().is_empty() {
                    return Err(anyhow!("yoetz live-CDP script failed: {message}"));
                }
                return Err(anyhow!(
                    "yoetz live-CDP script failed: {message}\nstderr:\n{stderr}"
                ));
            }
        }
    }
}

/// Append daemon output while keeping only the last 1 MiB plus a truncation marker.
fn append_bounded_output(buffer: &mut String, data: &str) {
    buffer.push_str(data);
    if buffer.len() <= DAEMON_OUTPUT_BUFFER_LIMIT_BYTES {
        return;
    }

    let marker = format!(
        "[truncated {} bytes]\n",
        buffer
            .len()
            .saturating_sub(DAEMON_OUTPUT_BUFFER_LIMIT_BYTES)
    );
    let keep_limit = DAEMON_OUTPUT_BUFFER_LIMIT_BYTES.saturating_sub(marker.len());
    let target_drop = buffer.len().saturating_sub(keep_limit);
    let drop_at = buffer
        .char_indices()
        .map(|(index, _)| index)
        .find(|index| *index >= target_drop)
        .unwrap_or(target_drop);
    buffer.drain(..drop_at);
    buffer.insert_str(0, &marker);
}

fn build_execute_request(
    script: &str,
    timeout_secs: u64,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
    capability_token: &str,
) -> Value {
    let request = json!({
        "id": request_id("execute"),
        "type": "execute",
        "version": daemon_version(),
        "capabilityToken": capability_token,
        "browser": browser_name.unwrap_or("default"),
        "script": script,
        "connect": cdp_endpoint.unwrap_or("auto"),
        "timeoutMs": timeout_secs.saturating_mul(1000),
    });
    request
}

fn daemon_version() -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(daemon_source().as_bytes());
    hex::encode(hasher.finalize())
}

fn daemon_source() -> &'static str {
    HARDENED_DAEMON_SOURCE
        .get_or_init(build_hardened_daemon_source)
        .as_str()
}

fn build_hardened_daemon_source() -> String {
    let source = replace_once(
        EMBEDDED_DAEMON,
        DAEMON_TOKEN_PATH_ANCHOR,
        r#"var PID_PATH = path.join(BASE_DIR, "live-cdp-daemon.pid");
  var TOKEN_PATH = path.join(BASE_DIR, "live-cdp-daemon.token");"#,
    );
    let source = replace_once(
        &source,
        DAEMON_VERSION_ANCHOR,
        "var YOETZ_DAEMON_VERSION = await computeDaemonVersion();\nvar YOETZ_DAEMON_CAPABILITY_TOKEN = await readDaemonCapabilityToken();",
    );
    let source = replace_once(
        &source,
        DAEMON_COMPUTE_VERSION_ANCHOR,
        r#"async function computeDaemonVersion() {
  const source = await readFile(fileURLToPath(import.meta.url));
  return createHash("sha256").update(source).digest("hex");
}
async function readDaemonCapabilityToken() {
  const token = (await readFile(TOKEN_PATH, "utf8")).trim();
  if (!token) {
    throw new Error(`yoetz live-cdp daemon capability token is empty: ${TOKEN_PATH}`);
  }
  return token;
}
function requestHasValidCapabilityToken(line) {
  try {
    const value = JSON.parse(line);
    return typeof value.capabilityToken === "string" && value.capabilityToken === YOETZ_DAEMON_CAPABILITY_TOKEN;
  } catch {
    return false;
  }
}"#,
    );
    replace_once(
        &source,
        DAEMON_REQUEST_GUARD_ANCHOR,
        "  if (!requestHasValidCapabilityToken(line)) {\n    await writeMessage(socket, { id: request.id, type: \"error\", message: \"Unauthorized yoetz live-CDP daemon request: missing or invalid capability token\" });\n    return;\n  }\n  if (shuttingDown && request.type !== \"stop\") {",
    )
}

fn replace_once(source: &str, needle: &str, replacement: &str) -> String {
    if !source.contains(needle) {
        panic!("embedded live-CDP daemon hardening anchor was not found: {needle}");
    }
    source.replacen(needle, replacement, 1)
}

fn request_id(prefix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    format!("{prefix}-{timestamp}-{}", std::process::id())
}

fn write_request<W: Write>(writer: &mut W, request: &Value) -> io::Result<()> {
    let payload = serde_json::to_string(request)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    writer.write_all(payload.as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}

fn ensure_daemon_asset(paths: &DaemonPaths) -> Result<()> {
    ensure_private_runtime_dir(paths)?;
    sync_text_file(&paths.daemon_path, daemon_source())
        .with_context(|| format!("write {}", paths.daemon_path.display()))?;
    Ok(())
}

fn spawn_daemon(paths: &DaemonPaths) -> Result<()> {
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log_path)
        .with_context(|| format!("open {}", paths.log_path.display()))?;
    let log_for_stderr = log
        .try_clone()
        .with_context(|| format!("clone {}", paths.log_path.display()))?;
    let mut process = Command::new("node");
    process.arg(&paths.daemon_path);
    process.current_dir(&paths.base_dir);
    process.stdin(Stdio::null());
    process.stdout(Stdio::from(log));
    process.stderr(Stdio::from(log_for_stderr));
    process
        .spawn()
        .with_context(|| "failed to spawn Node for yoetz live-CDP daemon")?;
    Ok(())
}

fn sync_text_file(path: &Path, contents: &str) -> Result<()> {
    let needs_update = match fs::read_to_string(path) {
        Ok(existing) => existing != contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => true,
        Err(error) => return Err(error.into()),
    };
    if needs_update {
        fs::write(path, contents)?;
    }
    Ok(())
}

#[cfg(unix)]
fn ensure_private_runtime_dir(paths: &DaemonPaths) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    match fs::symlink_metadata(&paths.base_dir) {
        Ok(metadata) => validate_private_runtime_dir_metadata(paths, &metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700);
            match builder.create(&paths.base_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(error)
                        .with_context(|| format!("create {}", paths.base_dir.display()));
                }
            }
            fs::set_permissions(&paths.base_dir, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 700 {}", paths.base_dir.display()))?;
            let metadata = fs::symlink_metadata(&paths.base_dir)
                .with_context(|| format!("inspect {}", paths.base_dir.display()))?;
            validate_private_runtime_dir_metadata(paths, &metadata)
        }
        Err(error) => Err(error).with_context(|| format!("inspect {}", paths.base_dir.display())),
    }
}

#[cfg(unix)]
fn validate_private_runtime_dir_metadata(
    paths: &DaemonPaths,
    metadata: &fs::Metadata,
) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon directory {}: path is a symlink; remove it and create a private directory with `mkdir -m 700 {}`",
            paths.base_dir.display(),
            paths.base_dir.display()
        ));
    }
    if !metadata.is_dir() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon directory {}: path is not a directory; remove it and create a private directory with `mkdir -m 700 {}`",
            paths.base_dir.display(),
            paths.base_dir.display()
        ));
    }

    let owner_uid = metadata.uid();
    let current_uid = current_uid();
    if owner_uid != current_uid {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon directory {}: owned by uid {}, current uid is {}; fix ownership with `chown $(id -un) {}` and `chmod 700 {}`",
            paths.base_dir.display(),
            owner_uid,
            current_uid,
            paths.base_dir.display(),
            paths.base_dir.display()
        ));
    }

    let mode = metadata.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        fs::set_permissions(&paths.base_dir, fs::Permissions::from_mode(0o700))
            .with_context(|| format!("chmod 700 {}", paths.base_dir.display()))?;
        let metadata = fs::symlink_metadata(&paths.base_dir)
            .with_context(|| format!("inspect {}", paths.base_dir.display()))?;
        let tightened_mode = metadata.permissions().mode() & 0o777;
        if tightened_mode & 0o077 != 0 {
            return Err(anyhow!(
                "refusing to use yoetz live-CDP daemon directory {}: permissions are {:03o}; run `chmod 700 {}` so group and world cannot access the daemon socket",
                paths.base_dir.display(),
                tightened_mode,
                paths.base_dir.display()
            ));
        }
    }

    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_runtime_dir(paths: &DaemonPaths) -> Result<()> {
    fs::create_dir_all(&paths.base_dir)
        .with_context(|| format!("create {}", paths.base_dir.display()))?;
    Ok(())
}

fn create_new_daemon_token(paths: &DaemonPaths) -> Result<String> {
    clear_daemon_token_cache();
    let token = random_daemon_token();
    match fs::symlink_metadata(&paths.token_path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() {
                return Err(anyhow!(
                    "refusing to replace yoetz live-CDP daemon token {}: path is a symlink",
                    paths.token_path.display()
                ));
            }
            if !metadata.is_file() {
                return Err(anyhow!(
                    "refusing to replace yoetz live-CDP daemon token {}: path is not a regular file",
                    paths.token_path.display()
                ));
            }
            fs::remove_file(&paths.token_path)
                .with_context(|| format!("remove {}", paths.token_path.display()))?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("inspect {}", paths.token_path.display()));
        }
    }

    write_new_daemon_token_file(&paths.token_path, &token)?;
    Ok(token)
}

fn clear_daemon_token_cache() {
    if let Some(cache) = DAEMON_TOKEN_CACHE.get() {
        *cache.lock().expect("daemon token cache mutex poisoned") = None;
    }
}

#[cfg(unix)]
fn write_new_daemon_token_file(path: &Path, token: &str) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(token.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush {}", path.display()))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
fn write_new_daemon_token_file(path: &Path, token: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .with_context(|| format!("create {}", path.display()))?;
    file.write_all(token.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("flush {}", path.display()))?;
    Ok(())
}

fn read_daemon_token_for_request(paths: &DaemonPaths) -> Result<String> {
    read_daemon_token_for_request_cached(paths)
        .map_err(|error| DaemonAuthError(format!("{error:#}")).into())
}

fn read_daemon_token_for_request_cached(paths: &DaemonPaths) -> Result<String> {
    let key = daemon_token_cache_key(paths)?;
    let cache = DAEMON_TOKEN_CACHE.get_or_init(|| Mutex::new(None));
    if let Some(cached) = cache
        .lock()
        .expect("daemon token cache mutex poisoned")
        .as_ref()
        .filter(|cached| cached.key == key)
        .cloned()
    {
        return Ok(cached.token);
    }

    let token = read_daemon_token(paths)?;
    *cache.lock().expect("daemon token cache mutex poisoned") = Some(CachedDaemonToken {
        key,
        token: token.clone(),
    });
    Ok(token)
}

fn daemon_token_cache_key(paths: &DaemonPaths) -> Result<DaemonTokenCacheKey> {
    let metadata = fs::symlink_metadata(&paths.token_path)
        .with_context(|| format!("inspect {}", paths.token_path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: path is a symlink",
            paths.token_path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: path is not a regular file",
            paths.token_path.display()
        ));
    }
    Ok(DaemonTokenCacheKey {
        token_path: paths.token_path.clone(),
        modified: metadata.modified().ok(),
        len: metadata.len(),
        #[cfg(unix)]
        uid: {
            use std::os::unix::fs::MetadataExt;
            metadata.uid()
        },
        #[cfg(unix)]
        mode: {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o777
        },
    })
}

fn read_daemon_token(paths: &DaemonPaths) -> Result<String> {
    let token = read_validated_daemon_token_file(paths)?.trim().to_owned();
    if token.is_empty() {
        return Err(anyhow!(
            "yoetz live-CDP daemon capability token is empty: {}",
            paths.token_path.display()
        ));
    }
    Ok(token)
}

#[cfg(unix)]
fn read_validated_daemon_token_file(paths: &DaemonPaths) -> Result<String> {
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&paths.token_path)
        .with_context(|| format!("open {}", paths.token_path.display()))?;
    validate_open_daemon_token_file(paths, &file)?;
    let mut token = String::new();
    file.read_to_string(&mut token)
        .with_context(|| format!("read {}", paths.token_path.display()))?;
    Ok(token)
}

#[cfg(unix)]
fn validate_open_daemon_token_file(paths: &DaemonPaths, file: &File) -> Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = file
        .metadata()
        .with_context(|| format!("inspect {}", paths.token_path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: path is not a regular file",
            paths.token_path.display()
        ));
    }
    let owner_uid = metadata.uid();
    let current_uid = current_uid();
    if owner_uid != current_uid {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: owned by uid {}, current uid is {}",
            paths.token_path.display(),
            owner_uid,
            current_uid
        ));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != 0o600 {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: permissions are {:03o}; run `chmod 600 {}`",
            paths.token_path.display(),
            mode,
            paths.token_path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn read_validated_daemon_token_file(paths: &DaemonPaths) -> Result<String> {
    let metadata = fs::metadata(&paths.token_path)
        .with_context(|| format!("inspect {}", paths.token_path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "refusing to use yoetz live-CDP daemon capability token {}: path is not a regular file",
            paths.token_path.display()
        ));
    }
    fs::read_to_string(&paths.token_path)
        .with_context(|| format!("read {}", paths.token_path.display()))
}

fn random_daemon_token() -> String {
    let token: [u8; 32] = rand::random();
    hex::encode(token)
}

fn is_daemon_unauthorized_message(message: &str) -> bool {
    message.contains("Unauthorized yoetz live-CDP daemon request")
}

#[cfg(unix)]
#[allow(unsafe_code)]
fn current_uid() -> u32 {
    unsafe extern "C" {
        fn getuid() -> std::os::raw::c_uint;
    }
    unsafe { getuid() as u32 }
}

fn daemon_paths() -> Result<DaemonPaths> {
    let home = home_dir()
        .ok_or_else(|| anyhow!("could not determine home directory for yoetz live-CDP daemon"))?;
    Ok(daemon_paths_for_base_dir(&live_cdp_runtime_base_dir(
        &home,
    )?))
}

#[cfg(test)]
fn daemon_paths_for_home(home: &Path) -> DaemonPaths {
    daemon_paths_for_base_dir(&home.join(".yoetz"))
}

#[cfg(unix)]
fn daemon_paths_for_runtime_dir(runtime_dir: &Path) -> DaemonPaths {
    daemon_paths_for_base_dir(&runtime_dir.join("yoetz"))
}

fn daemon_paths_for_base_dir(base_dir: &Path) -> DaemonPaths {
    DaemonPaths {
        daemon_path: base_dir.join(DAEMON_FILENAME),
        socket_path: base_dir.join(DAEMON_SOCKET_FILENAME),
        pid_path: base_dir.join(DAEMON_PID_FILENAME),
        lock_path: base_dir.join(DAEMON_LOCK_FILENAME),
        log_path: base_dir.join(DAEMON_LOG_FILENAME),
        token_path: base_dir.join(DAEMON_TOKEN_FILENAME),
        base_dir: base_dir.to_path_buf(),
    }
}

fn live_cdp_runtime_base_dir(home: &Path) -> Result<PathBuf> {
    #[cfg(unix)]
    if let Some(runtime_dir) = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Ok(daemon_paths_for_runtime_dir(&runtime_dir).base_dir);
    }

    Ok(home.join(".yoetz"))
}

fn cleanup_stale_files(paths: &DaemonPaths) {
    let _ = fs::remove_file(&paths.pid_path);
    let _ = fs::remove_file(&paths.socket_path);
    let _ = fs::remove_file(&paths.token_path);
    clear_daemon_token_cache();
}

fn wait_until_daemon_down(paths: &DaemonPaths, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    while std::time::Instant::now() < deadline {
        if connect_to_daemon(paths).is_err() {
            return true;
        }
        thread::sleep(Duration::from_millis(DAEMON_START_POLL_MS));
    }
    false
}

fn terminate_daemon_from_pid(paths: &DaemonPaths) -> Result<bool> {
    let Some(pid) = read_daemon_pid(&paths.pid_path)? else {
        return Ok(false);
    };
    if !process_is_alive(pid) {
        return Ok(false);
    }
    if !process_looks_like_live_cdp_daemon(pid) {
        return Ok(false);
    }
    let output = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .output()
        .with_context(|| format!("terminate yoetz live-CDP daemon pid {pid}"))?;
    Ok(output.status.success())
}

fn read_daemon_pid(path: &Path) -> Result<Option<u32>> {
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(contents.trim().parse::<u32>().ok())
}

fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn process_looks_like_live_cdp_daemon(pid: u32) -> bool {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output();
    let Ok(output) = output else {
        return false;
    };
    if !output.status.success() {
        return false;
    }
    String::from_utf8_lossy(&output.stdout).contains("live-cdp-daemon.mjs")
}

fn resolve_mode(value: Option<&str>) -> LiveCdpDaemonMode {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        Some("0" | "false" | "no" | "off" | "disabled") => LiveCdpDaemonMode::Disabled,
        _ => LiveCdpDaemonMode::Enabled,
    }
}

fn node_is_available() -> bool {
    let Ok(mut child) = Command::new("node")
        .args([
            "-e",
            "process.exit(typeof WebSocket === 'function' && typeof fetch === 'function' && typeof AbortSignal?.timeout === 'function' ? 0 : 1)",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    wait_for_child_success(&mut child, Duration::from_secs(2))
}

fn wait_for_child_success(child: &mut Child, timeout: Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(DAEMON_START_POLL_MS));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Err(_) => return false,
        }
    }
}

#[cfg(unix)]
fn connect_to_daemon(paths: &DaemonPaths) -> io::Result<UnixStream> {
    let stream = UnixStream::connect(&paths.socket_path)?;
    verify_daemon_peer(&stream)?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[allow(unsafe_code)]
fn verify_daemon_peer(stream: &UnixStream) -> io::Result<()> {
    let mut creds = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::addr_of_mut!(creds).cast(),
            std::ptr::addr_of_mut!(len),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if creds.uid != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing yoetz live-CDP daemon connection from uid {}; current uid is {}",
                creds.uid,
                current_uid()
            ),
        ));
    }
    Ok(())
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "dragonfly",
    target_os = "netbsd",
    target_os = "openbsd"
))]
#[allow(unsafe_code)]
fn verify_daemon_peer(stream: &UnixStream) -> io::Result<()> {
    let mut uid = 0u32;
    let mut gid = 0u32;
    unsafe extern "C" {
        fn getpeereid(
            socket: std::os::raw::c_int,
            euid: *mut std::os::raw::c_uint,
            egid: *mut std::os::raw::c_uint,
        ) -> std::os::raw::c_int;
    }
    let result = unsafe {
        getpeereid(
            stream.as_raw_fd(),
            std::ptr::addr_of_mut!(uid),
            std::ptr::addr_of_mut!(gid),
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    if uid != current_uid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing yoetz live-CDP daemon connection from uid {uid}; current uid is {}",
                current_uid()
            ),
        ));
    }
    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "dragonfly",
        target_os = "netbsd",
        target_os = "openbsd"
    ))
))]
fn verify_daemon_peer(_stream: &UnixStream) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_stream_timeouts(stream: &UnixStream, timeout: Duration) -> io::Result<()> {
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(())
}

#[cfg(not(unix))]
fn connect_to_daemon(_paths: &DaemonPaths) -> io::Result<std::io::Cursor<Vec<u8>>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "yoetz live-CDP daemon is not supported on this platform yet",
    ))
}

#[cfg(not(unix))]
fn set_stream_timeouts(_stream: &std::io::Cursor<Vec<u8>>, _timeout: Duration) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_mode_defaults_enabled_and_honors_disable_values() {
        assert_eq!(resolve_mode(None), LiveCdpDaemonMode::Enabled);
        assert_eq!(resolve_mode(Some("1")), LiveCdpDaemonMode::Enabled);
        assert_eq!(resolve_mode(Some("0")), LiveCdpDaemonMode::Disabled);
        assert_eq!(resolve_mode(Some("false")), LiveCdpDaemonMode::Disabled);
        assert_eq!(resolve_mode(Some("off")), LiveCdpDaemonMode::Disabled);
    }

    #[test]
    fn daemon_paths_live_under_yoetz_home() {
        let paths = daemon_paths_for_home(Path::new("/tmp/example-home"));
        assert_eq!(paths.base_dir, PathBuf::from("/tmp/example-home/.yoetz"));
        assert_eq!(
            paths.daemon_path,
            PathBuf::from("/tmp/example-home/.yoetz/live-cdp-daemon.mjs")
        );
        assert_eq!(
            paths.socket_path,
            PathBuf::from("/tmp/example-home/.yoetz/live-cdp-daemon.sock")
        );
        assert_eq!(
            paths.token_path,
            PathBuf::from("/tmp/example-home/.yoetz/live-cdp-daemon.token")
        );
    }

    #[cfg(unix)]
    #[test]
    fn daemon_paths_can_use_xdg_runtime_dir() {
        let paths = daemon_paths_for_runtime_dir(Path::new("/tmp/example-runtime"));
        assert_eq!(paths.base_dir, PathBuf::from("/tmp/example-runtime/yoetz"));
        assert_eq!(
            paths.socket_path,
            PathBuf::from("/tmp/example-runtime/yoetz/live-cdp-daemon.sock")
        );
    }

    #[test]
    fn execute_request_preserves_script_timeout_browser_and_endpoint() {
        let request = build_execute_request(
            "console.log('ok')",
            45,
            Some("yoetz-chatgpt"),
            Some("ws://127.0.0.1:9222/devtools/browser/test"),
            "test-token",
        );
        assert_eq!(request["type"], "execute");
        assert_eq!(request["version"], daemon_version());
        assert_eq!(request["capabilityToken"], "test-token");
        assert_eq!(request["browser"], "yoetz-chatgpt");
        assert_eq!(request["script"], "console.log('ok')");
        assert_eq!(request["timeoutMs"], 45_000);
        assert_eq!(
            request["connect"],
            "ws://127.0.0.1:9222/devtools/browser/test"
        );
    }

    #[test]
    fn execute_request_uses_auto_connect_without_endpoint() {
        let request = build_execute_request("await browser.listPages()", 10, None, None, "token");
        assert_eq!(request["browser"], "default");
        assert_eq!(request["connect"], "auto");
        assert_eq!(request["timeoutMs"], 10_000);
        assert_eq!(request["capabilityToken"], "token");
    }

    #[test]
    fn embedded_bundle_keeps_chrome147_matcher_phrases() {
        for phrase in [
            "Target.getTargets",
            "initializing live CDP browser",
            "remote-debugging consent",
        ] {
            assert!(
                EMBEDDED_DAEMON.contains(phrase),
                "embedded daemon should contain matcher phrase: {phrase}"
            );
        }
    }

    #[test]
    fn hardened_daemon_source_requires_capability_token() {
        let source = daemon_source();
        assert!(source.contains("live-cdp-daemon.token"));
        assert!(source.contains("YOETZ_DAEMON_CAPABILITY_TOKEN"));
        assert!(source.contains("requestHasValidCapabilityToken"));
        assert!(source.contains("Unauthorized yoetz live-CDP daemon request"));
    }

    #[test]
    fn embedded_daemon_contains_hardening_patch_anchors() {
        for anchor in [
            DAEMON_TOKEN_PATH_ANCHOR,
            DAEMON_VERSION_ANCHOR,
            DAEMON_COMPUTE_VERSION_ANCHOR,
            DAEMON_REQUEST_GUARD_ANCHOR,
        ] {
            assert!(
                EMBEDDED_DAEMON.contains(anchor),
                "embedded daemon hardening anchor should still exist: {anchor}"
            );
        }
    }

    #[test]
    fn append_bounded_output_marks_truncation() {
        let mut buffer = "prefix".to_string();
        append_bounded_output(
            &mut buffer,
            &"x".repeat(DAEMON_OUTPUT_BUFFER_LIMIT_BYTES + 32),
        );
        assert!(buffer.starts_with("[truncated "));
        assert!(buffer.contains(" bytes]\n"));
        assert!(buffer.len() <= DAEMON_OUTPUT_BUFFER_LIMIT_BYTES);
    }

    #[test]
    fn daemon_asset_sync_writes_and_skips_unchanged_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.mjs");
        sync_text_file(&path, "one").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "one");
        sync_text_file(&path, "one").unwrap();
        sync_text_file(&path, "two").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
    }

    #[cfg(unix)]
    #[test]
    fn private_runtime_dir_tightens_world_accessible_existing_dir() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let paths = daemon_paths_for_home(dir.path());
        fs::create_dir_all(&paths.base_dir).unwrap();
        fs::set_permissions(&paths.base_dir, fs::Permissions::from_mode(0o755)).unwrap();

        ensure_private_runtime_dir(&paths).unwrap();
        let metadata = fs::symlink_metadata(&paths.base_dir).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn private_runtime_dir_rejects_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let paths = daemon_paths_for_home(dir.path());
        let target = dir.path().join("target");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &paths.base_dir).unwrap();

        let error = ensure_private_runtime_dir(&paths).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("path is a symlink"));
        assert!(message.contains("mkdir -m 700"));
    }

    #[test]
    fn daemon_token_file_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let paths = daemon_paths_for_home(dir.path());
        ensure_private_runtime_dir(&paths).unwrap();

        let token = create_new_daemon_token(&paths).unwrap();
        assert_eq!(token.len(), 64);
        assert_eq!(read_daemon_token(&paths).unwrap(), token);

        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};

            let metadata = fs::symlink_metadata(&paths.token_path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(metadata.uid(), current_uid());
        }
    }

    #[test]
    fn cached_daemon_token_debug_redacts_secret() {
        let fake_secret = "redaction-test-secret-value";
        let cached = CachedDaemonToken {
            key: DaemonTokenCacheKey {
                token_path: PathBuf::from("/tmp/live-cdp-daemon.token"),
                modified: None,
                len: 64,
                #[cfg(unix)]
                uid: current_uid(),
                #[cfg(unix)]
                mode: 0o600,
            },
            token: fake_secret.to_string(),
        };
        let debug = format!("{cached:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains(fake_secret));
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn bundled_daemon_status_round_trips_under_isolated_home() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path().as_os_str());
        let _runtime = EnvVarGuard::remove("XDG_RUNTIME_DIR");
        let _enabled = EnvVarGuard::remove(YOETZ_LIVE_CDP_DAEMON_ENV);

        let paths = daemon_paths_for_home(dir.path());
        let result = (|| -> Result<()> {
            ensure_daemon()?;
            let token_metadata = fs::symlink_metadata(&paths.token_path)
                .with_context(|| format!("inspect {}", paths.token_path.display()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if token_metadata.permissions().mode() & 0o777 != 0o600 {
                    return Err(anyhow!(
                        "unexpected daemon token permissions: {:03o}",
                        token_metadata.permissions().mode() & 0o777
                    ));
                }
            }
            let status = request_daemon_status(&paths)?;
            if status.version.as_deref() != Some(daemon_version().as_str()) {
                return Err(anyhow!("unexpected daemon status version: {status:?}"));
            }
            if status.pid.is_none() {
                return Err(anyhow!("status response did not include pid"));
            }
            Ok(())
        })();
        let stopped = stop_live_cdp_daemon();

        result.unwrap();
        stopped.unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn bundled_daemon_rejects_status_without_capability_token() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path().as_os_str());
        let _runtime = EnvVarGuard::remove("XDG_RUNTIME_DIR");
        let _enabled = EnvVarGuard::remove(YOETZ_LIVE_CDP_DAEMON_ENV);

        let paths = daemon_paths_for_home(dir.path());
        let result = (|| -> Result<()> {
            ensure_daemon()?;
            let mut stream = connect_to_daemon(&paths)?;
            set_stream_timeouts(&stream, Duration::from_secs(5))?;
            let request = json!({
                "id": request_id("status"),
                "type": "status",
                "version": daemon_version(),
            });
            write_request(&mut stream, &request)?;

            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            let response: DaemonResponse = serde_json::from_str(line.trim_end())?;
            match response {
                DaemonResponse::Error { message } => {
                    if !is_daemon_unauthorized_message(&message) {
                        return Err(anyhow!("unexpected daemon error: {message}"));
                    }
                }
                other => return Err(anyhow!("unexpected daemon response: {other:?}")),
            }
            Ok(())
        })();
        let stopped = stop_live_cdp_daemon();

        result.unwrap();
        stopped.unwrap();
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn stale_socket_file_is_cleaned_and_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path().as_os_str());
        let _runtime = EnvVarGuard::remove("XDG_RUNTIME_DIR");
        let _enabled = EnvVarGuard::remove(YOETZ_LIVE_CDP_DAEMON_ENV);
        let paths = daemon_paths_for_home(dir.path());
        fs::create_dir_all(&paths.base_dir).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&paths.base_dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(&paths.socket_path, "not a socket").unwrap();

        let result = (|| -> Result<()> {
            ensure_daemon()?;
            let status = request_daemon_status(&paths)?;
            if status.version.as_deref() != Some(daemon_version().as_str()) {
                return Err(anyhow!("unexpected daemon status version: {status:?}"));
            }
            Ok(())
        })();
        let stopped = stop_live_cdp_daemon();

        result.unwrap();
        stopped.unwrap();
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &std::ffi::OsStr) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::set_var(key, value);
            }
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = env::var_os(key);
            #[allow(unsafe_code)]
            unsafe {
                env::remove_var(key);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            #[allow(unsafe_code)]
            unsafe {
                if let Some(previous) = &self.previous {
                    env::set_var(self.key, previous);
                } else {
                    env::remove_var(self.key);
                }
            }
        }
    }
}
