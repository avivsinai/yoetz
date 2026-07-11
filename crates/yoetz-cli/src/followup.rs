use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::chatgpt_web::{self, ChatgptConversation};
use yoetz_core::session::{list_sessions_in, write_json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FollowupMetadata {
    pub session_id: String,
    pub conversation_id: String,
    pub conversation_url: String,
    pub prompt_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFollowup {
    pub conversation: ChatgptConversation,
    pub source_session_id: Option<String>,
    pub prior_prompt_hash: Option<String>,
}

pub(crate) fn resolve_followup_target(raw: &str, sessions_base: &Path) -> Result<ResolvedFollowup> {
    let sessions = list_sessions_in(sessions_base)?;
    if let Some(session) = sessions.iter().find(|session| session.id == raw) {
        let metadata = read_followup_metadata(&session.path)?.ok_or_else(|| {
            anyhow!(
                "session `{}` does not contain followup metadata; followup only works for ChatGPT browser recipe runs that wrote session metadata",
                session.id
            )
        })?;
        let conversation = chatgpt_web::normalize_conversation(&metadata.conversation_id)?;
        return Ok(ResolvedFollowup {
            conversation,
            source_session_id: Some(metadata.session_id),
            prior_prompt_hash: Some(metadata.prompt_hash),
        });
    }

    let conversation = chatgpt_web::normalize_conversation(raw)?;
    let previous = find_latest_followup_metadata_for_conversation(&conversation.id, &sessions)?;
    Ok(ResolvedFollowup {
        conversation,
        source_session_id: previous
            .as_ref()
            .map(|metadata| metadata.session_id.clone()),
        prior_prompt_hash: previous.map(|metadata| metadata.prompt_hash),
    })
}

pub(crate) fn compute_prompt_hash(prompt: &str, bundle_path: Option<&Path>) -> Result<String> {
    let bundle_hash = if let Some(bundle_path) = bundle_path {
        let bundle_bytes = fs::read(bundle_path)
            .with_context(|| format!("read bundle for followup hash {}", bundle_path.display()))?;
        let mut hasher = Sha256::new();
        hasher.update(bundle_bytes);
        hex::encode(hasher.finalize())
    } else {
        String::new()
    };

    let mut hasher = Sha256::new();
    hasher.update(prompt.as_bytes());
    hasher.update([0]);
    hasher.update(bundle_hash.as_bytes());
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn guard_duplicate_prompt(
    current_hash: &str,
    prior_hash: Option<&str>,
    allow_duplicate_prompt: bool,
    conversation_id: &str,
    prior_session_id: Option<&str>,
) -> Result<()> {
    if allow_duplicate_prompt {
        return Ok(());
    }
    let Some(prior_hash) = prior_hash else {
        return Ok(());
    };
    if current_hash != prior_hash {
        return Ok(());
    }

    let prior_context = prior_session_id
        .map(|session_id| format!(" from prior session `{session_id}`"))
        .unwrap_or_default();
    bail!(
        "duplicate followup prompt for conversation `{conversation_id}` matches the immediately previous turn{prior_context}. Pass --allow-duplicate-prompt to override."
    )
}

pub(crate) fn validate_followup_args(
    followup: Option<&str>,
    conversation_var: Option<&str>,
) -> Result<()> {
    if followup.is_some() && conversation_var.is_some() {
        bail!("--followup is mutually exclusive with --var conversation=");
    }
    Ok(())
}

pub(crate) fn write_followup_metadata(
    session_dir: &Path,
    session_id: &str,
    conversation: &ChatgptConversation,
    prompt_hash: &str,
) -> Result<()> {
    let path = followup_metadata_path(session_dir);
    let metadata = FollowupMetadata {
        session_id: session_id.to_string(),
        conversation_id: conversation.id.clone(),
        conversation_url: conversation.url.clone(),
        prompt_hash: prompt_hash.to_string(),
    };
    write_json(&path, &metadata)?;
    Ok(())
}

pub(crate) fn read_followup_metadata(session_dir: &Path) -> Result<Option<FollowupMetadata>> {
    let path = followup_metadata_path(session_dir);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read followup metadata {}", path.display()))?;
    let metadata = serde_json::from_str(&raw)
        .with_context(|| format!("parse followup metadata {}", path.display()))?;
    Ok(Some(metadata))
}

fn followup_metadata_path(session_dir: &Path) -> PathBuf {
    session_dir.join("followup.json")
}

fn find_latest_followup_metadata_for_conversation(
    conversation_id: &str,
    sessions: &[yoetz_core::types::SessionInfo],
) -> Result<Option<FollowupMetadata>> {
    for session in sessions {
        if let Some(metadata) = read_followup_metadata(&session.path)? {
            if metadata.conversation_id == conversation_id {
                return Ok(Some(metadata));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn prompt_hash_changes_with_prompt_or_bundle() {
        let dir = tempdir().unwrap();
        let bundle = dir.path().join("bundle.md");
        fs::write(&bundle, "bundle one").unwrap();

        let a = compute_prompt_hash("prompt one", Some(&bundle)).unwrap();
        let b = compute_prompt_hash("prompt two", Some(&bundle)).unwrap();
        let c = compute_prompt_hash("prompt one", None).unwrap();

        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn duplicate_guard_hits_and_can_be_overridden() {
        let err = guard_duplicate_prompt("abc", Some("abc"), false, "conv-1", Some("sess-1"))
            .unwrap_err();
        assert!(err.to_string().contains("duplicate followup prompt"));
        assert!(guard_duplicate_prompt("abc", Some("abc"), true, "conv-1", Some("sess-1")).is_ok());
        assert!(
            guard_duplicate_prompt("abc", Some("def"), false, "conv-1", Some("sess-1")).is_ok()
        );
        assert!(guard_duplicate_prompt("abc", None, false, "conv-1", None).is_ok());
    }

    #[test]
    fn followup_args_are_mutually_exclusive_with_conversation_var() {
        assert!(validate_followup_args(Some("session-1"), None).is_ok());
        assert!(validate_followup_args(None, Some("https://chatgpt.com/c/conv-1")).is_ok());
        let err = validate_followup_args(Some("session-1"), Some("https://chatgpt.com/c/conv-1"))
            .unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn session_id_resolution_requires_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        fs::create_dir_all(sessions_dir.join("20260711_000000_aaaaaa")).unwrap();
        let err = resolve_followup_target("20260711_000000_aaaaaa", &sessions_dir).unwrap_err();
        assert!(err
            .to_string()
            .contains("does not contain followup metadata"));
    }

    #[test]
    fn direct_conversation_resolution_uses_latest_session_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let old = sessions_dir.join("20260711_000000_aaaaaa");
        let new = sessions_dir.join("20260711_010000_bbbbbb");
        fs::create_dir_all(&old).unwrap();
        fs::create_dir_all(&new).unwrap();
        write_followup_metadata(
            &old,
            "20260711_000000_aaaaaa",
            &ChatgptConversation {
                id: "conv-1".to_string(),
                url: "https://chatgpt.com/c/conv-1".to_string(),
            },
            "old-hash",
        )
        .unwrap();
        write_followup_metadata(
            &new,
            "20260711_010000_bbbbbb",
            &ChatgptConversation {
                id: "conv-1".to_string(),
                url: "https://chatgpt.com/c/conv-1".to_string(),
            },
            "new-hash",
        )
        .unwrap();

        let resolved = resolve_followup_target("conv-1", &sessions_dir).unwrap();
        assert_eq!(resolved.conversation.id, "conv-1");
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("new-hash"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_bbbbbb")
        );
    }

    #[test]
    fn session_id_resolution_reads_metadata() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let session = sessions_dir.join("20260711_010000_bbbbbb");
        fs::create_dir_all(&session).unwrap();
        write_followup_metadata(
            &session,
            "20260711_010000_bbbbbb",
            &ChatgptConversation {
                id: "conv-2".to_string(),
                url: "https://chatgpt.com/c/conv-2".to_string(),
            },
            "hash-2",
        )
        .unwrap();

        let resolved = resolve_followup_target("20260711_010000_bbbbbb", &sessions_dir).unwrap();
        assert_eq!(resolved.conversation.id, "conv-2");
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("hash-2"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_bbbbbb")
        );
    }
}
