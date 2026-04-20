use serde_json::{json, Value};
use std::path::PathBuf;

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
    pub prompt: String,
    pub browser_context_id: Option<String>,
    pub profile_email: Option<String>,
    pub run_id: String,
    pub wait_timeout_ms: u64,
    pub wait_interval_ms: u64,
    pub disable_extended: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChatgptRecipeOutput {
    pub transport: String,
    pub backend: String,
    pub response: String,
    pub model_used: Option<String>,
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

    #[test]
    fn chatgpt_recipe_output_serializes_standard_contract() {
        let output = ChatgptRecipeOutput {
            transport: "dev-browser".to_string(),
            backend: "dev-browser".to_string(),
            response: "ok".to_string(),
            model_used: Some("gpt-5-4-pro".to_string()),
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
        assert_eq!(payload["warnings"], json!(["fallback"]));
        assert_eq!(payload["fallback_used"], true);
        assert_eq!(payload["delivery_mode"], "paste");
        assert_eq!(payload["auto_paste_fallback"], true);
    }
}
