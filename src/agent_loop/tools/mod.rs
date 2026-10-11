use anyhow::{bail, Context, Result};
// Single and parallel turns share the same Rust dispatch for native and worker tools.
#[path = "../../../plugins/abilities/Web/web_access/adapter.rs"]
pub(crate) mod web_access;
#[path = "../../../plugins/abilities/Core/decision/adapter.rs"]
pub(crate) mod core_decision;
#[path = "../../../plugins/abilities/Core/decision/directory_adapter.rs"]
pub(crate) mod core_directory_decision;
pub(crate) mod code_index_retrieval;
#[path = "../../../plugins/abilities/Core/base/adapter.rs"]
pub(crate) mod core_utilities;
#[path = "../../../plugins/abilities/Core/base/session_adapter.rs"]
pub(crate) mod core_session;
#[path = "../../../plugins/abilities/Core/base/skills_adapter.rs"]
pub(crate) mod core_skills;
#[path = "../../../plugins/abilities/Core/base/prompt_adapter.rs"]
pub(crate) mod core_prompt;
#[path = "../../../plugins/abilities/Core/base/search_adapter.rs"]
pub(crate) mod core_search;
#[path = "../../../plugins/abilities/Core/base/tool_search_adapter.rs"]
pub(crate) mod core_tool_search;
#[path = "../../../plugins/abilities/Core/base/self_edit_adapter.rs"]
pub(crate) mod core_self_edit;
#[path = "../../../plugins/abilities/Core/base/task_adapter.rs"]
pub(crate) mod core_task;
#[path = "../../../plugins/abilities/Core/base/mode_adapter.rs"]
pub(crate) mod core_mode;
#[path = "../../../plugins/abilities/Core/base/compaction_adapter.rs"]
pub(crate) mod core_compaction;
#[path = "../../../plugins/abilities/Core/base/recall_adapter.rs"]
pub(crate) mod core_recall;
mod dispatch;
pub(crate) mod administrator;
pub(crate) use dispatch::{call, call_foreground};
#[cfg(test)]
pub(crate) use dispatch::call_raw;
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use tokio_util::sync::CancellationToken;

pub fn is_read(name: &str) -> bool {
    matches!(
        name,
        "task_status" | "task_wait" | "task_cancel"
            | "search_tools" | "list_tools" | "load_tool"
            | "decide_with_jev"
            | "score_directory_with_jev"
            | "get_time"
            | "get_date"
            | "calculate"
            | "read_attachment"
            | "search_this_session"
            | "list_skills"
            | "load_skill"
            | "read_own_prompt"
            | "session_search"
            | "similar_sessions"
            | "get_task_checkpoint"
            | "compact_context"
            | "recall_compacted"
            | "web_search"
            | "get_weather"
            | "maps_geocode"
            | "file_list"
            | "file_read"
            | "file_search"
            | "source_review"
            | "state_inspect"
            | "prompt_slot_read"
            | "set_execution_mode"
            | "code_index_lookup"
            | "code_index_coverage"
            | "code_index_trace"
            | "code_index_progress"
            | "terminal_read"
            | "terminal_wait"
            | "terminal_list"
            | "v3_workspace_status"
            | "v3_workspace_receipt"
            | "v3_build_status"
            | "v3_source_list"
            | "v3_source_read"
            | "v3_worker_job"
            | "read_diagnostics"
            | "dev_browser_url"
            | "get_tunnel_url"
            | "repository_access"
    )
}

