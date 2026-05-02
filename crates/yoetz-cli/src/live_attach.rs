use anyhow::{anyhow, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify};

use crate::browser::ResolvedCdpTarget;
use crate::chatgpt_web;
use crate::chrome_devtools_mcp::{
    self,
    chatgpt::{self, ChatgptRunResult},
    client::{
        browser_id_from_ws_endpoint, discover_devtools_active_port_files,
        discover_running_chrome_targets, is_closed_cdp_transport_error, CdpMcpClient,
        DevtoolsActivePortFile, RunningChromeTarget,
    },
};
use yoetz_core::paths::home_dir;

const LIVE_ATTACH_STATE_FILENAME: &str = "live-attach.json";
const LIVE_ATTACH_DAEMON_FILENAME: &str = "live-attach-daemon.json";
const LIVE_ATTACH_DAEMON_LOCK_FILENAME: &str = "live-attach-daemon.lock";
const LIVE_ATTACH_DAEMON_LOG_FILENAME: &str = "live-attach-daemon.log";
const IMPLICIT_TARGET_KEY: &str = "implicit-default";
const DEFAULT_CONTEXT_KEY: &str = "__default__";
const DAEMON_START_TIMEOUT: Duration = Duration::from_secs(10);
const DAEMON_START_POLL_MS: u64 = 100;
const DAEMON_HEALTH_RPC_TIMEOUT: Duration = Duration::from_secs(5);
// The first live attach can block on Chrome's native "Allow remote debugging?"
// dialog. Give an operator several minutes to approve it; after that the daemon
// keeps the approved websocket open and later recipe runs should not prompt.
const DAEMON_ENSURE_SESSION_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const DAEMON_RECIPE_RPC_GRACE: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LiveAttachStatus {
    Attached,
    AwaitingApproval,
    Degraded,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DaemonHealth {
    NotRunning,
    Healthy,
    Busy,
    Stale,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveAttachTarget {
    pub key: String,
    pub connect_endpoint: Option<String>,
    pub endpoint: Option<String>,
    pub source_path: Option<PathBuf>,
    pub browser_id: Option<String>,
    pub implicit_default: bool,
}

impl LiveAttachTarget {
    pub fn from_resolved(target: Option<&ResolvedCdpTarget>) -> Self {
        match target {
            Some(target) => Self {
                key: target.live_attach_target_key(),
                connect_endpoint: Some(target.endpoint.clone()),
                endpoint: Some(target.endpoint.clone()),
                source_path: target.source_path().map(Path::to_path_buf),
                browser_id: browser_id_from_ws_endpoint(&target.endpoint),
                implicit_default: false,
            },
            None => Self {
                key: IMPLICIT_TARGET_KEY.to_string(),
                connect_endpoint: None,
                endpoint: None,
                source_path: None,
                browser_id: None,
                implicit_default: true,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LiveAttachSession {
    pub target_key: String,
    pub control_run_id: String,
    pub browser_context_id: Option<String>,
    pub status: LiveAttachStatus,
    pub endpoint: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DaemonSummary {
    pub health: DaemonHealth,
    pub pid: Option<u32>,
    pub session_count: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct PersistedContextState {
    browser_context_id: Option<String>,
    control_run_id: String,
    updated_at_unix_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct PersistedTargetState {
    endpoint: Option<String>,
    status: Option<LiveAttachStatus>,
    updated_at_unix_ms: Option<u64>,
    last_error: Option<String>,
    contexts: BTreeMap<String, PersistedContextState>,
}

#[derive(Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct LiveAttachState {
    sessions: BTreeMap<String, PersistedTargetState>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct DaemonMetadata {
    pid: u32,
    addr: String,
    token: String,
    started_at_unix_ms: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum DaemonRequest {
    Ping {
        token: String,
    },
    Status {
        token: String,
    },
    Shutdown {
        token: String,
    },
    EnsureSession {
        token: String,
        target: LiveAttachTarget,
        browser_context_id: Option<String>,
        profile_email: Option<String>,
    },
    RunRecipe {
        token: String,
        target: LiveAttachTarget,
        recipe_ctx: chrome_devtools_mcp::DevtoolsMcpRecipeContext,
    },
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum DaemonResponse {
    Pong,
    Status { session_count: usize },
    Session { session: LiveAttachSession },
    Recipe { result: ChatgptRunResult },
    Error { message: String },
}

struct DaemonSession {
    target: LiveAttachTarget,
    client: CdpMcpClient,
}

struct LiveAttachDaemon {
    state: LiveAttachState,
    sessions: BTreeMap<String, DaemonSession>,
}

impl LiveAttachDaemon {
    fn load() -> Result<Self> {
        Ok(Self {
            state: load_state()?,
            sessions: BTreeMap::new(),
        })
    }

    async fn ensure_chatgpt_session(
        &mut self,
        target: LiveAttachTarget,
        explicit_context_id: Option<&str>,
        profile_email: Option<&str>,
    ) -> Result<LiveAttachSession> {
        let target_key = target.key.clone();
        let mut session = self.take_or_connect_session(target).await?;
        let mut reconnect_used = false;

        loop {
            let result = self
                .ensure_chatgpt_session_with_session(
                    &mut session,
                    explicit_context_id,
                    profile_email,
                )
                .await;

            match result {
                Ok(session_info) => {
                    self.sessions.insert(target_key.clone(), session);
                    return Ok(session_info);
                }
                Err(err) if !reconnect_used && is_closed_cdp_transport_error(&err) => {
                    reconnect_used = true;
                    self.reconnect_session_in_place(&mut session).await?;
                }
                Err(err) => {
                    self.record_target_error(
                        &target_key,
                        session.target.endpoint.as_deref(),
                        &err,
                    )?;
                    self.sessions.insert(target_key.clone(), session);
                    return Err(err);
                }
            }
        }
    }

    async fn ensure_chatgpt_session_with_session(
        &mut self,
        session: &mut DaemonSession,
        explicit_context_id: Option<&str>,
        profile_email: Option<&str>,
    ) -> Result<LiveAttachSession> {
        let (browser_context_id, control_run_id) = self
            .ensure_chatgpt_control_tab_for_selectors(session, explicit_context_id, profile_email)
            .await?;

        self.record_target_success(
            &session.target.key,
            Some(session.client.ws_endpoint()),
            browser_context_id.as_deref(),
            &control_run_id,
        )?;
        session.target.endpoint = Some(session.client.ws_endpoint().to_string());

        Ok(LiveAttachSession {
            target_key: session.target.key.clone(),
            control_run_id,
            browser_context_id,
            status: LiveAttachStatus::Attached,
            endpoint: session.target.endpoint.clone(),
        })
    }

    async fn run_chatgpt_recipe(
        &mut self,
        target: LiveAttachTarget,
        mut recipe_ctx: chrome_devtools_mcp::DevtoolsMcpRecipeContext,
    ) -> Result<ChatgptRunResult> {
        let target_key = target.key.clone();
        let mut session = self.take_or_connect_session(target).await?;
        let mut reconnect_used = false;

        if recipe_ctx.run_id.trim().is_empty() {
            recipe_ctx.run_id = chatgpt_web::generate_run_id();
        }

        loop {
            let result = self
                .run_chatgpt_recipe_with_session(&mut session, &mut recipe_ctx)
                .await;

            match result {
                Ok(result) => {
                    self.sessions.insert(target_key.clone(), session);
                    return Ok(result);
                }
                Err(err) if !reconnect_used && is_closed_cdp_transport_error(&err) => {
                    reconnect_used = true;
                    self.reconnect_session_in_place(&mut session).await?;
                }
                Err(err) => {
                    self.record_target_error(
                        &target_key,
                        session.target.endpoint.as_deref(),
                        &err,
                    )?;
                    self.sessions.insert(target_key.clone(), session);
                    return Err(err);
                }
            }
        }
    }

    async fn run_chatgpt_recipe_with_session(
        &mut self,
        session: &mut DaemonSession,
        recipe_ctx: &mut chrome_devtools_mcp::DevtoolsMcpRecipeContext,
    ) -> Result<ChatgptRunResult> {
        let (browser_context_id, control_run_id) = self
            .ensure_chatgpt_control_tab_for_selectors(
                session,
                recipe_ctx.browser_context_id.as_deref(),
                recipe_ctx.profile_email.as_deref(),
            )
            .await?;
        recipe_ctx.cdp_endpoint = Some(session.client.ws_endpoint().to_string());
        let result = match chatgpt::run_with_client_using_page_open_mode(
            &mut session.client,
            recipe_ctx,
            browser_context_id.clone(),
            live_attach_recipe_page_open_mode(),
        )
        .await
        {
            Ok(result) => result,
            Err(err) if chatgpt::should_recover_initial_page_open_after_reconnect(&err) => {
                if recipe_ctx.show_approval_guidance {
                    eprintln!(
                        "info: chrome-devtools-mcp lost the first ChatGPT page open; reconnecting once and retrying via an existing yoetz-owned anchor"
                    );
                }
                self.reconnect_session_in_place(session).await?;
                let (retry_browser_context_id, retry_control_run_id) = self
                    .ensure_chatgpt_control_tab_for_selectors(
                        session,
                        recipe_ctx.browser_context_id.as_deref(),
                        recipe_ctx.profile_email.as_deref(),
                    )
                    .await?;
                recipe_ctx.cdp_endpoint = Some(session.client.ws_endpoint().to_string());
                let retry_result = chatgpt::run_with_client_using_page_open_mode(
                    &mut session.client,
                    recipe_ctx,
                    retry_browser_context_id.clone(),
                    chatgpt::retry_initial_page_open_mode(),
                )
                .await
                .context("retrying ChatGPT page open after reconnect")
                .context(err)?;
                self.record_target_success(
                    &session.target.key,
                    Some(session.client.ws_endpoint()),
                    retry_browser_context_id.as_deref(),
                    &retry_control_run_id,
                )?;
                session.target.endpoint = Some(session.client.ws_endpoint().to_string());
                return Ok(retry_result);
            }
            Err(err) => return Err(err),
        };

        self.record_target_success(
            &session.target.key,
            Some(session.client.ws_endpoint()),
            browser_context_id.as_deref(),
            &control_run_id,
        )?;
        session.target.endpoint = Some(session.client.ws_endpoint().to_string());

        Ok(result)
    }

    async fn take_or_connect_session(&mut self, target: LiveAttachTarget) -> Result<DaemonSession> {
        if let Some(session) = self.sessions.remove(&target.key) {
            return Ok(session);
        }

        self.connect_session(target).await
    }

    async fn connect_session(&mut self, mut target: LiveAttachTarget) -> Result<DaemonSession> {
        let endpoint = resolve_connect_endpoint(&target);
        let client = chatgpt::connect_client_with_approval_lock(endpoint.as_deref(), false).await?;
        let actual_endpoint = client.ws_endpoint().to_string();
        self.ensure_target_record(&target.key).endpoint = Some(actual_endpoint.clone());
        target.browser_id = browser_id_from_ws_endpoint(&actual_endpoint).or(target.browser_id);
        target.endpoint = Some(actual_endpoint);

        Ok(DaemonSession { target, client })
    }

    async fn reconnect_session_in_place(&mut self, session: &mut DaemonSession) -> Result<()> {
        *session = self.connect_session(session.target.clone()).await?;
        Ok(())
    }

    async fn ensure_chatgpt_control_tab_for_selectors(
        &mut self,
        session: &mut DaemonSession,
        explicit_context_id: Option<&str>,
        profile_email: Option<&str>,
    ) -> Result<(Option<String>, String)> {
        let browser_context_id = session
            .client
            .resolve_browser_context_id(explicit_context_id, profile_email)?;
        let control_run_id =
            self.control_run_id_for(&session.target.key, browser_context_id.as_deref());
        chatgpt::ensure_chatgpt_control_tab_ready(
            &session.client,
            browser_context_id.as_deref(),
            Some(&control_run_id),
        )
        .await?;
        Ok((browser_context_id, control_run_id))
    }

    fn ensure_target_record(&mut self, target_key: &str) -> &mut PersistedTargetState {
        self.state
            .sessions
            .entry(target_key.to_string())
            .or_default()
    }

    fn control_run_id_for(&mut self, target_key: &str, browser_context_id: Option<&str>) -> String {
        let context_key = context_storage_key(browser_context_id);
        let target = self.ensure_target_record(target_key);
        let now = unix_ms_now();
        let context = target
            .contexts
            .entry(context_key)
            .or_insert_with(|| PersistedContextState {
                browser_context_id: browser_context_id.map(str::to_owned),
                control_run_id: chatgpt_web::generate_run_id(),
                updated_at_unix_ms: now,
            });
        context.browser_context_id = browser_context_id.map(str::to_owned);
        context.updated_at_unix_ms = now;
        context.control_run_id.clone()
    }

    fn record_target_success(
        &mut self,
        target_key: &str,
        endpoint: Option<&str>,
        browser_context_id: Option<&str>,
        control_run_id: &str,
    ) -> Result<()> {
        let now = unix_ms_now();
        let target = self.ensure_target_record(target_key);
        target.endpoint = endpoint
            .map(str::to_owned)
            .or_else(|| target.endpoint.clone());
        target.status = Some(LiveAttachStatus::Attached);
        target.updated_at_unix_ms = Some(now);
        target.last_error = None;
        target
            .contexts
            .entry(context_storage_key(browser_context_id))
            .and_modify(|context| {
                context.browser_context_id = browser_context_id.map(str::to_owned);
                context.control_run_id = control_run_id.to_string();
                context.updated_at_unix_ms = now;
            })
            .or_insert_with(|| PersistedContextState {
                browser_context_id: browser_context_id.map(str::to_owned),
                control_run_id: control_run_id.to_string(),
                updated_at_unix_ms: now,
            });
        save_state(&self.state)
    }

    fn record_target_error(
        &mut self,
        target_key: &str,
        endpoint: Option<&str>,
        err: &anyhow::Error,
    ) -> Result<()> {
        let target = self.ensure_target_record(target_key);
        target.endpoint = endpoint
            .map(str::to_owned)
            .or_else(|| target.endpoint.clone());
        target.status = Some(if crate::browser::is_chrome_approval_wait_error(err) {
            LiveAttachStatus::AwaitingApproval
        } else {
            LiveAttachStatus::Degraded
        });
        target.updated_at_unix_ms = Some(unix_ms_now());
        target.last_error = Some(format!("{err:#}"));
        save_state(&self.state)
    }
}

fn live_attach_recipe_page_open_mode() -> chatgpt::InitialPageOpenMode {
    // The daemon has already created or reused a stable yoetz-owned ChatGPT
    // control tab in this browser context. Open recipe tabs from that anchor
    // instead of issuing Target.createTarget for each run, because current
    // default-profile Chrome builds can close the approved CDP websocket on
    // external new-target creation and force another consent dialog.
    chatgpt::retry_initial_page_open_mode()
}

pub async fn ensure_chatgpt_session(
    cdp_target: Option<&ResolvedCdpTarget>,
    browser_context_id: Option<&str>,
    profile_email: Option<&str>,
    show_approval_guidance: bool,
) -> Result<LiveAttachSession> {
    let target = LiveAttachTarget::from_resolved(cdp_target);
    let daemon = ensure_daemon_running(show_approval_guidance).await?;
    match daemon_round_trip(
        &daemon,
        DaemonRequest::EnsureSession {
            token: daemon.token.clone(),
            target,
            browser_context_id: browser_context_id.map(str::to_owned),
            profile_email: profile_email.map(str::to_owned),
        },
    )
    .await?
    {
        DaemonResponse::Session { session } => Ok(session),
        other => Err(anyhow!("unexpected live-attach daemon response: {other:?}")),
    }
}

pub async fn run_chatgpt_recipe(
    cdp_target: Option<&ResolvedCdpTarget>,
    recipe_ctx: chrome_devtools_mcp::DevtoolsMcpRecipeContext,
    show_approval_guidance: bool,
) -> Result<ChatgptRunResult> {
    let target = LiveAttachTarget::from_resolved(cdp_target);
    let daemon = ensure_daemon_running(show_approval_guidance).await?;
    match daemon_round_trip(
        &daemon,
        DaemonRequest::RunRecipe {
            token: daemon.token.clone(),
            target,
            recipe_ctx,
        },
    )
    .await?
    {
        DaemonResponse::Recipe { result } => Ok(result),
        other => Err(anyhow!("unexpected live-attach daemon response: {other:?}")),
    }
}

#[allow(dead_code)]
pub async fn shutdown() -> Result<()> {
    if let Some(daemon) = load_daemon_metadata()? {
        let _ = request_daemon_shutdown(&daemon).await;
    }
    clear_daemon_metadata()
}

pub async fn reset() -> Result<()> {
    if let Some(daemon) = load_daemon_metadata()? {
        if request_daemon_shutdown(&daemon).await.is_err() {
            let _ = terminate_daemon_process(daemon.pid);
        }
        let _ = clear_daemon_metadata();
    }
    clear_state()
}

pub async fn serve_daemon() -> Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("bind yoetz live-attach daemon")?;
    let metadata = DaemonMetadata {
        pid: std::process::id(),
        addr: listener
            .local_addr()
            .context("read yoetz live-attach daemon address")?
            .to_string(),
        token: chatgpt_web::generate_run_id(),
        started_at_unix_ms: unix_ms_now(),
    };
    save_daemon_metadata(&metadata)?;
    let _metadata_guard = DaemonMetadataGuard;
    let daemon = Arc::new(AsyncMutex::new(LiveAttachDaemon::load()?));
    let shutdown = Arc::new(Notify::new());

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            accepted = listener.accept() => {
                let (socket, _) = accepted.context("accept yoetz live-attach client")?;
                let token = metadata.token.clone();
                let daemon = Arc::clone(&daemon);
                let shutdown = Arc::clone(&shutdown);
                tokio::spawn(async move {
                    if let Err(err) = handle_daemon_connection(socket, &token, daemon, shutdown).await {
                        eprintln!("warn: yoetz live-attach daemon connection failed: {err:#}");
                    }
                });
            }
        }
    }

    Ok(())
}

pub fn inspect_daemon_sync() -> DaemonSummary {
    let Some(metadata) = load_daemon_metadata().ok().flatten() else {
        return DaemonSummary {
            health: DaemonHealth::NotRunning,
            pid: None,
            session_count: 0,
        };
    };

    match daemon_round_trip_blocking(
        &metadata,
        DaemonRequest::Status {
            token: metadata.token.clone(),
        },
    ) {
        Ok(DaemonResponse::Status { session_count }) => DaemonSummary {
            health: DaemonHealth::Healthy,
            pid: Some(metadata.pid),
            session_count,
        },
        Err(err) if is_daemon_rpc_timeout_error(&err) => DaemonSummary {
            health: DaemonHealth::Busy,
            pid: Some(metadata.pid),
            session_count: 0,
        },
        _ => DaemonSummary {
            health: DaemonHealth::Stale,
            pid: Some(metadata.pid),
            session_count: 0,
        },
    }
}

async fn request_daemon_shutdown(daemon: &DaemonMetadata) -> Result<()> {
    match daemon_round_trip(
        daemon,
        DaemonRequest::Shutdown {
            token: daemon.token.clone(),
        },
    )
    .await?
    {
        DaemonResponse::Pong => Ok(()),
        other => Err(anyhow!(
            "unexpected live-attach daemon shutdown response: {other:?}"
        )),
    }
}

fn daemon_rpc_timeout_error(addr: &str, timeout: Duration) -> anyhow::Error {
    anyhow!(
        "yoetz live-attach daemon at {addr} timed out after {}ms waiting for a response",
        timeout.as_millis()
    )
}

fn resolve_connect_endpoint(target: &LiveAttachTarget) -> Option<String> {
    resolve_connect_endpoint_with_discovery(
        target,
        &discover_running_chrome_targets(),
        &discover_devtools_active_port_files(),
    )
}

fn resolve_connect_endpoint_with_discovery(
    target: &LiveAttachTarget,
    running_targets: &[RunningChromeTarget],
    active_port_files: &[DevtoolsActivePortFile],
) -> Option<String> {
    if target.implicit_default {
        return None;
    }
    if let Some(source_path) = target.source_path.as_deref() {
        if let Some(endpoint) =
            discover_endpoint_for_source_path(source_path, running_targets, active_port_files)
        {
            return Some(endpoint);
        }
    }
    if let Some(browser_id) = target.browser_id.as_deref() {
        if let Some(endpoint) =
            discover_endpoint_for_browser_id(browser_id, running_targets, active_port_files)
        {
            return Some(endpoint);
        }
    }
    target
        .connect_endpoint
        .clone()
        .or_else(|| target.endpoint.clone())
}

fn discover_endpoint_for_source_path(
    source_path: &Path,
    running_targets: &[RunningChromeTarget],
    active_port_files: &[DevtoolsActivePortFile],
) -> Option<String> {
    running_targets
        .iter()
        .find(|target| target.source_path.as_path() == source_path)
        .map(|target| target.ws_endpoint.clone())
        .or_else(|| {
            active_port_files
                .iter()
                .find(|file| file.healthy && file.path.as_path() == source_path)
                .and_then(|file| file.ws_endpoint.clone())
        })
}

fn discover_endpoint_for_browser_id(
    browser_id: &str,
    running_targets: &[RunningChromeTarget],
    active_port_files: &[DevtoolsActivePortFile],
) -> Option<String> {
    running_targets
        .iter()
        .find(|target| {
            browser_id_from_ws_endpoint(&target.ws_endpoint).as_deref() == Some(browser_id)
        })
        .map(|target| target.ws_endpoint.clone())
        .or_else(|| {
            active_port_files
                .iter()
                .find(|file| {
                    file.healthy
                        && file
                            .ws_endpoint
                            .as_deref()
                            .and_then(browser_id_from_ws_endpoint)
                            .as_deref()
                            == Some(browser_id)
                })
                .and_then(|file| file.ws_endpoint.clone())
        })
}

async fn ensure_daemon_running(show_approval_guidance: bool) -> Result<DaemonMetadata> {
    if let Some(metadata) = healthy_daemon_metadata().await? {
        return Ok(metadata);
    }

    let _lock = acquire_waitable_lock(&daemon_lock_path(), "live-attach daemon lock")?;
    if let Some(metadata) = healthy_daemon_metadata().await? {
        return Ok(metadata);
    }

    if show_approval_guidance {
        eprintln!(
            "info: starting yoetz live-attach daemon so attach/check/recipe can reuse one Chrome CDP session"
        );
    }
    spawn_daemon_process()?;

    let deadline = tokio::time::Instant::now() + DAEMON_START_TIMEOUT;
    loop {
        if let Some(metadata) = healthy_daemon_metadata().await? {
            return Ok(metadata);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!(
                "yoetz live-attach daemon did not start within {}s; check {} for details",
                DAEMON_START_TIMEOUT.as_secs(),
                daemon_log_path().display()
            ));
        }
        tokio::time::sleep(Duration::from_millis(DAEMON_START_POLL_MS)).await;
    }
}

async fn healthy_daemon_metadata() -> Result<Option<DaemonMetadata>> {
    healthy_daemon_metadata_with_timeout(DAEMON_HEALTH_RPC_TIMEOUT).await
}

async fn healthy_daemon_metadata_with_timeout(timeout: Duration) -> Result<Option<DaemonMetadata>> {
    let Some(metadata) = load_daemon_metadata()? else {
        return Ok(None);
    };
    // A long-running recipe can legitimately keep the single-owner daemon busy
    // long enough for a health ping to time out. Preserve the metadata in that
    // case so we do not spawn a second owner and trigger an extra Chrome attach.
    match daemon_round_trip_with_timeout(
        &metadata,
        DaemonRequest::Ping {
            token: metadata.token.clone(),
        },
        timeout,
    )
    .await
    {
        Ok(DaemonResponse::Pong) => Ok(Some(metadata)),
        Err(err) if is_daemon_rpc_timeout_error(&err) => Ok(Some(metadata)),
        _ => {
            let _ = clear_daemon_metadata();
            Ok(None)
        }
    }
}

async fn handle_daemon_connection(
    socket: TcpStream,
    token: &str,
    daemon: Arc<AsyncMutex<LiveAttachDaemon>>,
    shutdown: Arc<Notify>,
) -> Result<()> {
    let mut reader = AsyncBufReader::new(socket);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("read yoetz live-attach request")?;

    let request = serde_json::from_str::<DaemonRequest>(line.trim_end())
        .context("parse yoetz live-attach request")?;
    let (response, shutdown_requested) = match dispatch_daemon_request(request, token, daemon).await
    {
        Ok(response) => response,
        Err(err) => (
            DaemonResponse::Error {
                message: format!("{err:#}"),
            },
            false,
        ),
    };

    let socket = reader.get_mut();
    socket
        .write_all(format!("{}\n", serde_json::to_string(&response)?).as_bytes())
        .await
        .context("write yoetz live-attach response")?;
    socket
        .flush()
        .await
        .context("flush yoetz live-attach response")?;

    if shutdown_requested {
        shutdown.notify_one();
    }

    Ok(())
}

async fn dispatch_daemon_request(
    request: DaemonRequest,
    token: &str,
    daemon: Arc<AsyncMutex<LiveAttachDaemon>>,
) -> Result<(DaemonResponse, bool)> {
    match request {
        DaemonRequest::Ping {
            token: request_token,
        } => {
            ensure_token(token, &request_token)?;
            Ok((DaemonResponse::Pong, false))
        }
        DaemonRequest::Shutdown {
            token: request_token,
        } => {
            ensure_token(token, &request_token)?;
            Ok((DaemonResponse::Pong, true))
        }
        request => {
            let mut daemon = daemon.lock().await;
            dispatch_daemon_request_locked(request, token, &mut daemon)
                .await
                .map(|response| (response, false))
        }
    }
}

async fn dispatch_daemon_request_locked(
    request: DaemonRequest,
    token: &str,
    daemon: &mut LiveAttachDaemon,
) -> Result<DaemonResponse> {
    match request {
        DaemonRequest::Ping {
            token: request_token,
        } => {
            ensure_token(token, &request_token)?;
            Ok(DaemonResponse::Pong)
        }
        DaemonRequest::Status {
            token: request_token,
        } => {
            ensure_token(token, &request_token)?;
            Ok(DaemonResponse::Status {
                session_count: daemon.sessions.len(),
            })
        }
        DaemonRequest::Shutdown {
            token: request_token,
        } => {
            ensure_token(token, &request_token)?;
            Ok(DaemonResponse::Pong)
        }
        DaemonRequest::EnsureSession {
            token: request_token,
            target,
            browser_context_id,
            profile_email,
        } => {
            ensure_token(token, &request_token)?;
            let session = daemon
                .ensure_chatgpt_session(
                    target,
                    browser_context_id.as_deref(),
                    profile_email.as_deref(),
                )
                .await?;
            Ok(DaemonResponse::Session { session })
        }
        DaemonRequest::RunRecipe {
            token: request_token,
            target,
            recipe_ctx,
        } => {
            ensure_token(token, &request_token)?;
            let result = daemon.run_chatgpt_recipe(target, recipe_ctx).await?;
            Ok(DaemonResponse::Recipe { result })
        }
    }
}

fn ensure_token(expected: &str, actual: &str) -> Result<()> {
    if expected == actual {
        Ok(())
    } else {
        Err(anyhow!("invalid yoetz live-attach daemon token"))
    }
}

async fn daemon_round_trip(
    metadata: &DaemonMetadata,
    request: DaemonRequest,
) -> Result<DaemonResponse> {
    let timeout = daemon_request_timeout(&request);
    daemon_round_trip_with_timeout(metadata, request, timeout).await
}

fn daemon_request_timeout(request: &DaemonRequest) -> Duration {
    match request {
        DaemonRequest::Ping { .. }
        | DaemonRequest::Status { .. }
        | DaemonRequest::Shutdown { .. } => DAEMON_HEALTH_RPC_TIMEOUT,
        DaemonRequest::EnsureSession { .. } => DAEMON_ENSURE_SESSION_TIMEOUT,
        DaemonRequest::RunRecipe { recipe_ctx, .. } => {
            Duration::from_millis(recipe_ctx.response_timeout_ms.saturating_add(
                u64::try_from(DAEMON_RECIPE_RPC_GRACE.as_millis()).unwrap_or(u64::MAX),
            ))
        }
    }
}

async fn daemon_round_trip_with_timeout(
    metadata: &DaemonMetadata,
    request: DaemonRequest,
    timeout: Duration,
) -> Result<DaemonResponse> {
    tokio::time::timeout(timeout, daemon_round_trip_inner(metadata, request))
        .await
        .map_err(|_| daemon_rpc_timeout_error(&metadata.addr, timeout))?
}

fn daemon_round_trip_blocking(
    metadata: &DaemonMetadata,
    request: DaemonRequest,
) -> Result<DaemonResponse> {
    daemon_round_trip_blocking_with_timeout(metadata, request, DAEMON_HEALTH_RPC_TIMEOUT)
}

fn daemon_round_trip_blocking_with_timeout(
    metadata: &DaemonMetadata,
    request: DaemonRequest,
    timeout: Duration,
) -> Result<DaemonResponse> {
    let addr = metadata
        .addr
        .parse::<SocketAddr>()
        .with_context(|| format!("parse yoetz live-attach daemon address {}", metadata.addr))?;
    let stream = std::net::TcpStream::connect_timeout(&addr, timeout).map_err(|err| {
        daemon_blocking_io_error(
            err,
            &metadata.addr,
            timeout,
            &format!("connect to yoetz live-attach daemon at {}", metadata.addr),
        )
    })?;
    stream
        .set_read_timeout(Some(timeout))
        .context("set yoetz live-attach daemon read timeout")?;
    stream
        .set_write_timeout(Some(timeout))
        .context("set yoetz live-attach daemon write timeout")?;
    let mut reader = BufReader::new(stream);
    reader
        .get_mut()
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .map_err(|err| {
            daemon_blocking_io_error(
                err,
                &metadata.addr,
                timeout,
                "write yoetz live-attach request",
            )
        })?;
    reader.get_mut().flush().map_err(|err| {
        daemon_blocking_io_error(
            err,
            &metadata.addr,
            timeout,
            "flush yoetz live-attach request",
        )
    })?;

    let mut line = String::new();
    reader.read_line(&mut line).map_err(|err| {
        daemon_blocking_io_error(
            err,
            &metadata.addr,
            timeout,
            "read yoetz live-attach response",
        )
    })?;
    let response = serde_json::from_str::<DaemonResponse>(line.trim_end())
        .context("parse yoetz live-attach response")?;
    match response {
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        other => Ok(other),
    }
}

fn daemon_blocking_io_error(
    err: io::Error,
    addr: &str,
    timeout: Duration,
    action: &str,
) -> anyhow::Error {
    if matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    ) {
        daemon_rpc_timeout_error(addr, timeout)
    } else {
        anyhow::Error::new(err).context(action.to_string())
    }
}

pub fn is_daemon_rpc_timeout_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        let message = cause.to_string();
        message.contains("yoetz live-attach daemon at ") && message.contains("timed out")
    })
}

async fn daemon_round_trip_inner(
    metadata: &DaemonMetadata,
    request: DaemonRequest,
) -> Result<DaemonResponse> {
    let stream = TcpStream::connect(&metadata.addr)
        .await
        .with_context(|| format!("connect to yoetz live-attach daemon at {}", metadata.addr))?;
    let mut reader = AsyncBufReader::new(stream);
    reader
        .get_mut()
        .write_all(format!("{}\n", serde_json::to_string(&request)?).as_bytes())
        .await
        .context("write yoetz live-attach request")?;
    reader
        .get_mut()
        .flush()
        .await
        .context("flush yoetz live-attach request")?;

    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("read yoetz live-attach response")?;
    let response = serde_json::from_str::<DaemonResponse>(line.trim_end())
        .context("parse yoetz live-attach response")?;
    match response {
        DaemonResponse::Error { message } => Err(anyhow!(message)),
        other => Ok(other),
    }
}

fn spawn_daemon_process() -> Result<()> {
    let exe = env::current_exe().context("resolve current yoetz executable")?;
    let log_path = daemon_log_path();
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("open {}", log_path.display()))?;
    let stderr = stdout
        .try_clone()
        .with_context(|| format!("clone {}", log_path.display()))?;

    Command::new(exe)
        .args(["browser", "live-attach-daemon"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .context("spawn yoetz live-attach daemon")?;

    Ok(())
}

#[cfg(unix)]
fn terminate_daemon_process(pid: u32) -> Result<()> {
    let status = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .with_context(|| format!("send SIGTERM to yoetz live-attach daemon pid {pid}"))?;
    if !status.success() {
        let _ = Command::new("kill")
            .args(["-KILL", &pid.to_string()])
            .status();
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_daemon_process(_pid: u32) -> Result<()> {
    Ok(())
}

fn acquire_waitable_lock(lock_path: &Path, action: &str) -> Result<File> {
    if let Some(parent) = lock_path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(lock_path)
        .with_context(|| format!("open {action} {}", lock_path.display()))?;

    match file.try_lock_exclusive() {
        Ok(()) => Ok(file),
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
            file.lock_exclusive()
                .with_context(|| format!("lock {action} {}", lock_path.display()))?;
            Ok(file)
        }
        Err(err) => Err(err).with_context(|| format!("lock {action} {}", lock_path.display())),
    }
}

fn load_state() -> Result<LiveAttachState> {
    let path = live_attach_state_path();
    if !path.exists() {
        return Ok(LiveAttachState::default());
    }

    let content = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    match serde_json::from_str(&content) {
        Ok(state) => Ok(state),
        Err(_) => {
            let _ = fs::remove_file(&path);
            Ok(LiveAttachState::default())
        }
    }
}

fn save_state(state: &LiveAttachState) -> Result<()> {
    write_json_file(&live_attach_state_path(), state)
}

fn clear_state() -> Result<()> {
    remove_if_exists(&live_attach_state_path())
}

fn save_daemon_metadata(metadata: &DaemonMetadata) -> Result<()> {
    write_json_file(&daemon_metadata_path(), metadata)
}

fn load_daemon_metadata() -> Result<Option<DaemonMetadata>> {
    let path = daemon_metadata_path();
    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    match serde_json::from_str(&content) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(_) => {
            let _ = fs::remove_file(&path);
            Ok(None)
        }
    }
}

fn clear_daemon_metadata() -> Result<()> {
    remove_if_exists(&daemon_metadata_path())
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let data = serde_json::to_string_pretty(value)?;
    let tmp_path = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&tmp_path, data).with_context(|| format!("write {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("rename {} -> {}", tmp_path.display(), path.display()))
}

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn context_storage_key(browser_context_id: Option<&str>) -> String {
    browser_context_id
        .unwrap_or(DEFAULT_CONTEXT_KEY)
        .to_string()
}

fn live_attach_state_path() -> PathBuf {
    path_from_env_or_home("YOETZ_LIVE_ATTACH_STATE_PATH", LIVE_ATTACH_STATE_FILENAME)
}

fn daemon_metadata_path() -> PathBuf {
    path_from_env_or_home("YOETZ_LIVE_ATTACH_DAEMON_PATH", LIVE_ATTACH_DAEMON_FILENAME)
}

fn daemon_lock_path() -> PathBuf {
    path_from_env_or_home(
        "YOETZ_LIVE_ATTACH_DAEMON_LOCK_PATH",
        LIVE_ATTACH_DAEMON_LOCK_FILENAME,
    )
}

fn daemon_log_path() -> PathBuf {
    path_from_env_or_home(
        "YOETZ_LIVE_ATTACH_DAEMON_LOG_PATH",
        LIVE_ATTACH_DAEMON_LOG_FILENAME,
    )
}

fn path_from_env_or_home(env_key: &str, filename: &str) -> PathBuf {
    if let Some(path) = env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(path);
    }
    if let Some(home) = home_dir() {
        return home.join(".yoetz").join(filename);
    }
    PathBuf::from(".yoetz").join(filename)
}

fn unix_ms_now() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

struct DaemonMetadataGuard;

impl Drop for DaemonMetadataGuard {
    fn drop(&mut self) {
        let _ = clear_daemon_metadata();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;
    use std::sync::{mpsc, Mutex, MutexGuard, OnceLock};
    use std::thread;
    use std::time::Instant;
    use tempfile::tempdir;

    fn lock_env() -> MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(())).lock().unwrap()
    }

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        #[allow(unsafe_code)]
        fn set<K>(key: &'static str, value: K) -> Self
        where
            K: AsRef<std::ffi::OsStr>,
        {
            let original = env::var_os(key);
            unsafe {
                env::set_var(key, value);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        #[allow(unsafe_code)]
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe { env::set_var(self.key, value) },
                None => unsafe { env::remove_var(self.key) },
            }
        }
    }

    #[test]
    fn live_attach_target_uses_implicit_key_without_resolved_target() {
        let target = LiveAttachTarget::from_resolved(None);
        assert_eq!(target.key, IMPLICIT_TARGET_KEY);
        assert_eq!(target.connect_endpoint, None);
        assert_eq!(target.endpoint, None);
        assert!(target.implicit_default);
        assert_eq!(target.source_path, None);
        assert_eq!(target.browser_id, None);
    }

    #[test]
    fn context_storage_key_uses_default_marker() {
        assert_eq!(context_storage_key(None), DEFAULT_CONTEXT_KEY);
        assert_eq!(context_storage_key(Some("ctx-work")), "ctx-work");
    }

    #[test]
    fn live_attach_recipe_page_open_mode_reuses_existing_anchor() {
        assert_eq!(
            live_attach_recipe_page_open_mode(),
            chatgpt::retry_initial_page_open_mode()
        );
    }

    #[test]
    #[serial]
    fn reset_removes_state_and_daemon_metadata_files() {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("live-attach.json");
        let daemon_path = dir.path().join("live-attach-daemon.json");
        let _state = EnvVarGuard::set("YOETZ_LIVE_ATTACH_STATE_PATH", &state_path);
        let _daemon = EnvVarGuard::set("YOETZ_LIVE_ATTACH_DAEMON_PATH", &daemon_path);

        save_state(&LiveAttachState {
            sessions: BTreeMap::from([(
                "implicit-default".to_string(),
                PersistedTargetState {
                    endpoint: Some("ws://127.0.0.1:9222/devtools/browser/test".to_string()),
                    status: Some(LiveAttachStatus::Attached),
                    updated_at_unix_ms: Some(unix_ms_now()),
                    last_error: None,
                    contexts: BTreeMap::new(),
                },
            )]),
        })
        .unwrap();
        save_daemon_metadata(&DaemonMetadata {
            pid: 1234,
            addr: "127.0.0.1:39999".to_string(),
            token: "token".to_string(),
            started_at_unix_ms: unix_ms_now(),
        })
        .unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(reset()).unwrap();
        assert!(!state_path.exists());
        assert!(!daemon_path.exists());
    }

    #[test]
    #[serial]
    fn invalid_persisted_state_is_cleared() {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let state_path = dir.path().join("live-attach.json");
        let _state = EnvVarGuard::set("YOETZ_LIVE_ATTACH_STATE_PATH", &state_path);

        fs::write(&state_path, "{not-json").unwrap();
        let state = load_state().unwrap();

        assert!(state.sessions.is_empty());
        assert!(!state_path.exists());
    }

    #[test]
    #[serial]
    fn invalid_daemon_metadata_is_cleared() {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let daemon_path = dir.path().join("live-attach-daemon.json");
        let _daemon = EnvVarGuard::set("YOETZ_LIVE_ATTACH_DAEMON_PATH", &daemon_path);

        fs::write(&daemon_path, "{not-json").unwrap();
        let metadata = load_daemon_metadata().unwrap();

        assert!(metadata.is_none());
        assert!(!daemon_path.exists());
    }

    #[test]
    fn resolve_connect_endpoint_prefers_fresh_source_path_over_stale_endpoint() {
        let source_path = PathBuf::from("/tmp/yoetz-profile");
        let target = LiveAttachTarget {
            key: "source-path:/tmp/yoetz-profile".to_string(),
            connect_endpoint: Some("http://127.0.0.1:9222".to_string()),
            endpoint: Some("ws://127.0.0.1:9222/devtools/browser/stale".to_string()),
            source_path: Some(source_path.clone()),
            browser_id: Some("stale".to_string()),
            implicit_default: false,
        };
        let running_targets = vec![RunningChromeTarget {
            ws_endpoint: "ws://127.0.0.1:9333/devtools/browser/fresh".to_string(),
            source_path: source_path.clone(),
            browser_name: "Chrome".to_string(),
            chatgpt_tab_count: 1,
            page_target_count: 3,
            page_samples: vec![],
            modified_at: None,
        }];

        assert_eq!(
            resolve_connect_endpoint_with_discovery(&target, &running_targets, &[]),
            Some("ws://127.0.0.1:9333/devtools/browser/fresh".to_string())
        );
    }

    #[test]
    fn resolve_connect_endpoint_prefers_fresh_browser_id_over_stale_endpoint() {
        let target = LiveAttachTarget {
            key: "browser-id:browser-123".to_string(),
            connect_endpoint: Some("http://127.0.0.1:9222".to_string()),
            endpoint: Some("ws://127.0.0.1:9222/devtools/browser/browser-123".to_string()),
            source_path: None,
            browser_id: Some("browser-123".to_string()),
            implicit_default: false,
        };
        let running_targets = vec![RunningChromeTarget {
            ws_endpoint: "ws://127.0.0.1:9333/devtools/browser/browser-123".to_string(),
            source_path: PathBuf::from("/tmp/other-profile"),
            browser_name: "Chrome".to_string(),
            chatgpt_tab_count: 0,
            page_target_count: 1,
            page_samples: vec![],
            modified_at: None,
        }];

        assert_eq!(
            resolve_connect_endpoint_with_discovery(&target, &running_targets, &[]),
            Some("ws://127.0.0.1:9333/devtools/browser/browser-123".to_string())
        );
    }

    #[test]
    fn resolve_connect_endpoint_implicit_default_ignores_persisted_endpoint() {
        let target = LiveAttachTarget {
            key: IMPLICIT_TARGET_KEY.to_string(),
            connect_endpoint: Some("http://127.0.0.1:9222".to_string()),
            endpoint: Some("ws://127.0.0.1:9222/devtools/browser/stale".to_string()),
            source_path: None,
            browser_id: None,
            implicit_default: true,
        };

        assert_eq!(
            resolve_connect_endpoint_with_discovery(&target, &[], &[]),
            None
        );
    }

    #[test]
    fn resolve_connect_endpoint_prefers_original_http_connect_endpoint() {
        let target = LiveAttachTarget {
            key: "endpoint:http://127.0.0.1:9222".to_string(),
            connect_endpoint: Some("http://127.0.0.1:9222".to_string()),
            endpoint: Some("ws://127.0.0.1:9222/devtools/browser/stale".to_string()),
            source_path: None,
            browser_id: None,
            implicit_default: false,
        };

        assert_eq!(
            resolve_connect_endpoint_with_discovery(&target, &[], &[]),
            Some("http://127.0.0.1:9222".to_string())
        );
    }

    #[test]
    fn daemon_request_timeout_scopes_long_running_operations() {
        assert_eq!(
            daemon_request_timeout(&DaemonRequest::Ping {
                token: "token".to_string(),
            }),
            DAEMON_HEALTH_RPC_TIMEOUT
        );
        assert_eq!(
            daemon_request_timeout(&DaemonRequest::EnsureSession {
                token: "token".to_string(),
                target: LiveAttachTarget::from_resolved(None),
                browser_context_id: None,
                profile_email: None,
            }),
            DAEMON_ENSURE_SESSION_TIMEOUT
        );
        assert_eq!(
            daemon_request_timeout(&DaemonRequest::RunRecipe {
                token: "token".to_string(),
                target: LiveAttachTarget::from_resolved(None),
                recipe_ctx: chrome_devtools_mcp::DevtoolsMcpRecipeContext {
                    response_timeout_ms: 123_000,
                    ..Default::default()
                },
            }),
            Duration::from_millis(243_000)
        );
    }

    fn spawn_unresponsive_server() -> (String, mpsc::Sender<()>, thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _stream = stream;
            let _ = shutdown_rx.recv_timeout(Duration::from_secs(30));
        });
        (addr, shutdown_tx, handle)
    }

    #[test]
    fn daemon_round_trip_blocking_times_out_against_unresponsive_server() {
        let (addr, shutdown_tx, handle) = spawn_unresponsive_server();
        let metadata = DaemonMetadata {
            pid: 1234,
            addr,
            token: "token".to_string(),
            started_at_unix_ms: unix_ms_now(),
        };
        let start = Instant::now();
        let err = daemon_round_trip_blocking_with_timeout(
            &metadata,
            DaemonRequest::Status {
                token: metadata.token.clone(),
            },
            Duration::from_millis(50),
        )
        .expect_err("unresponsive daemon should time out");

        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(is_daemon_rpc_timeout_error(&err), "{err:#}");

        let _ = shutdown_tx.send(());
        let _ = handle.join();
    }

    #[test]
    fn shutdown_request_bypasses_busy_daemon_lock() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let daemon = Arc::new(AsyncMutex::new(LiveAttachDaemon {
                state: LiveAttachState::default(),
                sessions: BTreeMap::new(),
            }));
            let _busy = daemon.lock().await;
            let shutdown = Arc::new(Notify::new());
            let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server_daemon = Arc::clone(&daemon);
            let server_shutdown = Arc::clone(&shutdown);
            let server = tokio::spawn(async move {
                let (socket, _) = listener.accept().await.unwrap();
                handle_daemon_connection(socket, "token", server_daemon, server_shutdown)
                    .await
                    .unwrap();
            });

            let mut stream = TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(br#"{"type":"shutdown","token":"token"}"#)
                .await
                .unwrap();
            stream.write_all(b"\n").await.unwrap();
            let mut reader = AsyncBufReader::new(stream);
            let mut line = String::new();
            tokio::time::timeout(DAEMON_HEALTH_RPC_TIMEOUT, reader.read_line(&mut line))
                .await
                .expect("shutdown response should not wait for the daemon work lock")
                .unwrap();

            match serde_json::from_str::<DaemonResponse>(line.trim_end()).unwrap() {
                DaemonResponse::Pong => {}
                other => panic!("expected shutdown Pong, got {other:?}"),
            }
            tokio::time::timeout(DAEMON_HEALTH_RPC_TIMEOUT, shutdown.notified())
                .await
                .expect("shutdown notify should be sent after the response is flushed");
            server.await.unwrap();
        });
    }

    #[test]
    #[serial]
    fn healthy_daemon_metadata_keeps_busy_daemon_metadata_on_timeout() {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let daemon_path = dir.path().join("live-attach-daemon.json");
        let _daemon = EnvVarGuard::set("YOETZ_LIVE_ATTACH_DAEMON_PATH", &daemon_path);
        let (addr, shutdown_tx, handle) = spawn_unresponsive_server();
        let metadata = DaemonMetadata {
            pid: 1234,
            addr,
            token: "token".to_string(),
            started_at_unix_ms: unix_ms_now(),
        };
        save_daemon_metadata(&metadata).unwrap();

        let runtime = tokio::runtime::Runtime::new().unwrap();
        let observed = runtime
            .block_on(healthy_daemon_metadata_with_timeout(Duration::from_millis(
                50,
            )))
            .unwrap();

        assert_eq!(observed, Some(metadata));
        assert!(
            daemon_path.exists(),
            "busy daemon metadata should be preserved"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.join();
    }

    #[test]
    #[serial]
    fn inspect_daemon_sync_reports_busy_on_timeout() {
        let _guard = lock_env();
        let dir = tempdir().unwrap();
        let daemon_path = dir.path().join("live-attach-daemon.json");
        let _daemon = EnvVarGuard::set("YOETZ_LIVE_ATTACH_DAEMON_PATH", &daemon_path);
        let (addr, shutdown_tx, handle) = spawn_unresponsive_server();
        let metadata = DaemonMetadata {
            pid: 1234,
            addr,
            token: "token".to_string(),
            started_at_unix_ms: unix_ms_now(),
        };
        save_daemon_metadata(&metadata).unwrap();

        let summary = inspect_daemon_sync();

        assert_eq!(
            summary,
            DaemonSummary {
                health: DaemonHealth::Busy,
                pid: Some(1234),
                session_count: 0,
            }
        );
        assert!(
            daemon_path.exists(),
            "busy daemon should not be treated as stale"
        );

        let _ = shutdown_tx.send(());
        let _ = handle.join();
    }
}
