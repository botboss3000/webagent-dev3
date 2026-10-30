use anyhow::{bail, Result};
pub(crate) use webagent_engine::tools::outcome;
// Single and parallel turns share the same Rust dispatch for native and worker tools.
pub(crate) mod administrator;
#[path = "../../../plugins/abilities/Business/front_desk/adapter.rs"]
pub(crate) mod business_front_desk;
#[path = "../../../plugins/abilities/Administrator/code_index/configuration_adapter.rs"]
pub(crate) mod code_index_configuration;
#[path = "../../../plugins/abilities/Administrator/code_index/maintenance_adapter.rs"]
pub(crate) mod code_index_maintenance;
pub(crate) mod code_index_retrieval;
#[path = "../../../crates/modules/abilities/host/agent_management.rs"]
pub(crate) mod core_agent_management;
#[path = "../../../crates/modules/abilities/host/app_control.rs"]
pub(crate) mod core_app_control;
#[path = "../../../crates/modules/abilities/host/authority.rs"]
pub(crate) mod core_authority;
#[path = "../../../crates/modules/abilities/host/base_extras.rs"]
pub(crate) mod core_base_extras;
#[path = "../../../crates/modules/abilities/host/base/compaction_adapter.rs"]
pub(crate) mod core_compaction;
#[path = "../../../crates/modules/abilities/host/base/context_assets_adapter.rs"]
pub(crate) mod core_context_assets;
#[path = "../../../plugins/abilities/Core/decision/adapter.rs"]
pub(crate) mod core_decision;
#[path = "../../../plugins/abilities/Core/decision/directory_adapter.rs"]
pub(crate) mod core_directory_decision;
#[path = "../../../crates/modules/abilities/host/image_generation.rs"]
pub(crate) mod core_image_generation;
#[path = "../../../crates/modules/abilities/host/base/mode_adapter.rs"]
pub(crate) mod core_mode;
#[path = "../../../crates/modules/abilities/host/native_host.rs"]
pub(crate) mod native_host;
#[path = "../../../crates/modules/abilities/host/model_switcher.rs"]
pub(crate) mod core_model_switcher;
#[path = "../../../crates/modules/abilities/host/orchestration.rs"]
pub(crate) mod core_orchestration;
#[path = "../../../crates/modules/abilities/host/base/prompt_adapter.rs"]
pub(crate) mod core_prompt;
#[path = "../../../crates/modules/abilities/host/base/recall_adapter.rs"]
pub(crate) mod core_recall;
#[path = "../../../crates/modules/abilities/host/base/search_adapter.rs"]
pub(crate) mod core_search;
#[path = "../../../crates/modules/abilities/host/base/self_edit_adapter.rs"]
pub(crate) mod core_self_edit;
#[path = "../../../crates/modules/abilities/host/base/session_adapter.rs"]
pub(crate) mod core_session;
#[path = "../../../crates/modules/abilities/host/base/skills_adapter.rs"]
pub(crate) mod core_skills;
#[path = "../../../crates/modules/abilities/host/base/task_adapter.rs"]
pub(crate) mod core_task;
#[path = "../../../crates/modules/abilities/host/base/tool_search_adapter.rs"]
pub(crate) mod core_tool_search;
#[path = "../../../crates/modules/abilities/host/vault.rs"]
pub(crate) mod core_vault;
#[path = "../../../crates/modules/abilities/host/visualizer.rs"]
pub(crate) mod core_visualizer;
#[path = "../../../crates/modules/abilities/host/webhooks.rs"]
pub(crate) mod core_webhooks;
mod dispatch;
pub(crate) use webagent_native_tools as native;
pub(crate) mod failure;
#[path = "../../../crates/modules/abilities/host/web_search.rs"]
pub(crate) mod web_search;
#[cfg(test)]
pub(crate) use dispatch::call_raw;
pub(crate) use dispatch::{call, call_foreground};
use serde_json::{json, Value};
use std::sync::OnceLock;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use tokio_util::sync::CancellationToken;

