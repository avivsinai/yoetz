//! ChatGPT recipe output types and terminal-fallback phase markers.

use anyhow::Error as AnyhowError;
use serde_json::{json, Value};
use std::path::PathBuf;

use crate::web_recipe::{self, BuiltinWebRecipe};

pub type ChatgptTransportPhase = web_recipe::WebRecipeTransportPhase;
pub type ChatgptModelStrategy = web_recipe::WebModelStrategy;
pub type ChatgptModelSelectionStatus = web_recipe::WebModelSelectionStatus;

pub const CHATGPT_SOL_PRO_MODEL: &str = "gpt-5-6-sol-pro";

pub(crate) trait AnyhowResultExt<T> {
    fn with_chatgpt_phase(self, phase: ChatgptTransportPhase) -> Result<T, AnyhowError>;
}

impl<T> AnyhowResultExt<T> for Result<T, AnyhowError> {
    fn with_chatgpt_phase(self, phase: ChatgptTransportPhase) -> Result<T, AnyhowError> {
        self.map_err(|err| mark_terminal_fallback_phase(err, phase))
    }
}

pub fn mark_terminal_fallback_phase(err: AnyhowError, phase: ChatgptTransportPhase) -> AnyhowError {
    web_recipe::mark_terminal_fallback_phase(err, BuiltinWebRecipe::Chatgpt, phase)
}

pub fn terminal_fallback_phase(err: &AnyhowError) -> Option<ChatgptTransportPhase> {
    if let Some((BuiltinWebRecipe::Chatgpt, phase)) = web_recipe::terminal_fallback_marker(err) {
        return Some(phase);
    }

    classify_terminal_fallback_phase_message(&format!("{err:#}"))
}

