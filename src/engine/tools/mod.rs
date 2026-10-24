use anyhow::{bail, Result};
pub(crate) use webagent_engine::tools::outcome;
// Single and parallel turns share the same Rust dispatch for native and worker tools.
#[path = "../../../plugins/abilities/Web/web_access/adapter.rs"]
pub(crate) mod web_access;
#[path = "../../../plugins/abilities/Core/decision/adapter.rs"]
pub(crate) mod core_decision;
#[path = "../../../plugins/abilities/Core/decision/directory_adapter.rs"]
pub(crate) mod core_directory_decision;
pub(crate) mod code_index_retrieval;
#[path = "../../../plugins/abilities/Administrator/code_index/configuration_adapter.rs"]
pub(crate) mod code_index_configuration;
#[path = "../../../plugins/abilities/Administrator/code_index/maintenance_adapter.rs"]
pub(crate) mod code_index_maintenance;
#[path = "../../../plugins/abilities/Core/base/adapter.rs"]
pub(crate) mod core_utilities;
#[path = "../../../plugins/abilities/Core/base/session_adapter.rs"]
pub(crate) mod core_session;
#[path = "../../../plugins/abilities/Core/base/skills_adapter.rs"]
pub(crate) mod core_skills;
#[path = "../../../plugins/abilities/Core/base/context_assets_adapter.rs"]
pub(crate) mod core_context_assets;
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
#[path = "../../../crates/modules/abilities/host/user_files.rs"]
pub(crate) mod core_user_files;
#[path = "../../../crates/modules/abilities/host/authority.rs"]
pub(crate) mod core_authority;
#[path = "../../../crates/modules/abilities/host/model_switcher.rs"]
pub(crate) mod core_model_switcher;
#[path = "../../../crates/modules/abilities/host/agent_management.rs"]
pub(crate) mod core_agent_management;
#[path = "../../../crates/modules/abilities/host/image_vision.rs"]
pub(crate) mod core_image_vision;
#[path = "../../../crates/modules/abilities/host/image_generation.rs"]
pub(crate) mod core_image_generation;
#[path = "../../../crates/modules/abilities/host/app_control.rs"]
pub(crate) mod core_app_control;
#[path = "../../../crates/modules/abilities/host/ui_admin.rs"]
pub(crate) mod core_ui_admin;
#[path = "../../../crates/modules/abilities/host/functions.rs"]
pub(crate) mod core_functions;
#[path = "../../../crates/modules/abilities/host/orchestration.rs"]
pub(crate) mod core_orchestration;
#[path = "../../../crates/modules/abilities/host/base_extras.rs"]
pub(crate) mod core_base_extras;
#[path = "../../../crates/modules/abilities/host/webhooks.rs"]
pub(crate) mod core_webhooks;
#[path = "../../../crates/modules/abilities/host/visualizer.rs"]
pub(crate) mod core_visualizer;
#[path = "../../../crates/modules/abilities/host/vault.rs"]
pub(crate) mod core_vault;
#[path = "../../../crates/modules/abilities/host/visual_capture.rs"]
pub(crate) mod core_visual_capture;
pub(crate) mod failure;
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
    crate::source_control::is_read(name) || name=="process_image" || core_vault::is_read(name) || core_visualizer::is_read(name) || core_webhooks::is_read(name) || core_orchestration::is_read(name) || core_ui_admin::is_read(name) || core_agent_management::is_read(name) || name=="list_models" || core_user_files::is_read(name) || name=="main_notes_read" || crate::modules::is_read(name) || matches!(name,"mcp_list_connections"|"mcp_search_servers"|"mcp_inspect_server"|"mcp_list_tools") || matches!(
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
            | "load_skill" | "context_assets"
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
    let mut schemas=native_schemas();schemas.extend(crate::ability_plugins::current_schemas());schemas
}
pub(crate) fn native_schemas() -> Vec<Value> {
 let mut schemas=native_schemas_without_administrator();
 schemas.extend(administrator::schemas());
 schemas
}
pub(crate) fn native_schemas_without_administrator() -> Vec<Value> {
    static SCHEMAS: OnceLock<Vec<Value>> = OnceLock::new();
    let mut schemas=SCHEMAS.get_or_init(|| { let mut schemas: Vec<Value> = [
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
        ("shell", "Execute a shell command in this session workspace; source is read-only and source changes require broker task tools; policy-gated", json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["command"]),
        ("python", "Execute Python in this session workspace; source is read-only and source changes require broker task tools; policy-gated", json!({"code":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["code"]),
    ].into_iter().map(|(name, description, properties, required)| json!({
        "type":"function", "name":name, "description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect();
        for schema in &mut schemas {
            if matches!(schema["name"].as_str(), Some("file_list"|"file_read"|"file_search"|"file_edit"|"file_write")) {
                let properties = schema["parameters"]["properties"].as_object_mut().unwrap();
                properties.insert("task_id".into(), json!({"type":"string","pattern":"^[A-Za-z0-9_-]{8,80}$","description":"Deprecated compatibility alias. Prefer broker_id; do not supply both. Never use a chat session ID here."}));
                properties.insert("broker_id".into(),json!({"type":"string","pattern":"^(new|[A-Za-z0-9_-]{8,80})$","description":"Omit to continue current broker work; new creates independent isolated work in this operation; reuse a returned Broker ID to switch. Session identity is automatic. Never invent an ID."}));
                if matches!(schema["name"].as_str(), Some("file_edit"|"file_write")) {
                    schema["parameters"]["properties"]["expected_inputs_sha256"] = json!({"type":"string","pattern":"^[a-f0-9]{64}$","description":"Task digest from the latest task read/list/review; required when writing an existing task."});
                    schema["parameters"]["properties"]["request_id"] = json!({"type":"string","pattern":"^[A-Za-z0-9_-]{8,80}$","description":"Stable task-write request ID. Reuse only for identical retries; inspect build_work receipt after uncertainty."});
                }
                if schema["name"]=="file_list" {
                    schema["parameters"]["properties"]["offset"] = json!({"type":"integer","minimum":0});
                    schema["parameters"]["properties"]["limit"] = json!({"type":"integer","minimum":1,"maximum":100});
                }
                let description = schema["description"].as_str().unwrap().to_owned();
                schema["description"] = json!(format!("{description}. Source writes use isolated broker work automatically. Reads without a selector inspect published/container files; pass a returned broker_id for private-source reads. Optional broker_id selects current, new or returned work. Explicit runtime/workspace paths retain their owning tools. Never deploys."));
            }
        }
        schemas.extend(crate::background_tasks::schemas());
        schemas.extend(web_access::schemas());
        schemas.extend(core_decision::schemas());
        schemas.extend(core_directory_decision::schemas());
        schemas.extend(core_utilities::schemas());
        schemas.extend(core_user_files::schemas());
        schemas.extend(core_model_switcher::schemas());
        schemas.extend(core_agent_management::schemas());
        schemas.extend(core_image_vision::schemas());
        schemas.extend(core_image_generation::schemas());
        schemas.extend(core_app_control::schemas());
        schemas.extend(core_ui_admin::schemas());
        schemas.extend(core_functions::schemas());
        schemas.extend(core_orchestration::schemas());
        schemas.extend(core_base_extras::schemas());
        schemas.extend(core_webhooks::schemas());
        schemas.extend(core_visualizer::schemas());
        schemas.extend(core_vault::schemas());
        schemas.extend(core_visual_capture::schemas());
        schemas.extend(core_session::schemas());
        schemas.extend(crate::session_messaging::schemas());
        schemas.extend(crate::main_chat::schemas());
        schemas.extend(core_skills::schemas());
        schemas.extend(core_context_assets::schemas());
        schemas.extend(core_prompt::schemas());
        schemas.extend(core_search::schemas());
        schemas.extend(core_tool_search::schemas());
        schemas.extend(core_self_edit::schemas());
        schemas.extend(core_task::schemas());
        schemas.extend(core_compaction::schemas());
        schemas.extend(core_recall::schemas());
        schemas.extend(core_mode::schemas());
        #[cfg(test)] schemas.push(json!({"type":"function","name":"test_owner_purchase","description":"Development-only owner authorization fixture",
            "authorization":{"kind":"credential_owner","credential_kind":"test"},
            "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}}));
        normalize_native_schemas(schemas)
    }).clone();
    // Optional package schemas are live; a cached core catalog cannot resurrect removed tools.
    schemas.extend(crate::modules::schemas());
    schemas.extend(crate::mcp::schemas());
    schemas
}

pub use webagent_engine::tools::transport::{execute,execute_trusted,execute_host_trusted};

/// Apply existing fixed action/nullable-optional transformations before catalog identity is computed.
pub(crate) fn normalize_native_schemas(mut schemas:Vec<Value>)->Vec<Value>{
        for schema in &mut schemas {
            if crate::actions::supported(schema["name"].as_str().unwrap_or("")) {
                schema["parameters"]["properties"]["_action"]=json!({"type":["object","null"],
                    "properties":{"id":{"type":"string","minLength":1,"maxLength":128},
                    "separate_from":{"type":["string","null"],"maxLength":128}},
                    "required":["id"],"additionalProperties":false,
                    "description":"Stable intended-action ID. Reuse for identical retries. Matching additional actions require a prior action reference, fresh user intent and human authorization; new IDs cannot bypass protection."});
            }
        }
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
}
