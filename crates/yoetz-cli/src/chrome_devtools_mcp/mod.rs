// Skeleton module. Real dispatch wiring in main.rs will follow this commit;
// until then the items here are intentionally unused.
#![allow(dead_code)]

//! Chrome live-attach transport for yoetz browser recipes.
//!
//! Historical note: the module name stayed `chrome_devtools_mcp` to avoid a
//! wider dispatcher/schema churn while the transport internals changed.
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
//! - `headless_chrome` uses browser discovery plus per-tab attach, which avoids
//!   the broken root-session bootstrap while still attaching to the user's real
//!   logged-in Chrome session.
//!
//! ## Module layout
//!
//! - [`client`] — `CdpMcpClient`: direct `headless_chrome` browser/tab client
//!   with a small compatibility surface for the recipe layer.
//! - [`chatgpt`] — seven-step ChatGPT Pro recipe: navigate → snapshot → upload
//!   → snapshot → type → click → wait → evaluate.
//! - [`claude`] — claude.ai recipe (follows after chatgpt is verified).
//! - [`gemini`] — gemini.google.com recipe (follows after chatgpt is verified).

pub mod client;

pub mod chatgpt;

// `pub mod claude;` and `pub mod gemini;` land after the chatgpt recipe is
// proven end-to-end against live Chrome 147. They share the same seven-step
// shape (new_page → focus → upload → type → click → wait → extract) with
// per-LLM selectors — trivial to add once the client plumbing is verified.

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
    /// "use the default localhost:9222 and resolve the browser websocket from
    /// DevToolsActivePort or `/json/version`."
    pub cdp_endpoint: Option<String>,

    /// Path to the bundle markdown file to upload via `upload_file`.
    /// The live-attach Chrome transport requires a real file attachment and
    /// does not fall back to paste mode.
    pub bundle_path: Option<PathBuf>,

    /// Reserved for compatibility with other recipe contexts. The current
    /// live-attach Chrome transport does not use inline bundle text.
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
