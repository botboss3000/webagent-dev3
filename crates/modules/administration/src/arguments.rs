//! Projections from model arguments to what the broker or worker receives. Each keeps
//! only the keys its action uses and rejects unknown keys, so a model cannot smuggle a
//! field the host would otherwise trust.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Map, Value};
use webagent_configuration::digest;

/// A caller-chosen idempotency key: 1–80 ASCII letters, digits, `_` or `-`.
pub fn request_id(args: &Value) -> Result<&str> {
    let value = args["request_id"].as_str().unwrap_or("");
    if value.is_empty()
        || value.len() > 80
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!("Canonical bounded request_id required");
    }
    Ok(value)
}

/// Copy the keys of `object` named in `keys` into a new object.
fn pick(object: &Map<String, Value>, keys: &[&str]) -> Map<String, Value> {
    let mut picked = Map::new();
    for key in keys {
        if let Some(value) = object.get(*key) {
            picked.insert((*key).into(), value.clone());
        }
    }
    picked
}

/// The source-relative path of a `file_edit`/`file_write` that enters an owned broker
/// draft, or `None` when the write targets an explicit runtime or workspace path.
/// The source-relative part of a checkout path: under the configured checkout root, or
/// the names prompts and history use, `/code/` and the image's compatibility link
/// `/live-checkout/`.
fn checkout_relative(raw: &str) -> Option<&str> {
    let root = webagent_configuration::paths::checkout();
    let root = format!("{}/", root.to_string_lossy().trim_end_matches('/'));
    raw.strip_prefix(root.as_str())
        .or_else(|| raw.strip_prefix("/code/"))
        .or_else(|| raw.strip_prefix("/live-checkout/"))
}

pub fn implicit_source_write(name: &str, args: &Value) -> Result<Option<String>> {
    if !matches!(name, "file_edit" | "file_write") {
        return Ok(None);
    }
    let raw = args["path"]
        .as_str()
        .ok_or_else(|| anyhow!("Source write path required"))?;
    let relative = if let Some(value) = checkout_relative(raw) {
        value
    } else if raw.starts_with('/') {
        return Ok(None);
    } else {
        raw
    };
    if relative.is_empty()
        || relative.len() > 240
        || relative
            .split('/')
            .any(|p| p.is_empty() || p == "." || p == "..")
    {
        bail!("Source path must be relative without traversal");
    }
    if matches!(
        relative.split('/').next(),
        Some("data" | "workspaces" | "target" | ".git" | "build-jobs" | "deployment-secrets")
    ) {
        bail!("Use the owning runtime or workspace API for this excluded source path");
    }
    Ok(Some(relative.to_owned()))
}

/// A broker draft request for a Codebase Admin file tool, or `None` when the call keeps
/// the live-checkout worker path. Broker IDs select work; the host attaches identity.
pub fn broker_file(sid: &str, pending_id: &str, name: &str, args: &Value) -> Result<Option<Value>> {
    let legacy = args.get("task_id").is_some_and(|v| !v.is_null());
    let selected = args.get("broker_id").is_some_and(|v| !v.is_null());
    if legacy && selected {
        bail!("Use broker_id or legacy task_id, not both");
    }
    // Ordinary reads keep the worker contract; private reads select returned work.
    if !legacy && !selected && !matches!(name, "file_edit" | "file_write") {
        return Ok(None);
    }
    let raw = args["path"].as_str().unwrap_or("");
    let path = checkout_relative(raw).unwrap_or(raw);
    if !legacy && !selected && raw.starts_with('/') && checkout_relative(raw).is_none() {
        return Ok(None);
    }
    if selected && path.starts_with('/') {
        bail!("Broker ID requires a source-relative path");
    }
    if matches!(name, "file_edit" | "file_write") && !legacy {
        implicit_source_write(name, args)?;
    }
    let mut forwarded = args.clone();
    forwarded["path"] = json!(if path == "." { "" } else { path });
    if (!legacy || matches!(name, "file_edit" | "file_write")) && forwarded["request_id"].is_null()
    {
        forwarded["request_id"] = json!(retry_id(sid, pending_id));
    }
    Ok(Some(forwarded))
}

/// A request ID that is stable across retries of one tool call in one session.
#[must_use]
pub fn retry_id(sid: &str, pending_id: &str) -> String {
    format!("broker-{}", digest(&json!([sid, pending_id])))
}