pub fn schemas() -> Vec<Value> {
    static SCHEMAS: OnceLock<Vec<Value>> = OnceLock::new();
    SCHEMAS.get_or_init(|| { let mut schemas: Vec<Value> = [
        ("file_list", "List a directory by absolute container path or relative live-checkout path", json!({"path":{"type":"string"}}), vec![]),
        ("file_read", "Read a bounded page of file lines from a container or live-checkout path", json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1,"maximum":10000000},"limit":{"type":"integer","minimum":1,"maximum":200}}), vec!["path"]),
        ("file_search", "Search container or live-checkout files by literal or regex query; outline comments in one file", json!({"path":{"type":"string"},"query":{"type":"string"},"regex":{"type":"boolean"},"file_glob":{"type":"string"},"comments_only":{"type":"boolean"}}), vec![]),
        ("file_write", "Write a container or live-checkout file; policy-gated", json!({"path":{"type":"string"},"content":{"type":"string"}}), vec!["path","content"]),
        ("file_edit", "Replace exactly one matching span in an existing container or live-checkout file; policy-gated", json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"expected_sha256":{"type":"string"}}), vec!["path","old_text","new_text"]),
        ("workspace_seed", "Copy the audited WebAgent v3 source snapshot into this session workspace and create a Git baseline; policy-gated", json!({}), vec![]),
        ("source_review", "Review tracked and new changes in the seeded v3 source without modifying its Git index", json!({}), vec![]),
        ("prompt_slot_read", "List or read the current agent's ordered instruction slots", json!({"action":{"type":"string","enum":["list","get"]},"slot_name":{"type":"string"}}), vec!["action"]),
        ("prompt_slot_write", "Create, update or delete a current-agent instruction slot; policy-gated", json!({"action":{"type":"string","enum":["put","delete"]},"slot_name":{"type":"string"},"content":{"type":"string"},"expected_revision":{"type":"integer","minimum":0},"order_index":{"type":"integer"},"lock":{"type":"boolean"},"merge_mode":{"type":"string","enum":["replace","append"]}}), vec!["action","slot_name","expected_revision"]),
        ("state_inspect", "Read bounded public metadata for the current session; catalog, session, chat, or runs", json!({"resource":{"type":"string","enum":["catalog","session","chat","runs"]},"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":50}}), vec!["resource"]),
        ("source_patch", "Apply a checked text patch to the seeded v3 source; policy-gated", json!({"patch":{"type":"string"},"expected_review_sha256":{"type":"string"}}), vec!["patch","expected_review_sha256"]),
        ("promotion_propose", "Submit the current reviewed v3 source patch for administrator approval; does not change live source", json!({"expected_review_sha256":{"type":"string"},"summary":{"type":"string"}}), vec!["expected_review_sha256","summary"]),
        ("shell", "Execute a shell command from the live development checkout; policy-gated", json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["command"]),
        ("python", "Execute Python from the live development checkout; policy-gated", json!({"code":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["code"]),
    ].into_iter().map(|(name, description, properties, required)| json!({
        "type":"function", "name":name, "description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect();
        for schema in &mut schemas {
            if matches!(schema["name"].as_str(), Some("file_list"|"file_read"|"file_search"|"file_edit"|"file_write")) {
                let properties = schema["parameters"]["properties"].as_object_mut().unwrap();
                properties.insert("task_id".into(), json!({"type":"string","pattern":"^[A-Za-z0-9_-]{8,80}$","description":"Omit or set null for live files; never invent a placeholder ID. Supply one stable ID for an isolated build task; missing tasks are created automatically. Paths then are source-relative."}));
                if matches!(schema["name"].as_str(), Some("file_edit"|"file_write")) {
                    schema["parameters"]["properties"]["expected_inputs_sha256"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Task digest from the latest task read/list/review; required when writing an existing task."});
                    schema["parameters"]["properties"]["request_id"] = json!({"type":"string","pattern":"^[A-Za-z0-9_-]{8,80}$","description":"Stable task-write request ID. Reuse only for identical retries; inspect build_work receipt after uncertainty."});
                }
                if schema["name"]=="file_list" {
                    schema["parameters"]["properties"]["offset"] = json!({"type":"integer","minimum":0});
                    schema["parameters"]["properties"]["limit"] = json!({"type":"integer","minimum":1,"maximum":100});
                }
                let description = schema["description"].as_str().unwrap().to_owned();
                schema["description"] = json!(format!("{description}. Optional task_id selects isolated source for a build and automatically creates the task; omit for explicit live edits or runtime configuration. Never deploys."));
            }
        }
        schemas.extend(crate::background_tasks::schemas());
        schemas.extend(administrator::schemas());
        schemas.extend(web_access::schemas());
        schemas.extend(core_decision::schemas());
        schemas.extend(core_directory_decision::schemas());
        schemas.extend(core_utilities::schemas());
        schemas.extend(core_session::schemas());
        schemas.extend(core_skills::schemas());
        schemas.extend(core_prompt::schemas());
        schemas.extend(core_search::schemas());
        schemas.extend(core_tool_search::schemas());
        schemas.extend(core_self_edit::schemas());
        schemas.extend(core_task::schemas());
        schemas.extend(core_compaction::schemas());
        schemas.extend(core_recall::schemas());
        schemas.extend(core_mode::schemas());
        // Responses may require every property: nullable optional fields keep
        // live file calls and action-specific build arguments representable.
        for schema in &mut schemas {
            if matches!(schema["name"].as_str(), Some("file_list"|"file_read"|"file_search"|"file_edit"|"file_write"|"build_work")) {
                let required = schema["parameters"]["required"].as_array().cloned().unwrap_or_default();
                for (key, property) in schema["parameters"]["properties"].as_object_mut().unwrap() {
                    if required.iter().any(|value| value.as_str() == Some(key.as_str())) { continue; }
                    if let Some(kind) = property["type"].as_str().map(str::to_owned) {
                        property["type"] = json!([kind, "null"]);
                    }
                    if let Some(values) = property.get_mut("enum").and_then(Value::as_array_mut) {
                        if !values.contains(&Value::Null) { values.push(Value::Null); }
                    }
                }
            }
        }
        schemas
    }).clone()
}

pub async fn execute(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    cancel: CancellationToken,
) -> Result<Value> {
    if !matches!(
        name,
        "file_list"
            | "file_read"
            | "file_search"
            | "file_write"
            | "file_edit"
            | "workspace_seed"
            | "source_review"
            | "source_patch"
            | "shell"
            | "python"
            | "code_index_scan"
            | "code_index_lookup"
            | "code_index_coverage"
            | "code_index_trace"
            | "code_index_progress"
            | "git_tool"
            | "resolve_conflict"
            | "commit_and_push"
            | "terminal_open"
            | "terminal_read"
            | "terminal_send"
            | "terminal_wait"
            | "terminal_list"
            | "terminal_close"
            | "v3_workspace_status"
            | "v3_workspace_action"
            | "v3_cargo_task"
    ) {
        bail!("unknown tool");
    }
    execute_trusted(name,args,session,socket,None,cancel).await
}

// Only Rust supplies trusted browser/host metadata; it is not a model schema.
pub(crate) async fn execute_trusted(
    name:&str,args:&Value,session:&str,socket:&std::path::Path,
    trusted:Option<Value>,cancel:CancellationToken,
)->Result<Value> {
    uuid::Uuid::parse_str(session).context("invalid workspace identifier")?;
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let mut stream = UnixStream::connect(socket)
        .await
        .context("tool worker unavailable")?;
    let timeout = args["timeout_seconds"]
        .as_u64()
        .unwrap_or(120)
        .clamp(1, 600);
    let request_id = uuid::Uuid::new_v4().to_string();
    let request = json!({"id":request_id,"name":name,
        "arguments":args,"workspace":session,"timeout_seconds":timeout,"trusted":trusted});
    let mut encoded = serde_json::to_vec(&request)?;
    if encoded.len() > 1024 * 1024 {
        bail!("tool input exceeds limit");
    }
    encoded.push(b'\n');
    tokio::select! {
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = async {
            stream.write_all(&encoded).await?;
            let mut reader = BufReader::new(stream).take(2 * 1024 * 1024);
            let mut response = String::new();
            reader.read_line(&mut response).await?;
            if response.is_empty() { bail!("tool worker disconnected"); }
            let value = serde_json::from_str::<Value>(&response)?;
            if value["id"].as_str() != Some(request_id.as_str()) {
                bail!("tool response identifier mismatch");
            }
            Ok(value)
        } => result
    }
}
