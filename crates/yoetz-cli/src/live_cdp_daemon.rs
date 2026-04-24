use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::env;
use std::fs;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use yoetz_core::paths::home_dir;

const EMBEDDED_DAEMON: &str = include_str!("../assets/live-cdp-daemon.mjs");
const DAEMON_FILENAME: &str = "live-cdp-daemon.mjs";
const DAEMON_SOCKET_FILENAME: &str = "live-cdp-daemon.sock";
const DAEMON_PID_FILENAME: &str = "live-cdp-daemon.pid";
const DAEMON_LOCK_FILENAME: &str = "live-cdp-daemon.lock";
const DAEMON_LOG_FILENAME: &str = "live-cdp-daemon.log";
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_START_POLL_MS: u64 = 100;
// The child daemon enforces the script timeout exactly; the Rust parent keeps a
// short grace window so queued stdout/stderr and the final complete/error frame
// can still be read after script cancellation.
const RESPONSE_READ_TIMEOUT_GRACE_SECS: u64 = 20;
pub(crate) const YOETZ_LIVE_CDP_DAEMON_ENV: &str = "YOETZ_LIVE_CDP_DAEMON";

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
}

#[derive(Debug, Clone, Eq, PartialEq)]
struct DaemonStatus {
    version: Option<String>,
    pid: Option<u64>,
}

#[derive(Debug)]
struct DaemonConnectError(io::Error);

impl std::fmt::Display for DaemonConnectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl std::error::Error for DaemonConnectError {}

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
    let Ok(mut stream) = connect_to_daemon(&paths) else {
        let terminated = terminate_daemon_from_pid(&paths)?;
        if !terminated || wait_until_daemon_down(&paths, Duration::from_secs(5)) {
            cleanup_stale_files(&paths);
        }
        return Ok(terminated);
    };

    let request = json!({
        "id": request_id("stop"),
        "type": "stop",
        "version": daemon_version(),
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
    fs::create_dir_all(&paths.base_dir)
        .with_context(|| format!("create {}", paths.base_dir.display()))?;

    let _lock = acquire_spawn_lock(&paths)?;
    if daemon_is_current(&paths)? {
        return Ok(());
    }

    let _ = request_daemon_stop(&paths);
    let _ = terminate_daemon_from_pid(&paths);
    let _ = wait_until_daemon_down(&paths, Duration::from_secs(5));
    cleanup_stale_files(&paths);

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
    let mut stream = connect_to_daemon(&paths).context("connect to yoetz live-CDP daemon")?;
    let timeout =
        Duration::from_secs(timeout_secs.saturating_add(RESPONSE_READ_TIMEOUT_GRACE_SECS));
    set_stream_timeouts(&stream, timeout)?;

    let request = build_execute_request(script, timeout_secs, browser_name, cdp_endpoint);
    write_request(&mut stream, &request).context("send yoetz live-CDP daemon execute request")?;
    let mut reader = BufReader::new(stream);
    read_until_complete(&mut reader, timeout)
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
        Err(first_error) => {
            thread::sleep(Duration::from_millis(DAEMON_START_POLL_MS));
            match request_daemon_status(paths) {
                Ok(status) => Ok(status.version.as_deref() == Some(daemon_version().as_str())),
                Err(error) if is_daemon_connect_error(&error) => Ok(false),
                Err(error) => Err(error.context(format!(
                    "yoetz live-CDP daemon status failed after retry; first error: {first_error:#}"
                ))),
            }
        }
    }
}

fn request_daemon_status(paths: &DaemonPaths) -> Result<DaemonStatus> {
    let mut stream = connect_to_daemon(paths)
        .map_err(DaemonConnectError)
        .context("connect to yoetz live-CDP daemon")?;
    set_stream_timeouts(&stream, Duration::from_secs(5))?;
    let request = json!({
        "id": request_id("status"),
        "type": "status",
        "version": daemon_version(),
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

fn request_daemon_stop(paths: &DaemonPaths) -> Result<()> {
    let mut stream = connect_to_daemon(paths).context("connect to yoetz live-CDP daemon")?;
    set_stream_timeouts(&stream, Duration::from_secs(5))?;
    let request = json!({
        "id": request_id("stop"),
        "type": "stop",
        "version": daemon_version(),
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
            DaemonResponse::Stdout { data } => stdout.push_str(&data),
            DaemonResponse::Stderr { data } => {
                eprint!("{data}");
                stderr.push_str(&data);
            }
            DaemonResponse::Result { data } => {
                if !data.is_null() {
                    stdout.push_str(&data.to_string());
                    stdout.push('\n');
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

fn build_execute_request(
    script: &str,
    timeout_secs: u64,
    browser_name: Option<&str>,
    cdp_endpoint: Option<&str>,
) -> Value {
    let request = json!({
        "id": request_id("execute"),
        "type": "execute",
        "version": daemon_version(),
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
    hasher.update(EMBEDDED_DAEMON.as_bytes());
    hex::encode(hasher.finalize())
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
    fs::create_dir_all(&paths.base_dir)
        .with_context(|| format!("create {}", paths.base_dir.display()))?;
    sync_text_file(&paths.daemon_path, EMBEDDED_DAEMON)
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

fn daemon_paths() -> Result<DaemonPaths> {
    let home = home_dir()
        .ok_or_else(|| anyhow!("could not determine home directory for yoetz live-CDP daemon"))?;
    Ok(daemon_paths_for_home(&home))
}

fn daemon_paths_for_home(home: &Path) -> DaemonPaths {
    let base_dir = home.join(".yoetz");
    DaemonPaths {
        daemon_path: base_dir.join(DAEMON_FILENAME),
        socket_path: base_dir.join(DAEMON_SOCKET_FILENAME),
        pid_path: base_dir.join(DAEMON_PID_FILENAME),
        lock_path: base_dir.join(DAEMON_LOCK_FILENAME),
        log_path: base_dir.join(DAEMON_LOG_FILENAME),
        base_dir,
    }
}

fn cleanup_stale_files(paths: &DaemonPaths) {
    let _ = fs::remove_file(&paths.pid_path);
    let _ = fs::remove_file(&paths.socket_path);
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
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(stream)
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
    }

    #[test]
    fn execute_request_preserves_script_timeout_browser_and_endpoint() {
        let request = build_execute_request(
            "console.log('ok')",
            45,
            Some("yoetz-chatgpt"),
            Some("ws://127.0.0.1:9222/devtools/browser/test"),
        );
        assert_eq!(request["type"], "execute");
        assert_eq!(request["version"], daemon_version());
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
        let request = build_execute_request("await browser.listPages()", 10, None, None);
        assert_eq!(request["browser"], "default");
        assert_eq!(request["connect"], "auto");
        assert_eq!(request["timeoutMs"], 10_000);
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
    #[serial_test::serial]
    fn bundled_daemon_status_round_trips_under_isolated_home() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path().as_os_str());
        let _enabled = EnvVarGuard::remove(YOETZ_LIVE_CDP_DAEMON_ENV);

        let paths = daemon_paths_for_home(dir.path());
        let result = (|| -> Result<()> {
            ensure_daemon()?;
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
    fn stale_socket_file_is_cleaned_and_recovered() {
        let dir = tempfile::tempdir().unwrap();
        let _home = EnvVarGuard::set("HOME", dir.path().as_os_str());
        let _enabled = EnvVarGuard::remove(YOETZ_LIVE_CDP_DAEMON_ENV);
        let paths = daemon_paths_for_home(dir.path());
        fs::create_dir_all(&paths.base_dir).unwrap();
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
