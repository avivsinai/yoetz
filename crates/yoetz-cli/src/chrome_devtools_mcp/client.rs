//! rmcp client wrapper over `chrome-devtools-mcp` (stdio MCP server).
//!
//! **OWNED BY CODEX (staff engineer on this commit).** I'm leaving this stub
//! so the module compiles while codex writes the real implementation in
//! parallel. See the AMQ message on thread `review/chatgpt-cdp-page-persistence`
//! subject "GO: Commit 3 implementation" for the full contract.
//!
//! Summary of what this file needs to contain:
//!
//! - `pub struct CdpMcpClient` — owns the rmcp service handle + subprocess
//! - `impl CdpMcpClient`
//!   - `pub async fn connect_to_running_chrome(cdp_endpoint: Option<&str>) -> Result<Self>`
//!     spawns `npx -y chrome-devtools-mcp@latest --browser-url <endpoint>`
//!     via `TokioChildProcess`, calls `().serve(transport).await?`, stores handle.
//!   - `pub async fn new_page(&self, url: &str, background: bool, timeout_ms: u64) -> Result<NewPageResult>`
//!   - `pub async fn navigate_page(&self, url: &str, timeout_ms: u64) -> Result<NavigateResult>`
//!   - `pub async fn take_snapshot(&self, verbose: bool) -> Result<Snapshot>`
//!   - `pub async fn click(&self, uid: &str, double_click: bool) -> Result<()>`
//!   - `pub async fn type_text(&self, text: &str, submit_key: Option<&str>) -> Result<()>`
//!   - `pub async fn upload_file(&self, uid: &str, file_path: &Path) -> Result<()>`
//!   - `pub async fn wait_for(&self, text: &[&str], timeout_ms: u64) -> Result<WaitForResult>`
//!   - `pub async fn evaluate_script(&self, function: &str, args: Vec<serde_json::Value>) -> Result<serde_json::Value>`
//! - Snapshot parsing helpers: `Snapshot::find_uid_by_role`, `find_uid_by_text`
//! - `Drop` impl that cancels the rmcp service (no orphaned subprocess)
//! - Unit tests with fixture JSON snapshots
//!
//! The canonical rmcp client example is at
//! `modelcontextprotocol/rust-sdk/examples/clients/src/everything_stdio.rs`.
//! Tool names + arg schemas are at
//! `github.com/ChromeDevTools/chrome-devtools-mcp/blob/main/docs/tool-reference.md`.
//! A parallel research agent is writing the verbatim schemas to
//! `~/.claude/projects/-Users-avivsinai-MyProjects-yoetz/memory/project_chrome_devtools_mcp_tool_schemas.md`.

use anyhow::{anyhow, Result};
use std::path::Path;

/// Placeholder until codex lands the real implementation.
///
/// Keep this file compiling so the rest of the module (mod.rs, chatgpt.rs,
/// main.rs wiring) can be written and verified in parallel. The recipe layer
/// will call through this type even though every method currently errors.
pub struct CdpMcpClient {
    _private: (),
}

#[derive(Debug, Clone)]
pub struct NewPageResult {
    pub page_id: String,
}

#[derive(Debug, Clone)]
pub struct NavigateResult {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    /// Raw JSON from chrome-devtools-mcp's `take_snapshot` response. Parsed
    /// into a typed tree once codex lands the real implementation; for now
    /// the recipe layer just calls the helpers below.
    pub raw: serde_json::Value,
}

impl Snapshot {
    /// Find the first element with the given a11y role + accessible name.
    /// Used by the recipe layer to locate the composer, send button, etc.
    pub fn find_uid_by_role(&self, _role: &str, _name: &str) -> Option<String> {
        // TODO(codex): walk the snapshot tree and return the uid
        None
    }

    /// Find the first element containing the given text as its rendered label.
    pub fn find_uid_by_text(&self, _text: &str) -> Option<String> {
        // TODO(codex): walk the snapshot tree and return the uid
        None
    }

    /// Number of nodes matching the given role (used for assertion-style
    /// readiness checks like "is the assistant message container visible yet").
    pub fn count_by_role(&self, _role: &str) -> usize {
        0
    }
}

#[derive(Debug, Clone)]
pub struct WaitForResult {
    pub matched_text: String,
}

impl CdpMcpClient {
    /// Connect to a running Chrome instance via chrome-devtools-mcp.
    ///
    /// `cdp_endpoint` defaults to `http://127.0.0.1:9222` when `None`. This
    /// enforces the attach-to-running-Chrome invariant — no `--user-data-dir`,
    /// no dedicated profile, no separate Chrome for Testing.
    pub async fn connect_to_running_chrome(_cdp_endpoint: Option<&str>) -> Result<Self> {
        Err(anyhow!(
            "chrome-devtools-mcp client not yet implemented — codex is writing this file in parallel; see AMQ thread review/chatgpt-cdp-page-persistence"
        ))
    }

    pub async fn new_page(
        &self,
        _url: &str,
        _background: bool,
        _timeout_ms: u64,
    ) -> Result<NewPageResult> {
        Err(anyhow!("client.new_page: not yet implemented"))
    }

    pub async fn navigate_page(&self, _url: &str, _timeout_ms: u64) -> Result<NavigateResult> {
        Err(anyhow!("client.navigate_page: not yet implemented"))
    }

    pub async fn take_snapshot(&self, _verbose: bool) -> Result<Snapshot> {
        Err(anyhow!("client.take_snapshot: not yet implemented"))
    }

    pub async fn click(&self, _uid: &str, _double_click: bool) -> Result<()> {
        Err(anyhow!("client.click: not yet implemented"))
    }

    pub async fn type_text(&self, _text: &str, _submit_key: Option<&str>) -> Result<()> {
        Err(anyhow!("client.type_text: not yet implemented"))
    }

    pub async fn upload_file(&self, _uid: &str, _file_path: &Path) -> Result<()> {
        Err(anyhow!("client.upload_file: not yet implemented"))
    }

    pub async fn wait_for(&self, _text: &[&str], _timeout_ms: u64) -> Result<WaitForResult> {
        Err(anyhow!("client.wait_for: not yet implemented"))
    }

    pub async fn evaluate_script(
        &self,
        _function: &str,
        _args: Vec<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        Err(anyhow!("client.evaluate_script: not yet implemented"))
    }
}