/// Live checkout files without a broker draft: drop broker selectors and nulls, and
/// refuse task-write keys that would otherwise be silently ignored.
pub fn live_file(args: &Value) -> Result<Value> {
    let mut live = args.clone();
    if let Some(object) = live.as_object_mut() {
        object.remove("task_id");
        object.remove("broker_id");
        object.retain(|_, value| !value.is_null());
        if object.contains_key("expected_inputs_sha256") || object.contains_key("request_id") {
            bail!("request_id and expected_inputs_sha256 apply only to broker task files (broker_id); remove them for workspace or live checkout files");
        }
    }
    Ok(live)
}

/// Whether a Git call targets the shared live checkout. That checkout is published
/// release source: inspection and pushing an existing commit are allowed, while source
/// and index changes stay with the broker.
pub fn live_git_operation(name: &str, args: &Value) -> Result<bool> {
    if args["repo"] != "__root__" {
        return Ok(false);
    }
    if name != "git_tool"
        || !matches!(
            args["operation"].as_str(),
            Some("status" | "diff" | "log" | "show" | "branch_list" | "tag_list" | "push")
        )
    {
        bail!("Live checkout Git supports inspection and push only; commit through the release broker");
    }
    Ok(true)
}

/// The run `read_diagnostics` targets: the current run for an omitted, empty or
/// `current` ID, a canonical UUID otherwise, and `None` for anything else.
#[must_use]
pub fn diagnostics_target<'a>(args: &'a Value, current: &'a str) -> Option<&'a str> {
    match args.get("run_id") {
        None | Some(Value::Null) => Some(current),
        Some(Value::String(value)) if value.is_empty() || value == "current" => Some(current),
        Some(Value::String(value)) => uuid::Uuid::parse_str(value)
            .ok()
            .filter(|id| id.to_string() == *value)
            .map(|_| value.as_str()),
        _ => None,
    }
}

/// `repository_access` takes only an optional `verify_remote` boolean; returns it.
pub fn repository_access(args: &Value) -> Result<bool> {
    let valid = args.as_object().is_some_and(|object| {
        object.keys().all(|key| key == "verify_remote")
            && object.get("verify_remote").is_none_or(Value::is_boolean)
    });
    if !valid {
        bail!("repository_access accepts only verify_remote boolean");
    }
    Ok(args["verify_remote"].as_bool().unwrap_or(true))
}

/// Performance actions that run work and so register foreground renewal authority.
#[must_use]
pub fn performance_effect_action(args: &Value) -> bool {
    matches!(
        args["action"].as_str(),
        Some("benchmark" | "profile_cpu" | "profile_allocations")
    )
}

/// A successful receipt whose job is still queued or running.
#[must_use]
pub fn performance_pending(receipt: &Value) -> bool {
    receipt["status"] == "success"
        && matches!(
            receipt["output"]["status"].as_str(),
            Some("queued" | "running")
        )
}

