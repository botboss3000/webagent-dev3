//! Code Index rules: maintenance argument projection and the bounded configuration
//! excerpts that accompany a feature-bound source read.
//!
//! Index metadata is untrusted navigation data. Only native fixed configuration aliases
//! appoint runtime reads; private snapshots never mix with live settings. Snapshot
//! roots can never perform maintenance mutations.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

/// The largest live configuration file a source read may excerpt.
pub const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_POINTERS: usize = 64;

/// Normalize maintenance action fields; snapshot roots can never perform mutations.
pub fn maintenance_arguments(args: &Value) -> Result<Value> {
    let input = args
        .as_object()
        .ok_or_else(|| anyhow!("Maintenance object required"))?;
    const KNOWN: &[&str] = &[
        "action",
        "root",
        "task_id",
        "task_key",
        "scope",
        "status",
        "feature",
        "offset",
        "limit",
        "entry_id",
        "request_id",
        "expected_revision",
        "source_hash",
        "checks",
        "remaining_work",
        "reconciliation",
        "claim_id",
        "configuration_assessment",
    ];
    if input.keys().any(|key| !KNOWN.contains(&key.as_str())) {
        bail!("Unknown maintenance argument");
    }
    let action = args["action"]
        .as_str()
        .ok_or_else(|| anyhow!("Maintenance action required"))?;
    let fields: &[&str] = match action {
        "list" => &[
            "root", "task_id", "task_key", "scope", "status", "feature", "offset", "limit",
        ],
        "get" => &["root", "entry_id", "offset", "limit"],
        "sync" => &["root", "task_id", "task_key", "scope", "request_id"],
        "claim" => &[
            "root",
            "task_id",
            "task_key",
            "entry_id",
            "expected_revision",
            "request_id",
            "claim_id",
        ],
        "release" => &[
            "root",
            "task_id",
            "task_key",
            "entry_id",
            "expected_revision",
            "request_id",
            "remaining_work",
            "claim_id",
        ],
        "record" => &[
            "root",
            "task_id",
            "task_key",
            "entry_id",
            "expected_revision",
            "request_id",
            "source_hash",
            "status",
            "checks",
            "remaining_work",
            "reconciliation",
            "claim_id",
            "configuration_assessment",
        ],
        _ => bail!("Unknown maintenance action"),
    };
    let mut output = serde_json::Map::new();
    output.insert("action".into(), json!(action));
    for key in fields {
        if let Some(value) = input.get(*key).filter(|v| !v.is_null()) {
            output.insert((*key).into(), value.clone());
        }
    }
    output.entry("root").or_insert(json!("live"));
    if output.get("root") != Some(&json!("live")) && !matches!(action, "list" | "get") {
        bail!("Maintenance snapshots are read-only; select an isolated broker task");
    }
    Ok(Value::Object(output))
}

/// Preserve UTF-8 while applying the same byte budget as source excerpts.
#[must_use]
pub fn bounded(text: &str, budget: usize) -> (&str, bool) {
    let mut end = text.len().min(budget);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (&text[..end], end < text.len())
}

/// The JSON pointers a configuration reference requests, with chat profile overrides.
#[must_use]
pub fn pointers(reference: &Value) -> Vec<String> {
    let mut result = Vec::new();
    for pointer in reference["pointers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .take(16)
    {
        if !pointer.starts_with('/') || pointer.len() > 192 {
            continue;
        }
        result.push(pointer.to_owned());
        if reference["profile_overrides"] == true {
            if let Some(tail) = pointer.strip_prefix("/chat_common/") {
                for profile in ["chat_desktop", "chat_mobile", "chat_widget"] {
                    result.push(format!("/{profile}/{tail}"));
                }
            }
        }
    }
    result.truncate(MAX_POINTERS);
    result
}

/// Whether a live configuration alias is read, snapshot-only or not permitted.
#[must_use]
pub fn runtime_state(path: &str, published: bool, administrator: bool) -> &'static str {
    if !published {
        "snapshot_only"
    } else if path == "crates/modules/context/context-compaction.json" && !administrator {
        "not_permitted"
    } else {
        "read"
    }
}