/// Whether a tool only reads. A tool folder's `effect` decides; tools still defined in
/// code use the lists below, and an unknown tool counts as a write.
pub fn is_read(name: &str) -> bool {
    if let Some(definition) = webagent_abilities::tool_definitions::get(name) {
        return definition.is_read();
    }
    crate::source_control::is_read(name)
        || native::webhooks::is_read(name)
        || name == "main_notes_read"
        || crate::modules::is_read(name)
        || matches!(
            name,
            "mcp_list_connections" | "mcp_search_servers" | "mcp_inspect_server" | "mcp_list_tools"
        )
        || matches!(
            name,
            "task_status"
                | "task_wait"
                | "task_cancel"
                | "search_tools"
                | "list_tools"
                | "load_tool"
                | "get_time"
                | "get_date"
                | "calculate"
                | "read_attachment"
                | "search_this_session"
                | "list_skills"
                | "load_skill"
                | "context_assets"
                | "read_own_prompt"
                | "session_search"
                | "similar_sessions"
                | "get_task_checkpoint"
                | "compact_context"
                | "recall_compacted"
                | "file_list"
                | "file_read"
                | "file_search"
                | "source_review"
                | "state_inspect"
                | "prompt_slot_read"
                | "set_execution_mode"
        )
}

/// Compiled handlers whose schemas, effect and role defaults live in tool folders.
pub(crate) fn folder_handlers() -> impl Iterator<Item = &'static str> {
    [
        native::web_access::TOOLS,
        web_search::TOOLS,
        business_front_desk::TOOLS,
        native::knowledge_files::TOOLS,
        core_decision::TOOLS,
        core_directory_decision::TOOLS,
        native::user_files::TOOLS,
        core_model_switcher::TOOLS,
        core_agent_management::TOOLS,
        native::image_vision::TOOLS,
        core_image_generation::TOOLS,
        native::app_control::TOOLS,
        native::ui_admin::TOOLS,
        native::functions::TOOLS,
        core_orchestration::TOOLS,
        native::visualizer::TOOLS,
        native::vault::TOOLS,
        native::visual_capture::TOOLS,
    ]
    .into_iter()
    .flatten()
    .copied()
}

/// Schemas for `names` from their tool folders. A tool whose folder is missing or invalid
/// is left out, so the model never sees it and the catalog marks it unsupported.
pub(crate) fn folder_schemas(names: &[&str]) -> Vec<Value> {
    names
        .iter()
        .filter_map(|name| webagent_abilities::tool_definitions::get(name))
        .map(webagent_abilities::tool_definitions::ToolDefinition::schema)
        .collect()
}

/// Startup check, both ways: every folder handler has a valid definition and every
/// definition has a compiled handler. Each problem disables only its own tool.
pub(crate) fn definition_problems() -> Vec<String> {
    use webagent_abilities::tool_definitions as definitions;
    let handlers: std::collections::BTreeSet<_> = folder_handlers()
        .chain(webagent_administration::tools::TOOLS)
        .chain(
            webagent_abilities::package::AUTOMATION_TOOLS
                .iter()
                .copied(),
        )
        .collect();
    let mut problems = definitions::problems().to_vec();
    problems.extend(
        handlers
            .iter()
            .filter(|name| definitions::get(name).is_none())
            .map(|name| {
                format!("Compiled tool {name} has no tool folder definition; it is disabled")
            }),
    );
    problems.extend(
        definitions::definitions()
            .keys()
            .filter(|name| !handlers.contains(name.as_str()))
            .map(|name| {
                format!("Tool folder {name} has no compiled handler; it is listed but cannot run")
            }),
    );
    problems
}

/// Default access for a tool built into Base or defined in a tool folder; `None` keeps
/// the earlier Auto for tools still defined only in code.
fn tool_defaults(name: &str, rungs: &[String]) -> Option<webagent_access::Permission> {
    if webagent_abilities::base::owns(name) {
        let read = is_read(name) || webagent_abilities::is_discovery(name);
        return Some(webagent_abilities::base::default_access(rungs, read));
    }
    // Role defaults edited in the live checkout apply at once.
    if let Some(definition) = crate::ability_drift::current().definition(name) {
        return Some(definition.default_for(rungs));
    }
    webagent_abilities::tool_definitions::get(name).map(|definition| definition.default_for(rungs))
}

