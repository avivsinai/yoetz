use anyhow::Error as AnyhowError;
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuiltinWebRecipe {
    #[default]
    Chatgpt,
    Claude,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WebConversation {
    pub id: String,
    pub url: String,
}

impl BuiltinWebRecipe {
    pub fn detect(name: Option<&str>, recipe_path: &Path) -> Option<Self> {
        if let Some(recipe) = name.and_then(Self::from_exact_name) {
            return Some(recipe);
        }

        let stem = recipe_path
            .file_stem()
            .and_then(|value| value.to_str())?
            .to_ascii_lowercase();
        if recipe_stem_matches(&stem, "chatgpt") {
            Some(Self::Chatgpt)
        } else if recipe_stem_matches(&stem, "claude") {
            Some(Self::Claude)
        } else {
            None
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chatgpt => "chatgpt",
            Self::Claude => "claude",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT",
            Self::Claude => "Claude",
        }
    }

    fn from_exact_name(value: &str) -> Option<Self> {
        if value.eq_ignore_ascii_case("chatgpt") {
            Some(Self::Chatgpt)
        } else if value.eq_ignore_ascii_case("claude") {
            Some(Self::Claude)
        } else {
            None
        }
    }
}

impl fmt::Display for BuiltinWebRecipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.display_name())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WebRecipeTransportPhase {
    Upload,
    Send,
    WaitResponse,
}

impl fmt::Display for WebRecipeTransportPhase {
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
    "{recipe} {phase} phase failed after browser side effects; automatic transport fallback is disabled"
)]
pub struct WebRecipeTerminalFallbackError {
    recipe: BuiltinWebRecipe,
    phase: WebRecipeTransportPhase,
}

impl WebRecipeTerminalFallbackError {
    pub fn recipe(&self) -> BuiltinWebRecipe {
        self.recipe
    }

    pub fn phase(&self) -> WebRecipeTransportPhase {
        self.phase
    }
}

pub fn mark_terminal_fallback_phase(
    err: AnyhowError,
    recipe: BuiltinWebRecipe,
    phase: WebRecipeTransportPhase,
) -> AnyhowError {
    err.context(WebRecipeTerminalFallbackError { recipe, phase })
}

pub fn terminal_fallback_marker(
    err: &AnyhowError,
) -> Option<(BuiltinWebRecipe, WebRecipeTransportPhase)> {
    if let Some(marker) = err.downcast_ref::<WebRecipeTerminalFallbackError>() {
        return Some((marker.recipe(), marker.phase()));
    }
    err.chain().find_map(|cause| {
        cause
            .downcast_ref::<WebRecipeTerminalFallbackError>()
            .map(|marker| (marker.recipe(), marker.phase()))
    })
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum WebModelStrategy {
    Select,
    Current,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WebModelSelectionStatus {
    Selected,
    KeptCurrent,
    Current,
    Unavailable,
    Mismatch,
}

fn recipe_stem_matches(stem: &str, recipe: &str) -> bool {
    stem == recipe
        || stem
            .strip_prefix(recipe)
            .is_some_and(|suffix| suffix.starts_with(['-', '_']))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    #[test]
    fn terminal_fallback_marker_preserves_recipe_and_phase() {
        let err = mark_terminal_fallback_phase(
            anyhow!("send failed"),
            BuiltinWebRecipe::Claude,
            WebRecipeTransportPhase::Send,
        );

        assert_eq!(
            terminal_fallback_marker(&err),
            Some((BuiltinWebRecipe::Claude, WebRecipeTransportPhase::Send))
        );
        assert!(err.to_string().contains("Claude send phase failed"));
    }

    #[test]
    fn shared_model_contract_serializes_like_the_existing_chatgpt_contract() {
        assert_eq!(
            serde_json::to_value(WebModelStrategy::Select).unwrap(),
            serde_json::json!("select")
        );
        assert_eq!(
            serde_json::to_value(WebModelSelectionStatus::KeptCurrent).unwrap(),
            serde_json::json!("kept_current")
        );
    }
}
