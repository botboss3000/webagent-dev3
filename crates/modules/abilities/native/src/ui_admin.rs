//! UI-only source aliases retain the protected broker and legacy source permission ceilings.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "read_source"
            | "write_source"
            | "edit_source"
            | "patch_source"
            | "delete_source"
            | "read_directory"
            | "search_source"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "read_source",
    "write_source",
    "edit_source",
    "patch_source",
    "delete_source",
    "read_directory",
    "search_source",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    path: Option<String>,
    broker_id: Option<String>,
    expected_inputs_sha256: Option<String>,
    content: Option<String>,
    old_text: Option<String>,
    new_text: Option<String>,
    patch: Option<String>,
    query: Option<String>,
    offset: Option<u64>,
    limit: Option<u64>,
}
pub fn path(raw: &str, directory: bool) -> Result<String> {
    let value = raw.strip_prefix("/live-checkout/").unwrap_or(raw);
    let value = if value == "ui" || value.starts_with("ui/") {
        value.to_owned()
    } else {
        format!("ui/{value}")
    };
    if value.len() > 240
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'.' | b'_' | b'-'))
        || value
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == ".." || p.starts_with('.'))
    {
        return Err(anyhow!(
            "UI source path must stay below ui/ without hidden or parent components"
        ));
    }
    if !directory {
        let extension = std::path::Path::new(&value)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("");
        if !matches!(
            extension,
            "html" | "css" | "js" | "mjs" | "cjs" | "json" | "svg" | "txt"
        ) {
            return Err(anyhow!("UI Admin edits frontend text assets only"));
        }
    }
    Ok(value)
}
pub fn patch_paths(patch: &str, delete: Option<&str>) -> Result<()> {
    if patch.is_empty() || patch.len() > 1024 * 1024 {
        return Err(anyhow!("Bounded UI patch required"));
    }
    let mut files = std::collections::BTreeSet::new();
    let mut deletions = 0;
    let mut current = None;
    for line in patch.lines() {
        if line.starts_with("GIT binary patch")
            || line.starts_with("rename ")
            || line.starts_with("copy ")
        {
            return Err(anyhow!("Only text UI patches are supported"));
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let parts = rest.split_whitespace().collect::<Vec<_>>();
            if parts.len() != 2 || parts[0].strip_prefix("a/") != parts[1].strip_prefix("b/") {
                return Err(anyhow!("UI patch paths must match"));
            }
            let name = parts[0]
                .strip_prefix("a/")
                .ok_or_else(|| anyhow!("Invalid UI patch header"))?;
            if !name.starts_with("ui/") || path(name, false)? != name {
                return Err(anyhow!("UI patch escaped frontend source"));
            }
            current = Some(name.to_owned());
            files.insert(name.to_owned());
        }
        for prefix in ["--- ", "+++ "] {
            if let Some(raw) = line.strip_prefix(prefix) {
                if current.is_none() {
                    return Err(anyhow!("UI patch lacks its file header"));
                }
                if raw == "/dev/null" {
                    if prefix == "+++ " {
                        deletions += 1;
                    }
                    continue;
                }
                let name = raw
                    .strip_prefix("a/")
                    .or_else(|| raw.strip_prefix("b/"))
                    .ok_or_else(|| anyhow!("Invalid UI patch path"))?;
                if current.as_deref() != Some(name)
                    || !name.starts_with("ui/")
                    || path(name, false)? != name
                {
                    return Err(anyhow!("UI patch file headers disagree"));
                }
            }
        }
        if delete.is_some() && line.starts_with('+') && !line.starts_with("+++") {
            return Err(anyhow!("Deletion patch cannot add content"));
        }
    }
    if files.is_empty() || files.len() > 64 {
        return Err(anyhow!("UI patch must contain 1..64 paths"));
    }
    if let Some(target) = delete {
        if files.len() != 1 || !files.contains(target) || deletions != 1 {
            return Err(anyhow!(
                "Deletion patch must delete exactly its named UI file"
            ));
        }
    }
    Ok(())
}
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    host.authorize(call, name, args, &cancel).await?;
    let q: Args = serde_json::from_value(args.clone())?;
    let operation = match name {
        "read_source" => "file_read",
        "write_source" => "file_write",
        "edit_source" => "file_edit",
        "read_directory" => "file_list",
        "search_source" => "file_search",
        "patch_source" | "delete_source" => "v3_source_apply",
        _ => return Err(anyhow!("Unknown UI tool")),
    };
    let mut forwarded = json!({});
    if let Some(id) = q.broker_id {
        forwarded["broker_id"] = json!(id);
    }
    if let Some(digest) = q.expected_inputs_sha256 {
        forwarded["expected_inputs_sha256"] = json!(digest);
    }
    if matches!(name, "patch_source" | "delete_source") {
        let patch = q.patch.ok_or_else(|| anyhow!("UI patch required"))?;
        let target = if name == "delete_source" {
            Some(path(
                q.path
                    .as_deref()
                    .ok_or_else(|| anyhow!("Deletion path required"))?,
                false,
            )?)
        } else {
            None
        };
        patch_paths(&patch, target.as_deref())?;
        forwarded["patch"] = json!(patch);
    } else {
        forwarded["path"] = json!(path(
            q.path.as_deref().unwrap_or("ui"),
            matches!(name, "read_directory" | "search_source")
        )?);
        for (key, value) in [
            ("content", q.content),
            ("old_text", q.old_text),
            ("new_text", q.new_text),
            ("query", q.query),
        ] {
            if let Some(value) = value {
                forwarded[key] = json!(value);
            }
        }
        if let Some(offset) = q.offset {
            forwarded["offset"] = json!(if name == "read_directory" {
                offset.saturating_sub(1)
            } else {
                offset
            });
        }
        if let Some(limit) = q.limit {
            if name == "read_directory" && limit > 100 {
                return Err(anyhow!("Directory page limit is 100"));
            }
            forwarded["limit"] = json!(limit);
        }
    }
    if !crate::is_read(name) {
        forwarded["request_id"] = json!(format!(
            "ui-{}",
            digest(&json!([call.sid, call.run_id, call.pending_id]))
        ));
    }
    host.ui_admin(call.sid, call.run_id, name, operation, &forwarded, cancel)
        .await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frontend_confinement_rejects_backend_and_mixed_patches() {
        for value in [
            "../src/main.rs",
            "/state/config.json",
            "ui/../src/main.rs",
            "ui/.hidden.js",
            "ui/main.rs",
        ] {
            assert!(path(value, false).is_err());
        }
        assert_eq!(path("chat/chat.css", false).unwrap(), "ui/chat/chat.css");
        let patch="diff --git a/ui/test.css b/ui/test.css\n--- a/ui/test.css\n+++ b/ui/test.css\n@@ -1 +1 @@\n-a\n+b\n";
        assert!(patch_paths(patch, None).is_ok());
        assert!(patch_paths(
            &patch.replace("--- a/ui/test.css", "--- a/ui/other.css"),
            None
        )
        .is_err());
        assert!(patch_paths(&patch.replace("ui/test.css", "src/main.rs"), None).is_err());
    }
}