pub(crate) fn classify_terminal_fallback_phase_message(
    message: &str,
) -> Option<ChatgptTransportPhase> {
    let message = message.to_ascii_lowercase();
    const PHASE_NEEDLES: &[(&[&str], ChatgptTransportPhase)] = &[
        (
            &["chatgpt_wait_response"],
            ChatgptTransportPhase::WaitResponse,
        ),
        (
            &["timed out waiting for chatgpt response"],
            ChatgptTransportPhase::WaitResponse,
        ),
        (
            &["chatgpt response timed out"],
            ChatgptTransportPhase::WaitResponse,
        ),
        (
            &["response timed out after", "chatgpt"],
            ChatgptTransportPhase::WaitResponse,
        ),
        (&["chatgpt_wait_upload"], ChatgptTransportPhase::Upload),
        (&["recipe step", "(upload)"], ChatgptTransportPhase::Upload),
        (&["attachment chip for `"], ChatgptTransportPhase::Upload),
        (
            &["file attachment did not finish uploading"],
            ChatgptTransportPhase::Upload,
        ),
        (
            &["could not set chatgpt upload input files"],
            ChatgptTransportPhase::Upload,
        ),
        (
            &["parse attachment upload probe"],
            ChatgptTransportPhase::Upload,
        ),
        (&["upload for `"], ChatgptTransportPhase::Upload),
        (&["chatgpt_send"], ChatgptTransportPhase::Send),
        (&["chatgpt send button"], ChatgptTransportPhase::Send),
        (&["chatgpt send click"], ChatgptTransportPhase::Send),
        (
            &["missing assistant baseline in chatgpt send payload"],
            ChatgptTransportPhase::Send,
        ),
        (
            &["unexpected chatgpt send status"],
            ChatgptTransportPhase::Send,
        ),
    ];

    for (needles, phase) in PHASE_NEEDLES {
        if needles.iter().all(|needle| message.contains(needle)) {
            return Some(*phase);
        }
    }

    None
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatgptDeliveryMode {
    FileUpload,
    Paste,
}

impl ChatgptDeliveryMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FileUpload => "file_upload",
            Self::Paste => "paste",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeSpec {
    pub bundle_path: Option<PathBuf>,
    pub model: String,
    pub model_strategy: ChatgptModelStrategy,
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
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ChatgptRecipeDiagnostics {
    pub extraction_method: Option<String>,
    pub completion_reason: Option<String>,
    pub finality_anchor: Option<String>,
    pub stable_for_ms: Option<u64>,
    pub assistant_turn_count: Option<u64>,
    pub copy_button_count: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeOutput {
    pub transport: String,
    pub backend: String,
    pub response: String,
    pub model_strategy: ChatgptModelStrategy,
    pub model_used: Option<String>,
    pub model_selection_status: ChatgptModelSelectionStatus,
    pub warnings: Vec<String>,
    pub fallback_used: bool,
    pub delivery_mode: ChatgptDeliveryMode,
    pub auto_paste_fallback: bool,
    pub conversation_id: Option<String>,
    pub conversation_url: Option<String>,
    pub diagnostics: ChatgptRecipeDiagnostics,
}

impl ChatgptRecipeOutput {
    pub fn to_value(&self) -> Value {
        json!({
            "status": "ok",
            "transport": self.transport,
            "backend": self.backend,
            "response": self.response,
            "model_strategy": self.model_strategy,
            "model_used": self.model_used,
            "model_selection_status": self.model_selection_status,
            "warnings": self.warnings,
            "fallback_used": self.fallback_used,
            "delivery_mode": self.delivery_mode.as_str(),
            "auto_paste_fallback": self.auto_paste_fallback,
            "conversation_id": self.conversation_id,
            "conversation_url": self.conversation_url,
            "extraction_method": self.diagnostics.extraction_method,
            "completion_reason": self.diagnostics.completion_reason,
            "finality_anchor": self.diagnostics.finality_anchor,
            "stable_for_ms": self.diagnostics.stable_for_ms,
            "assistant_turn_count": self.diagnostics.assistant_turn_count,
            "copy_button_count": self.diagnostics.copy_button_count,
        })
    }

    pub fn to_recipe_complete_event(&self) -> Value {
        json!({
            "type": "recipe_complete",
            "transport": self.transport,
            "backend": self.backend,
            "response": self.response,
            "model_strategy": self.model_strategy,
            "model_used": self.model_used,
            "model_selection_status": self.model_selection_status,
            "warnings": self.warnings,
            "fallback_used": self.fallback_used,
            "delivery_mode": self.delivery_mode.as_str(),
            "auto_paste_fallback": self.auto_paste_fallback,
            "conversation_id": self.conversation_id,
            "conversation_url": self.conversation_url,
            "extraction_method": self.diagnostics.extraction_method,
            "completion_reason": self.diagnostics.completion_reason,
            "finality_anchor": self.diagnostics.finality_anchor,
            "stable_for_ms": self.diagnostics.stable_for_ms,
            "assistant_turn_count": self.diagnostics.assistant_turn_count,
            "copy_button_count": self.diagnostics.copy_button_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn chatgpt_recipe_output_serializes_standard_contract() {
        let output = ChatgptRecipeOutput {
            transport: "dev-browser".to_string(),
            backend: "dev-browser".to_string(),
            response: "ok".to_string(),
            model_strategy: ChatgptModelStrategy::Select,
            model_used: Some("GPT-5.6 Sol Pro".to_string()),
            model_selection_status: ChatgptModelSelectionStatus::Selected,
            warnings: vec!["fallback".to_string()],
            fallback_used: true,
            delivery_mode: ChatgptDeliveryMode::Paste,
            auto_paste_fallback: true,
            conversation_id: Some("conv-123".to_string()),
            conversation_url: Some("https://chatgpt.com/c/conv-123".to_string()),
            diagnostics: ChatgptRecipeDiagnostics {
                extraction_method: Some("copy_scope_dom_fallback".to_string()),
                completion_reason: Some("copy_button".to_string()),
                finality_anchor: Some("dom_only".to_string()),
                stable_for_ms: Some(5000),
                assistant_turn_count: Some(2),
                copy_button_count: Some(1),
            },
        };

        let payload = output.to_value();
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["transport"], "dev-browser");
        assert_eq!(payload["backend"], "dev-browser");
        assert_eq!(payload["response"], "ok");
        assert_eq!(payload["model_strategy"], "select");
        assert_eq!(payload["model_used"], "GPT-5.6 Sol Pro");
        assert_eq!(payload["model_selection_status"], "selected");
        assert_eq!(payload["warnings"], json!(["fallback"]));
        assert_eq!(payload["fallback_used"], true);
        assert_eq!(payload["delivery_mode"], "paste");
        assert_eq!(payload["auto_paste_fallback"], true);
        assert_eq!(payload["conversation_id"], "conv-123");
        assert_eq!(
            payload["conversation_url"],
            "https://chatgpt.com/c/conv-123"
        );
        assert_eq!(payload["extraction_method"], "copy_scope_dom_fallback");
        assert_eq!(payload["completion_reason"], "copy_button");
        assert_eq!(payload["finality_anchor"], "dom_only");
        assert_eq!(payload["stable_for_ms"], 5000);
        assert_eq!(payload["assistant_turn_count"], 2);
        assert_eq!(payload["copy_button_count"], 1);

        let event = output.to_recipe_complete_event();
        assert_eq!(event["conversation_id"], "conv-123");
        assert_eq!(event["conversation_url"], "https://chatgpt.com/c/conv-123");
        assert_eq!(event["extraction_method"], "copy_scope_dom_fallback");
        assert_eq!(event["completion_reason"], "copy_button");
        assert_eq!(event["finality_anchor"], "dom_only");
        assert_eq!(event["stable_for_ms"], 5000);
        assert_eq!(event["assistant_turn_count"], 2);
        assert_eq!(event["copy_button_count"], 1);
    }

    #[test]
    fn terminal_fallback_phase_reads_typed_marker() {
        let err = mark_terminal_fallback_phase(anyhow!("send failed"), ChatgptTransportPhase::Send);

        assert_eq!(
            terminal_fallback_phase(&err),
            Some(ChatgptTransportPhase::Send)
        );
    }

    #[test]
    fn terminal_fallback_phase_classifies_upload_send_and_wait_messages() {
        let cases = [
            (
                anyhow!("recipe step 7 (upload) failed: agent-browser failed"),
                ChatgptTransportPhase::Upload,
            ),
            (
                anyhow!("recipe step 8 (chatgpt_send) failed: ChatGPT send button never became enabled after typing"),
                ChatgptTransportPhase::Send,
            ),
            (
                anyhow!("recipe step 9 (chatgpt_wait_response) failed: timed out waiting for ChatGPT response"),
                ChatgptTransportPhase::WaitResponse,
            ),
        ];

        for (err, expected) in cases {
            assert_eq!(terminal_fallback_phase(&err), Some(expected));
        }
    }

    #[test]
    fn terminal_fallback_phase_does_not_classify_pre_delivery_errors() {
        let err = anyhow!("recipe step 3 (chatgpt_select_model) failed: model selector not found");

        assert_eq!(terminal_fallback_phase(&err), None);
    }
}