/// The parsed current file and its SHA-256, within [`MAX_CONFIG_BYTES`].
pub fn read_json(path: &std::path::Path) -> Result<(Value, String)> {
    // A checkout-relative path is the current live file; an absolute one is a fixture.
    webagent_configuration::live::parsed_path(path, MAX_CONFIG_BYTES, |bytes| {
        Ok((
            serde_json::from_slice(bytes)?,
            format!("{:x}", Sha256::digest(bytes)),
        ))
    })
}

/// The bytes of configuration excerpt text already attached to a source result.
#[must_use]
pub fn snippet_bytes(source: &Value) -> usize {
    source["configuration_snippets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry["text"].as_str())
        .map(str::len)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_cannot_smuggle_identity_or_unused_mutations() {
        assert!(maintenance_arguments(&json!({"action":"sync","user_id":"other"})).is_err());
        let a = maintenance_arguments(
            &json!({"action":"list","root":null,"task_id":null,"checks":{"documentation":"verified"}}),
        )
        .unwrap();
        assert_eq!(a, json!({"action":"list","root":"live"}));
    }

    #[test]
    fn private_and_published_snapshots_cannot_be_written() {
        assert!(maintenance_arguments(&json!({"action":"record","root":"published"})).is_err());
        assert!(maintenance_arguments(&json!({"action":"sync","root":"webagent-v3"})).is_err());
        assert!(
            maintenance_arguments(&json!({"action":"get","root":"published","entry_id":"e"}))
                .is_ok()
        );
    }

    #[test]
    fn runtime_access_respects_snapshot_and_administrator_scope() {
        assert_eq!(runtime_state("ui/chat/chat.json", true, false), "read");
        assert_eq!(
            runtime_state(
                "crates/modules/context/context-compaction.json",
                true,
                false
            ),
            "not_permitted"
        );
        assert_eq!(
            runtime_state("crates/modules/context/context-compaction.json", true, true),
            "read"
        );
        assert_eq!(
            runtime_state("ui/chat/chat.json", false, true),
            "snapshot_only"
        );
    }

    #[test]
    fn current_json_hash_changes_and_oversized_or_invalid_files_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("code-index-config.json");
        std::fs::write(&path, br#"{"color":"blue"}"#).unwrap();
        let (before, hash) = read_json(&path).unwrap();
        assert_eq!(before.pointer("/color"), Some(&json!("blue")));
        assert!(before.pointer("/missing").is_none());
        std::fs::write(&path, br#"{"color":"red"}"#).unwrap();
        let (after, next_hash) = read_json(&path).unwrap();
        assert_eq!(after.pointer("/color"), Some(&json!("red")));
        assert_ne!(hash, next_hash);
        std::fs::write(&path, b"invalid JSON").unwrap();
        assert!(read_json(&path).is_err());
        std::fs::write(&path, vec![b' '; MAX_CONFIG_BYTES as usize + 1]).unwrap();
        assert!(read_json(&path).is_err());
    }

    #[test]
    fn utf8_budget_and_profile_pointers() {
        assert_eq!(bounded("🙂 blue", 3), ("", true));
        assert_eq!(bounded("🙂 blue", 5), ("🙂 ", true));
        let value = json!({"pointers":["/chat_common/message_bubbles/user/bg_dark"],"profile_overrides":true});
        assert_eq!(
            pointers(&value),
            vec![
                "/chat_common/message_bubbles/user/bg_dark",
                "/chat_desktop/message_bubbles/user/bg_dark",
                "/chat_mobile/message_bubbles/user/bg_dark",
                "/chat_widget/message_bubbles/user/bg_dark"
            ]
        );
        assert_eq!(
            snippet_bytes(
                &json!({"configuration_snippets":[{"text":"🙂"},{"status":"unset"},{"text":"red"}]})
            ),
            7
        );
    }
}
