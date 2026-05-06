//! ChatGPT recipe output types and terminal-fallback phase markers.

use anyhow::Error as AnyhowError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fmt;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ChatgptTransportPhase {
    Upload,
    Send,
    WaitResponse,
}

pub(crate) trait AnyhowResultExt<T> {
    fn with_chatgpt_phase(self, phase: ChatgptTransportPhase) -> Result<T, AnyhowError>;
}

impl<T> AnyhowResultExt<T> for Result<T, AnyhowError> {
    fn with_chatgpt_phase(self, phase: ChatgptTransportPhase) -> Result<T, AnyhowError> {
        self.map_err(|err| mark_terminal_fallback_phase(err, phase))
    }
}

impl fmt::Display for ChatgptTransportPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Upload => "upload",
            Self::Send => "send",
            Self::WaitResponse => "wait_response",
        })
    }
}

#[derive(Debug, thiserror::Error)]
#[error(
    "ChatGPT {phase} phase failed after browser side effects; automatic transport fallback is disabled"
)]
pub struct ChatgptTerminalFallbackError {
    phase: ChatgptTransportPhase,
}

impl ChatgptTerminalFallbackError {
    pub fn phase(&self) -> ChatgptTransportPhase {
        self.phase
    }
}

pub fn mark_terminal_fallback_phase(err: AnyhowError, phase: ChatgptTransportPhase) -> AnyhowError {
    err.context(ChatgptTerminalFallbackError { phase })
}

pub fn terminal_fallback_phase(err: &AnyhowError) -> Option<ChatgptTransportPhase> {
    if let Some(marker) = err.downcast_ref::<ChatgptTerminalFallbackError>() {
        return Some(marker.phase());
    }

    for cause in err.chain() {
        if let Some(marker) = cause.downcast_ref::<ChatgptTerminalFallbackError>() {
            return Some(marker.phase());
        }
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatgptModelSelectionStatus {
    Selected,
    KeptCurrent,
    Unavailable,
    Mismatch,
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
    pub prompt: String,
    pub browser_context_id: Option<String>,
    pub profile_email: Option<String>,
    pub run_id: String,
    pub wait_timeout_ms: u64,
    pub wait_interval_ms: u64,
    pub upload_timeout_ms: u64,
    pub disable_extended: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeOutput {
    pub transport: String,
    pub backend: String,
    pub response: String,
    pub model_used: Option<String>,
    pub model_selection_status: ChatgptModelSelectionStatus,
    pub warnings: Vec<String>,
    pub fallback_used: bool,
    pub delivery_mode: ChatgptDeliveryMode,
    pub auto_paste_fallback: bool,
}

impl ChatgptRecipeOutput {
    pub fn to_value(&self) -> Value {
        json!({
            "status": "ok",
            "transport": self.transport,
            "backend": self.backend,
            "response": self.response,
            "model_used": self.model_used,
            "model_selection_status": self.model_selection_status,
            "warnings": self.warnings,
            "fallback_used": self.fallback_used,
            "delivery_mode": self.delivery_mode.as_str(),
            "auto_paste_fallback": self.auto_paste_fallback,
        })
    }

    pub fn to_recipe_complete_event(&self) -> Value {
        json!({
            "type": "recipe_complete",
            "transport": self.transport,
            "backend": self.backend,
            "response": self.response,
            "model_used": self.model_used,
            "model_selection_status": self.model_selection_status,
            "warnings": self.warnings,
            "fallback_used": self.fallback_used,
            "delivery_mode": self.delivery_mode.as_str(),
            "auto_paste_fallback": self.auto_paste_fallback,
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
            model_used: Some("gpt-5-4-pro".to_string()),
            model_selection_status: ChatgptModelSelectionStatus::Selected,
            warnings: vec!["fallback".to_string()],
            fallback_used: true,
            delivery_mode: ChatgptDeliveryMode::Paste,
            auto_paste_fallback: true,
        };

        let payload = output.to_value();
        assert_eq!(payload["status"], "ok");
        assert_eq!(payload["transport"], "dev-browser");
        assert_eq!(payload["backend"], "dev-browser");
        assert_eq!(payload["response"], "ok");
        assert_eq!(payload["model_used"], "gpt-5-4-pro");
        assert_eq!(payload["model_selection_status"], "selected");
        assert_eq!(payload["warnings"], json!(["fallback"]));
        assert_eq!(payload["fallback_used"], true);
        assert_eq!(payload["delivery_mode"], "paste");
        assert_eq!(payload["auto_paste_fallback"], true);
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