/// Only a running, uncancelled run may renew performance job authority.
#[must_use]
pub fn performance_run_live(run: &Value) -> bool {
    run["status"] == "running" && run["error"] != "Cancelled by user"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn some(value: Result<Option<Value>>) -> Value {
        value.expect("projection").expect("broker draft")
    }

    #[test]
    fn automatic_broker_file_selection_is_retry_stable_and_runtime_scoped() {
        let args = json!({"path":"src/app.rs","broker_id":"new"});
        let first = some(broker_file("session-one", "call-one", "file_read", &args));
        assert_eq!(first["broker_id"], "new");
        assert!(first["request_id"].is_string());
        assert_eq!(
            first,
            some(broker_file("session-one", "call-one", "file_read", &args))
        );
        let other = some(broker_file("session-two", "call-one", "file_read", &args));
        assert_ne!(first["request_id"], other["request_id"]);
        for args in [
            json!({"path":"src/a.rs"}),
            json!({"query":"fn", "regex":true,"path":"src"}),
            json!({"path":"/workspaces/own/file"}),
        ] {
            assert!(broker_file("s", "c", "file_read", &args)
                .expect("projection")
                .is_none());
        }
        for args in [
            json!({"path":"src/a.rs","broker_id":"new","task_id":"old-task"}),
            json!({"path":"/state/private","broker_id":"new"}),
        ] {
            assert!(broker_file("s", "c", "file_read", &args).is_err());
        }
        let selected = json!({"path":"/live-checkout/src/a.rs","broker_id":"selected-work"});
        assert_eq!(
            some(broker_file("s", "c", "file_read", &selected))["path"],
            "src/a.rs"
        );
    }

    #[test]
    fn implicit_source_writes_are_broker_scoped() {
        let write = |name, path: &str| implicit_source_write(name, &json!({"path":path}));
        assert_eq!(
            write("file_write", "src/app.rs").ok(),
            Some(Some("src/app.rs".into()))
        );
        assert_eq!(
            write("file_edit", "/live-checkout/ui/app.css").ok(),
            Some(Some("ui/app.css".into()))
        );
        assert_eq!(write("file_write", "/workspaces/own/file").ok(), Some(None));
        assert_eq!(write("file_read", "src/app.rs").ok(), Some(None));
        for path in [
            "/live-checkout/../data/key",
            "../src/app.rs",
            "data/config/chat.json",
        ] {
            assert!(write("file_write", path).is_err(), "{path}");
        }
    }

    #[test]
    fn live_files_refuse_task_write_keys() {
        let live = live_file(&json!({"path":"a","task_id":null,"broker_id":null,"limit":null}));
        assert_eq!(live.ok(), Some(json!({"path":"a"})));
        assert!(live_file(&json!({"path":"a","request_id":"r"})).is_err());
    }

    #[test]
    fn live_git_route_is_read_or_push_only() {
        let git = |operation: &str, repo: &str| {
            live_git_operation("git_tool", &json!({"repo":repo,"operation":operation}))
        };
        assert_eq!(git("status", "__root__").ok(), Some(true));
        assert_eq!(git("push", "__root__").ok(), Some(true));
        assert_eq!(git("push", "webagent-v3").ok(), Some(false));
        assert!(git("add", "__root__").is_err());
        assert!(live_git_operation("commit_and_push", &json!({"repo":"__root__"})).is_err());
    }

    #[test]
    fn current_aliases_and_canonical_run_ids() {
        let current = "11111111-1111-4111-8111-111111111111";
        for args in [
            json!({}),
            json!({"run_id":null}),
            json!({"run_id":""}),
            json!({"run_id":"current"}),
        ] {
            assert_eq!(diagnostics_target(&args, current), Some(current));
        }
        let explicit = json!({"run_id":"22222222-2222-4222-8222-222222222222"});
        assert_eq!(
            diagnostics_target(&explicit, current),
            explicit["run_id"].as_str()
        );
        for args in [
            json!({"run_id":"not-a-run"}),
            json!({"run_id":42}),
            json!({"run_id":" current "}),
        ] {
            assert!(diagnostics_target(&args, current).is_none());
        }
    }

    #[test]
    fn repository_access_takes_only_a_verify_flag() {
        assert_eq!(repository_access(&json!({})).ok(), Some(true));
        assert_eq!(
            repository_access(&json!({"verify_remote":false})).ok(),
            Some(false)
        );
        assert!(repository_access(&json!({"verify_remote":"no"})).is_err());
        assert!(repository_access(&json!({"token":"x"})).is_err());
        assert!(repository_access(&json!([])).is_err());
    }

    #[test]
    fn request_ids_are_bounded_and_canonical() {
        assert_eq!(
            request_id(&json!({"request_id":"seed-1_a"})).ok(),
            Some("seed-1_a")
        );
        for value in [json!(""), json!("a b"), json!("x".repeat(81)), json!(7)] {
            assert!(request_id(&json!({ "request_id": value })).is_err());
        }
    }

    #[test]
    fn durable_wait_only_waits_for_successful_nonterminal_receipts() {
        for state in ["queued", "running"] {
            assert!(performance_pending(
                &json!({"status":"success","output":{"status":state}})
            ));
        }
        for state in ["completed", "failed", "cancelled", "interrupted"] {
            assert!(!performance_pending(
                &json!({"status":"success","output":{"status":state}})
            ));
        }
        assert!(!performance_pending(
            &json!({"status":"error","output":{"status":"queued"}})
        ));
        assert!(!performance_pending(
            &json!({"status":"success","output":{}})
        ));
    }

    #[test]
    fn terminal_or_cancelled_performance_runs_cannot_renew_job_authority() {
        assert!(performance_run_live(&json!({"status":"running"})));
        for state in [
            "queued",
            "waiting_approval",
            "stopped",
            "cancelled",
            "completed",
            "failed",
            "recovery",
            "paused",
        ] {
            assert!(!performance_run_live(&json!({"status":state})));
        }
        let cancelled = json!({"status":"running","error":"Cancelled by user"});
        assert!(!performance_run_live(&cancelled));
    }

    #[test]
    fn only_effect_actions_register_foreground_renewal_authority() {
        for action in ["benchmark", "profile_cpu", "profile_allocations"] {
            assert!(performance_effect_action(&json!({"action":action})));
        }
        for action in ["status", "artifact", "cancel", "unknown"] {
            assert!(!performance_effect_action(&json!({"action":action})));
        }
        assert!(!performance_effect_action(&json!({})));
    }
}
