use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(test)]
use crate::chatgpt_web::{self, ChatgptConversation};
use crate::web_recipe::{BuiltinWebRecipe, WebConversation};
use yoetz_core::session::{list_sessions_in, write_json};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct FollowupMetadata {
    pub session_id: String,
    #[serde(default)]
    pub recipe: BuiltinWebRecipe,
    #[serde(default)]
    pub thread_label: Option<String>,
    pub conversation_id: String,
    pub conversation_url: String,
    pub prompt_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedFollowup {
    pub recipe: BuiltinWebRecipe,
    pub conversation: WebConversation,
    pub source_session_id: Option<String>,
    pub prior_prompt_hash: Option<String>,
}

#[cfg(test)]
pub(crate) fn resolve_followup_target(raw: &str, sessions_base: &Path) -> Result<ResolvedFollowup> {
    resolve_followup_target_for_recipe(raw, sessions_base, BuiltinWebRecipe::Chatgpt, |value| {
        let conversation = chatgpt_web::normalize_conversation(value)?;
        Ok(WebConversation {
            id: conversation.id,
            url: conversation.url,
        })
    })
}

pub(crate) fn resolve_followup_target_for_recipe<F>(
    raw: &str,
    sessions_base: &Path,
    recipe: BuiltinWebRecipe,
    normalize: F,
) -> Result<ResolvedFollowup>
where
    F: Fn(&str) -> Result<WebConversation>,
{
    let sessions = list_sessions_in(sessions_base)?;
    if let Some(session) = sessions.iter().find(|session| session.id == raw) {
        let metadata = read_followup_metadata(&session.path)?.ok_or_else(|| {
            anyhow!(
                "session `{}` does not contain followup metadata; followup only works for browser recipe runs that wrote session metadata",
                session.id,
            )
        })?;
        if metadata.recipe != recipe {
            bail!(
                "session `{}` belongs to the `{}` browser recipe and cannot be used as a `{}` followup",
                session.id,
                metadata.recipe.as_str(),
                recipe.as_str(),
            );
        }
        let conversation = normalize(&metadata.conversation_url)?;
        return Ok(ResolvedFollowup {
            recipe,
            conversation,
            source_session_id: Some(metadata.session_id),
            prior_prompt_hash: Some(metadata.prompt_hash),
        });
    }

    let conversation = normalize(raw)?;
    let previous =
        find_latest_followup_metadata_for_conversation(recipe, &conversation.id, &sessions)?;
    Ok(ResolvedFollowup {
        recipe,
        conversation,
        source_session_id: previous
            .as_ref()
            .map(|metadata| metadata.session_id.clone()),
        prior_prompt_hash: previous.map(|metadata| metadata.prompt_hash),
    })
}

pub(crate) fn validate_thread_label(label: &str) -> Result<()> {
    let mut chars = label.chars();
    let Some(first) = chars.next() else {
        bail!("thread label must match ^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$");
    };
    if !first.is_ascii_alphanumeric()
        || label.len() > 64
        || !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-'))
    {
        bail!("invalid thread label `{label}`; expected ^[A-Za-z0-9][A-Za-z0-9_.-]{{0,63}}$");
    }
    Ok(())
}

pub(crate) fn resolve_thread_target(
    label: &str,
    sessions_base: &Path,
    recipe: BuiltinWebRecipe,
) -> Result<Option<ResolvedFollowup>> {
    validate_thread_label(label)?;
    for session in list_sessions_in(sessions_base)? {
        let Some(metadata) = read_followup_metadata(&session.path)? else {
            continue;
        };
        if metadata.recipe != recipe || metadata.thread_label.as_deref() != Some(label) {
            continue;
        }
        return Ok(Some(ResolvedFollowup {
            recipe,
            conversation: WebConversation {
                id: metadata.conversation_id,
                url: metadata.conversation_url,
            },
            source_session_id: Some(metadata.session_id),
            prior_prompt_hash: Some(metadata.prompt_hash),
        }));
    }
    Ok(None)
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
    thread: Option<&str>,
    fresh: bool,
) -> Result<()> {
    let selector_count = usize::from(followup.is_some())
        + usize::from(conversation_var.is_some())
        + usize::from(thread.is_some());
    if selector_count > 1 {
        bail!("--thread, --followup, and --var conversation= are mutually exclusive");
    }
    if fresh && thread.is_none() {
        bail!("--fresh requires --thread");
    }
    if let Some(label) = thread {
        validate_thread_label(label)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn write_followup_metadata(
    session_dir: &Path,
    session_id: &str,
    conversation: &ChatgptConversation,
    prompt_hash: &str,
) -> Result<()> {
    write_followup_metadata_for_recipe(
        session_dir,
        session_id,
        BuiltinWebRecipe::Chatgpt,
        &WebConversation {
            id: conversation.id.clone(),
            url: conversation.url.clone(),
        },
        prompt_hash,
        None,
    )
}

pub(crate) fn write_followup_metadata_for_recipe(
    session_dir: &Path,
    session_id: &str,
    recipe: BuiltinWebRecipe,
    conversation: &WebConversation,
    prompt_hash: &str,
    thread_label: Option<&str>,
) -> Result<()> {
    let path = followup_metadata_path(session_dir);
    let metadata = FollowupMetadata {
        session_id: session_id.to_string(),
        recipe,
        thread_label: thread_label.map(str::to_string),
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
    recipe: BuiltinWebRecipe,
    conversation_id: &str,
    sessions: &[yoetz_core::types::SessionInfo],
) -> Result<Option<FollowupMetadata>> {
    for session in sessions {
        if let Some(metadata) = read_followup_metadata(&session.path)? {
            if metadata.recipe == recipe && metadata.conversation_id == conversation_id {
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
    fn conversation_selectors_are_pairwise_mutually_exclusive() {
        let selectors = [
            (
                Some("session-1"),
                Some("https://chatgpt.com/c/conv-1"),
                None,
            ),
            (Some("session-1"), None, Some("review-pr-341")),
            (
                None,
                Some("https://chatgpt.com/c/conv-1"),
                Some("review-pr-341"),
            ),
        ];
        for (followup, conversation, thread) in selectors {
            let err = validate_followup_args(followup, conversation, thread, false).unwrap_err();
            assert!(err.to_string().contains("mutually exclusive"));
        }
    }

    #[test]
    fn fresh_requires_a_valid_thread_label() {
        assert!(validate_followup_args(None, None, Some("review-pr_341.v2"), true).is_ok());
        assert!(validate_followup_args(None, None, None, true)
            .unwrap_err()
            .to_string()
            .contains("requires --thread"));
        for invalid in ["", "../escape", "-leading", "space label", "é"] {
            assert!(validate_followup_args(None, None, Some(invalid), false).is_err());
        }
        assert!(validate_followup_args(None, None, Some(&"a".repeat(65)), false).is_err());
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

    #[test]
    fn legacy_followup_metadata_defaults_to_chatgpt() {
        let root = tempdir().unwrap();
        fs::write(
            root.path().join("followup.json"),
            r#"{
              "session_id": "legacy-session",
              "conversation_id": "conv-legacy",
              "conversation_url": "https://chatgpt.com/c/conv-legacy",
              "prompt_hash": "legacy-hash"
            }"#,
        )
        .unwrap();

        let metadata = read_followup_metadata(root.path()).unwrap().unwrap();
        assert_eq!(metadata.recipe, BuiltinWebRecipe::Chatgpt);
        assert_eq!(metadata.thread_label, None);
    }

    #[test]
    fn direct_followup_lookup_is_keyed_by_recipe_and_conversation_id() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let chatgpt_session = sessions_dir.join("20260711_000000_chatgpt");
        let claude_session = sessions_dir.join("20260711_010000_claude");
        fs::create_dir_all(&chatgpt_session).unwrap();
        fs::create_dir_all(&claude_session).unwrap();
        write_followup_metadata_for_recipe(
            &chatgpt_session,
            "20260711_000000_chatgpt",
            BuiltinWebRecipe::Chatgpt,
            &WebConversation {
                id: "shared-id".to_string(),
                url: "https://chatgpt.com/c/shared-id".to_string(),
            },
            "chatgpt-hash",
            None,
        )
        .unwrap();
        write_followup_metadata_for_recipe(
            &claude_session,
            "20260711_010000_claude",
            BuiltinWebRecipe::Claude,
            &WebConversation {
                id: "shared-id".to_string(),
                url: "https://claude.ai/chat/shared-id".to_string(),
            },
            "claude-hash",
            None,
        )
        .unwrap();

        let resolved = resolve_followup_target_for_recipe(
            "shared-id",
            &sessions_dir,
            BuiltinWebRecipe::Claude,
            |raw| {
                Ok(WebConversation {
                    id: raw.to_string(),
                    url: format!("https://claude.ai/chat/{raw}"),
                })
            },
        )
        .unwrap();

        assert_eq!(resolved.recipe, BuiltinWebRecipe::Claude);
        assert_eq!(resolved.prior_prompt_hash.as_deref(), Some("claude-hash"));
        assert_eq!(
            resolved.source_session_id.as_deref(),
            Some("20260711_010000_claude")
        );
    }

    #[test]
    fn thread_resolution_uses_newest_matching_recipe_and_skips_legacy_files() {
        let root = tempdir().unwrap();
        let sessions_dir = root.path().join("sessions");
        let legacy = sessions_dir.join("20260711_020000_legacy");
        let old_chatgpt = sessions_dir.join("20260711_000000_chatgpt");
        let new_chatgpt = sessions_dir.join("20260711_030000_chatgpt");
        let claude = sessions_dir.join("20260711_040000_claude");
        for path in [&legacy, &old_chatgpt, &new_chatgpt, &claude] {
            fs::create_dir_all(path).unwrap();
        }
        fs::write(
            legacy.join("followup.json"),
            r#"{
              "session_id": "20260711_020000_legacy",
              "conversation_id": "legacy",
              "conversation_url": "https://chatgpt.com/c/legacy",
              "prompt_hash": "legacy-hash"
            }"#,
        )
        .unwrap();
        for (path, session_id, recipe, conversation_id, hash) in [
            (
                &old_chatgpt,
                "20260711_000000_chatgpt",
                BuiltinWebRecipe::Chatgpt,
                "old-chatgpt",
                "old-hash",
            ),
            (
                &new_chatgpt,
                "20260711_030000_chatgpt",
                BuiltinWebRecipe::Chatgpt,
                "new-chatgpt",
                "new-hash",
            ),
            (
                &claude,
                "20260711_040000_claude",
                BuiltinWebRecipe::Claude,
                "claude-conversation",
                "claude-hash",
            ),
        ] {
            let host = match recipe {
                BuiltinWebRecipe::Chatgpt => "https://chatgpt.com/c/",
                BuiltinWebRecipe::Claude => "https://claude.ai/chat/",
            };
            write_followup_metadata_for_recipe(
                path,
                session_id,
                recipe,
                &WebConversation {
                    id: conversation_id.to_string(),
                    url: format!("{host}{conversation_id}"),
                },
                hash,
                Some("review-pr-341"),
            )
            .unwrap();
        }

        let chatgpt =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .unwrap();
        assert_eq!(chatgpt.conversation.id, "new-chatgpt");
        assert_eq!(chatgpt.prior_prompt_hash.as_deref(), Some("new-hash"));

        let claude =
            resolve_thread_target("review-pr-341", &sessions_dir, BuiltinWebRecipe::Claude)
                .unwrap()
                .unwrap();
        assert_eq!(claude.conversation.id, "claude-conversation");

        assert!(
            resolve_thread_target("unknown-thread", &sessions_dir, BuiltinWebRecipe::Chatgpt)
                .unwrap()
                .is_none()
        );
    }
}
