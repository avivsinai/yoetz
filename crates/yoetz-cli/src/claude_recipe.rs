//! Claude recipe contract and preflight policy.

use anyhow::{Context, Error as AnyhowError, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};

use crate::web_recipe::{self, BuiltinWebRecipe, WebModelSelectionStatus, WebRecipeTransportPhase};

pub const CLAUDE_FABLE_MAX_MODEL: &str = "fable-5-max";
pub const CLAUDE_FABLE_MAX_ALIAS: &str = "fable-max";
pub const CLAUDE_REPORTED_MODEL: &str = "Fable 5 Max";
pub const DEFAULT_INLINE_WARN_TOKENS: usize = 150_000;
pub const OUTPUT_CHANNEL_CONTRACT: &str =
    "Reply entirely in the chat message as plain markdown. Do not create artifacts, files, or documents.";

pub fn render_builtin_prompt(caller_prompt: &str) -> String {
    format!("{OUTPUT_CHANNEL_CONTRACT}\n\n{caller_prompt}")
}

pub fn canonical_fable_max_model(value: &str) -> Option<&'static str> {
    matches!(
        value.trim(),
        CLAUDE_FABLE_MAX_MODEL | CLAUDE_FABLE_MAX_ALIAS
    )
    .then_some(CLAUDE_FABLE_MAX_MODEL)
}

pub(crate) trait AnyhowResultExt<T> {
    fn with_claude_phase(self, phase: WebRecipeTransportPhase) -> Result<T, AnyhowError>;
}

impl<T> AnyhowResultExt<T> for Result<T, AnyhowError> {
    fn with_claude_phase(self, phase: WebRecipeTransportPhase) -> Result<T, AnyhowError> {
        self.map_err(|err| {
            web_recipe::mark_terminal_fallback_phase(err, BuiltinWebRecipe::Claude, phase)
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRecipeSpec {
    pub bundle_path: Option<PathBuf>,
    pub prompt: String,
    pub browser_context_id: Option<String>,
    pub profile_email: Option<String>,
    pub extension_instance_id: Option<String>,
    pub extension_profile_id: Option<String>,
    pub conversation_id: Option<String>,
    pub run_id: String,
    pub wait_timeout_ms: u64,
    pub wait_interval_ms: u64,
    pub upload_timeout_ms: u64,
    pub send_timeout_ms: u64,
    pub close_tab_on_complete: bool,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaudeRecipeOutput {
    pub transport: String,
    pub backend: String,
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: WebModelSelectionStatus,
    pub warnings: Vec<String>,
    pub warning_details: Vec<Value>,
    pub fallback_used: bool,
    pub conversation_id: Option<String>,
    pub conversation_url: Option<String>,
    pub run_id: String,
    pub elapsed_ms: u64,
}

impl ClaudeRecipeOutput {
    pub fn to_value(&self) -> Value {
        let warnings = self
            .warnings
            .iter()
            .cloned()
            .map(Value::String)
            .chain(self.warning_details.iter().cloned())
            .collect::<Vec<_>>();
        json!({
            "status": "ok",
            "transport": self.transport,
            "backend": self.backend,
            "response": self.response,
            "model_strategy": "select",
            "model_used": self.model_used,
            "model_selection_status": self.model_selection_status,
            "warnings": warnings,
            "fallback_used": self.fallback_used,
            "delivery_mode": "file_upload",
            "auto_paste_fallback": false,
            "conversation_id": self.conversation_id,
            "conversation_url": self.conversation_url,
            "run_id": self.run_id,
            "elapsed_ms": self.elapsed_ms,
        })
    }

    pub fn to_recipe_complete_event(&self) -> Value {
        let mut value = self.to_value();
        value["type"] = Value::String("recipe_complete".to_string());
        value
            .as_object_mut()
            .expect("output is an object")
            .remove("status");
        value
    }
}

pub fn inline_size_warnings(
    bundle_path: Option<&Path>,
    inline_warn_tokens: usize,
) -> Result<Vec<String>> {
    let Some(bundle_path) = bundle_path else {
        return Ok(Vec::new());
    };
    if inline_warn_tokens == 0 {
        return Ok(Vec::new());
    }

    let contents = fs::read_to_string(bundle_path).with_context(|| {
        format!(
            "read Claude bundle {} for token estimate",
            bundle_path.display()
        )
    })?;
    let estimated_tokens = yoetz_core::bundle::estimate_tokens(contents.chars().count());
    if estimated_tokens <= inline_warn_tokens {
        return Ok(Vec::new());
    }

    Ok(vec![format!(
        "Claude bundle is estimated {estimated_tokens} tokens, above the {inline_warn_tokens}-token heuristic inline-quality threshold; claude.ai may use retrieval-backed access, which can degrade holistic review quality. Consider trimming the bundle or accepting search-style access."
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_recipe_output_keeps_conversation_url_at_top_level() {
        let output = ClaudeRecipeOutput {
            transport: "chrome-extension-native".to_string(),
            backend: "chrome-extension-native".to_string(),
            response: "done".to_string(),
            model_used: Some(CLAUDE_REPORTED_MODEL.to_string()),
            model_selection_status: WebModelSelectionStatus::Selected,
            warnings: Vec::new(),
            warning_details: Vec::new(),
            fallback_used: false,
            conversation_id: Some("123e4567-e89b-12d3-a456-426614174000".to_string()),
            conversation_url: Some(
                "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000".to_string(),
            ),
            run_id: "run-claude".to_string(),
            elapsed_ms: 42,
        };

        let payload = output.to_value();

        assert_eq!(
            payload["conversation_url"],
            "https://claude.ai/chat/123e4567-e89b-12d3-a456-426614174000"
        );
    }

    #[test]
    fn exact_model_aliases_canonicalize_and_other_models_fail_closed() {
        assert_eq!(
            canonical_fable_max_model("fable-5-max"),
            Some(CLAUDE_FABLE_MAX_MODEL)
        );
        assert_eq!(
            canonical_fable_max_model("fable-max"),
            Some(CLAUDE_FABLE_MAX_MODEL)
        );
        assert_eq!(canonical_fable_max_model("current"), None);
        assert_eq!(canonical_fable_max_model("opus-4.8"), None);
    }

    #[test]
    fn builtin_prompt_has_a_stable_output_contract_and_preserves_caller_bytes() {
        let caller_prompt = "  Review `alpha`.\r\nKeep this trailing newline.\n";
        let rendered = render_builtin_prompt(caller_prompt);

        assert_eq!(
            rendered,
            "Reply entirely in the chat message as plain markdown. Do not create artifacts, files, or documents.\n\n  Review `alpha`.\r\nKeep this trailing newline.\n"
        );
        assert_eq!(
            rendered
                .strip_prefix(OUTPUT_CHANNEL_CONTRACT)
                .and_then(|value| value.strip_prefix("\n\n")),
            Some(caller_prompt)
        );
    }
}