/// Let capability levels fall back to tool and ability defaults, and account plans grant by
/// entitlement group; once per process.
pub(crate) fn install_tool_defaults() {
    webagent_access::capabilities::install_tool_defaults(tool_defaults);
    webagent_access::capabilities::install_ability_defaults(
        webagent_abilities::ability_access::default_for,
    );
    webagent_access::install_entitlements(
        webagent_abilities::ability_access::ability_entitlement,
        webagent_abilities::ability_access::tool_entitlement,
    );
}

pub fn schemas() -> Vec<Value> {
    let mut schemas = native_schemas();
    schemas.extend(crate::ability_plugins::current_schemas());
    schemas
}
pub(crate) fn native_schemas() -> Vec<Value> {
    let mut schemas = native_schemas_without_administrator();
    schemas.extend(administrator::schemas());
    schemas
}
pub(crate) fn native_schemas_without_administrator() -> Vec<Value> {
    static SCHEMAS: OnceLock<Vec<Value>> = OnceLock::new();
    let mut schemas=SCHEMAS.get_or_init(|| {
        for problem in definition_problems() {eprintln!("{problem}");}
        let mut schemas: Vec<Value> = [
        ("file_list", "List a directory by absolute container path or relative live-checkout path", json!({"path":{"type":"string"}}), vec![]),
        ("file_read", "Read a bounded page of file lines from a container or live-checkout path", json!({"path":{"type":"string"},"offset":{"type":"integer","minimum":1,"maximum":10000000},"limit":{"type":"integer","minimum":1,"maximum":200}}), vec!["path"]),
        ("file_search", "Search container or live-checkout files by literal or regex query; outline comments in one file", json!({"path":{"type":"string"},"query":{"type":"string"},"regex":{"type":"boolean"},"file_glob":{"type":"string"},"comments_only":{"type":"boolean"}}), vec![]),
        ("file_write", "Write a container or live-checkout file; policy-gated", json!({"path":{"type":"string"},"content":{"type":"string"}}), vec!["path","content"]),
        ("file_edit", "Replace exactly one matching span in an existing container or live-checkout file; policy-gated", json!({"path":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"expected_sha256":{"type":"string"}}), vec!["path","old_text","new_text"]),
        ("checkout_edit", "Write (content) or edit (old_text/new_text) one live-checkout source file directly, without a broker task; not built or deployed; policy-gated", json!({"path":{"type":"string"},"content":{"type":"string"},"old_text":{"type":"string"},"new_text":{"type":"string"},"expected_sha256":{"type":"string","pattern":"^[a-f0-9]{64}$"}}), vec!["path"]),
        ("workspace_seed", "Copy the audited WebAgent v3 source snapshot into this session workspace and create a Git baseline; policy-gated", json!({}), vec![]),
        ("source_review", "Review tracked and new changes in the seeded v3 source without modifying its Git index", json!({}), vec![]),
        ("prompt_slot_read", "List or read the current agent's ordered instruction slots", json!({"action":{"type":"string","enum":["list","get"]},"slot_name":{"type":"string"}}), vec!["action"]),
        ("prompt_slot_write", "Create, update or delete a current-agent instruction slot; policy-gated", json!({"action":{"type":"string","enum":["put","delete"]},"slot_name":{"type":"string"},"content":{"type":"string"},"expected_revision":{"type":"integer","minimum":0},"order_index":{"type":"integer"},"lock":{"type":"boolean"},"merge_mode":{"type":"string","enum":["replace","append"]}}), vec!["action","slot_name","expected_revision"]),
        ("state_inspect", "Read bounded public metadata for the current session; catalog, session, chat, or runs", json!({"resource":{"type":"string","enum":["catalog","session","chat","runs"]},"after":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":50}}), vec!["resource"]),
        ("source_patch", "Apply a checked text patch to the seeded v3 source; policy-gated", json!({"patch":{"type":"string"},"expected_review_sha256":{"type":"string"}}), vec!["patch","expected_review_sha256"]),
        ("promotion_propose", "Submit the current reviewed v3 source patch for administrator approval; does not change live source", json!({"expected_review_sha256":{"type":"string"},"summary":{"type":"string"}}), vec!["expected_review_sha256","summary"]),
        ("shell", "Execute a shell command in this session workspace; reads any container file, writes only the workspace; source changes use broker task tools or checkout_edit; policy-gated", json!({"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["command"]),
        ("python", "Execute Python in this session workspace; reads any container file, writes only the workspace; source changes use broker task tools or checkout_edit; policy-gated", json!({"code":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":600}}), vec!["code"]),
    ].into_iter().map(|(name, description, properties, required)| json!({
        "type":"function", "name":name, "description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect();
        for schema in &mut schemas {
            if matches!(schema["name"].as_str(), Some("file_list"|"file_read"|"file_search"|"file_edit"|"file_write")) {
                let properties = schema["parameters"]["properties"]
                    .as_object_mut()
                    .expect("native schemas declare properties");
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
                let description = schema["description"]
                    .as_str()
                    .expect("native schemas declare a description")
                    .to_owned();
                schema["description"] = json!(format!("{description}. Source writes use isolated broker work automatically. Reads without a selector inspect published/container files; pass a returned broker_id for private-source reads. Optional broker_id selects current, new or returned work. Explicit runtime/workspace paths retain their owning tools. Never deploys."));
            }
        }
        schemas.extend(crate::background_tasks::schemas());
        schemas.extend(native::schemas(native::web_access::TOOLS));
        schemas.extend(web_search::schemas());
        schemas.extend(business_front_desk::schemas());
        schemas.extend(native::schemas(native::knowledge_files::TOOLS));
        schemas.extend(core_decision::schemas());
        schemas.extend(core_directory_decision::schemas());
        schemas.extend(native::utilities::schemas());
        schemas.extend(native::schemas(native::user_files::TOOLS));
        schemas.extend(core_model_switcher::schemas());
        schemas.extend(core_agent_management::schemas());
        schemas.extend(native::schemas(native::image_vision::TOOLS));
        schemas.extend(core_image_generation::schemas());
        schemas.extend(native::schemas(native::app_control::TOOLS));
        schemas.extend(native::schemas(native::ui_admin::TOOLS));
        schemas.extend(native::functions::schemas());
        schemas.extend(core_orchestration::schemas());
        schemas.extend(core_base_extras::schemas());
        schemas.extend(native::webhooks::schemas());
        schemas.extend(native::schemas(native::visualizer::TOOLS));
        schemas.extend(native::schemas(native::vault::TOOLS));
        schemas.extend(native::schemas(native::visual_capture::TOOLS));
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

pub use webagent_engine::tools::transport::{execute, execute_host_trusted, execute_trusted};

/// Apply existing fixed action/nullable-optional transformations before catalog identity is computed.
pub(crate) fn normalize_native_schemas(mut schemas: Vec<Value>) -> Vec<Value> {
    for schema in &mut schemas {
        if crate::actions::supported(schema["name"].as_str().unwrap_or("")) {
            schema["parameters"]["properties"]["_action"] = json!({"type":["object","null"],
                    "properties":{"id":{"type":"string","minLength":1,"maxLength":128},
                    "separate_from":{"type":["string","null"],"maxLength":128}},
                    "required":["id"],"additionalProperties":false,
                    "description":"Stable intended-action ID. Reuse for identical retries. Matching additional actions require a prior action reference, fresh user intent and human authorization; new IDs cannot bypass protection."});
        }
    }
    // Responses may require every property: nullable optional fields keep
    // live file calls and action-specific build arguments representable.
    for schema in &mut schemas {
        if matches!(
            schema["name"].as_str(),
            Some(
                "file_list"
                    | "file_read"
                    | "file_search"
                    | "file_edit"
                    | "file_write"
                    | "build_work"
            )
        ) {
            let required = schema["parameters"]["required"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let properties = schema["parameters"]["properties"]
                .as_object_mut()
                .expect("native schemas declare properties");
            for (key, property) in properties {
                if required
                    .iter()
                    .any(|value| value.as_str() == Some(key.as_str()))
                {
                    continue;
                }
                if let Some(kind) = property["type"].as_str().map(str::to_owned) {
                    property["type"] = json!([kind, "null"]);
                }
                if let Some(values) = property.get_mut("enum").and_then(Value::as_array_mut) {
                    if !values.contains(&Value::Null) {
                        values.push(Value::Null);
                    }
                }
            }
        }
    }
    schemas
}

#[cfg(test)]
mod definition_tests {
    #[test]
    fn every_folder_handler_and_definition_match() {
        assert_eq!(super::definition_problems(), Vec::<String>::new());
    }
}
