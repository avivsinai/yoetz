// Skeleton module. Real dispatch wiring in main.rs will follow this commit;
// until then the items here are intentionally unused.
#![allow(dead_code)]

//! Chrome DevTools MCP transport for yoetz browser recipes.
//!
//! This module implements the primary browser transport for Chrome 147+, which
//! delegates to `chrome-devtools-mcp` (Google-maintained MCP server) over stdio
//! using the official `rmcp` Rust SDK.
//!
//! ## Why this exists
//!
//! `dev-browser` uses Playwright's `connectOverCDP`, which hangs against Chrome
//! 147's default-profile `chrome://inspect/#remote-debugging` flow. See
//! `~/.claude/projects/-Users-avivsinai-MyProjects-yoetz/memory/project_chrome147_playwright_cdp_research.md`
//! for the full analysis. The short version:
//!
//! - Playwright 1.58.2 calls `Target.setAutoAttach { flatten: true }` on the root
//!   browser session and waits for inline `Target.attachedToTarget` events that
//!   Chrome 147 never emits for pre-existing tabs.
//! - `chrome-devtools-mcp` uses Puppeteer 24.40.0 internally, which uses a
//!   discover-then-attach-at-tab-level bootstrap that Chrome 147 accepts.
//!
//! Switching the client (Playwright → Puppeteer via chrome-devtools-mcp) while
//! keeping the target (user's real logged-in Chrome) restores Chrome 147
//! compatibility without violating the "attach to running Chrome" invariant.
//!
//! ## Module layout
//!
//! - [`client`] — `CdpMcpClient`: rmcp subprocess lifecycle + typed tool-call
//!   wrappers over `chrome-devtools-mcp` stdio.
//! - [`chatgpt`] — seven-step ChatGPT Pro recipe: navigate → snapshot → upload
//!   → snapshot → type → click → wait → evaluate.
//! - [`claude`] — claude.ai recipe (follows after chatgpt is verified).
//! - [`gemini`] — gemini.google.com recipe (follows after chatgpt is verified).

pub mod client;

pub mod chatgpt;

#[cfg(any())] // placeholders — wired after chatgpt is proven
pub mod claude;

#[cfg(any())] // placeholders — wired after chatgpt is proven
pub mod gemini;

use std::path::PathBuf;

/// Context for any chrome-devtools-mcp recipe run. Threads the minimal config
/// each recipe needs: CDP endpoint (attach-to-running-Chrome), bundle path for
/// file upload, prompt text, model hint, poll settings, and output formatting
/// toggles.
///
/// Construction of this struct is the boundary between yoetz-cli's `main.rs`
/// dispatcher and the chrome_devtools_mcp recipe layer.
#[derive(Debug, Clone)]
pub struct DevtoolsMcpRecipeContext {
    /// Explicit CDP endpoint (e.g. `http://127.0.0.1:9222`). `None` means
    /// "use the default localhost:9222, let chrome-devtools-mcp auto-resolve
    /// the browser URL under the hood via Puppeteer."
    pub cdp_endpoint: Option<String>,

    /// Path to the bundle markdown file to upload via `upload_file`. `None`
    /// means the recipe should paste-mode inline the bundle text into the
    /// prompt instead of uploading. (Parity with dev-browser's paste fallback
    /// for non-macOS platforms; chrome-devtools-mcp's `upload_file` should
    /// work on all platforms, so prefer `Some(path)` by default.)
    pub bundle_path: Option<PathBuf>,

    /// Optional inline bundle text (used only when `bundle_path` is `None`
    /// and paste-mode was requested via recipe vars).
    pub bundle_text: Option<String>,

    /// Model slug to select in the LLM provider's UI (e.g. `gpt-5-4-pro`,
    /// `claude-sonnet-4-6`, `gemini-3-1-pro`). Empty string = keep current.
    pub model: String,

    /// User prompt to send alongside the bundle.
    pub prompt: String,

    /// How long to wait for the full response before giving up, in ms.
    pub response_timeout_ms: u64,

    /// Whether to surface approval-dialog guidance to stderr in text mode.
    pub show_approval_guidance: bool,
}

impl Default for DevtoolsMcpRecipeContext {
    fn default() -> Self {
        Self {
            cdp_endpoint: None,
            bundle_path: None,
            bundle_text: None,
            model: String::new(),
            prompt: String::new(),
            response_timeout_ms: 1_800_000, // 30 min default for ChatGPT Pro Extended
            show_approval_guidance: true,
        }
    }
}
