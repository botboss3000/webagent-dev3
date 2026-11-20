pub(crate) use webagent_engine::tools::outcome;
// Single and parallel turns share the same Rust dispatch for native and worker tools.
#[cfg(feature = "administrator")]
pub(crate) mod administrator;
#[cfg(not(feature = "administrator"))]
#[path = "administrator_absent.rs"]
pub(crate) mod administrator;
#[path = "../../../crates/modules/abilities/host/agent_management.rs"]
pub(crate) mod core_agent_management;
#[path = "../../../crates/modules/abilities/host/app_control.rs"]
pub(crate) mod core_app_control;
#[path = "../../../crates/modules/abilities/host/authority.rs"]
pub(crate) mod core_authority;
#[path = "../../../crates/modules/inference/host/tools.rs"]
pub(crate) mod core_config_controller;
#[path = "../../../crates/modules/abilities/host/base/elements_adapter.rs"]
pub(crate) mod core_elements;
#[path = "../../../crates/modules/media/host/image_generation.rs"]
pub(crate) mod core_image_generation;
#[path = "../../../crates/modules/abilities/host/base/mode_adapter.rs"]
pub(crate) mod core_mode;
#[path = "../../../crates/modules/abilities/host/base/prompt_adapter.rs"]
pub(crate) mod core_prompt;
#[path = "../../../crates/modules/abilities/host/base/recall_adapter.rs"]
pub(crate) mod core_recall;
#[path = "../../../crates/modules/abilities/host/base/scripts_adapter.rs"]
pub(crate) mod core_scripts;
#[path = "../../../crates/modules/abilities/host/base/session_adapter.rs"]
pub(crate) mod core_session;
#[path = "../../../crates/modules/abilities/host/base/skills_adapter.rs"]
pub(crate) mod core_skills;
#[path = "../../../crates/modules/abilities/host/vault.rs"]
pub(crate) mod core_vault;
#[path = "../../../crates/modules/abilities/host/webhooks.rs"]
pub(crate) mod core_webhooks;
mod dispatch;
#[path = "../../../crates/modules/abilities/host/native_host.rs"]
pub(crate) mod native_host;
pub(crate) use webagent_native_tools as native;
pub(crate) mod failure;
#[path = "../../../crates/modules/abilities/host/web_search.rs"]
pub(crate) mod web_search;
#[cfg(test)]
pub(crate) use dispatch::call_raw;
pub(crate) use dispatch::{call, call_foreground};
use serde_json::{json, Value};
use std::sync::OnceLock;
/// A tool's repeat policy: reads are never repeat-guarded; a write uses its `<tool>.tool.json`
/// retry fields. Without them, workspace-confined coding tools (shell, python) are low
/// risk, since their effects can be inspected and reverted, and every other write is high.
pub fn retry(name: &str) -> webagent_engine::tools::retry_guard::Retry {
    use webagent_abilities::tool_definitions::RetryRisk;
    use webagent_engine::tools::retry_guard::{Retry, Risk, EXEMPT};
    if EXEMPT.contains(&name) {
        return Retry {
            risk: Risk::Exempt,
            ..Retry::default()
        };
    }
    if is_read(name) {
        return Retry {
            risk: Risk::None,
            ..Retry::default()
        };
    }
    let fallback = || Retry {
        risk: if webagent_engine::tools::execution::workspace_confined(name) {
            Risk::Low
        } else {
            Risk::High
        },
        ..Retry::default()
    };
    webagent_abilities::tool_definitions::get(name).map_or_else(fallback, |definition| Retry {
        risk: match definition.repeat_guard.risk {
            Some(RetryRisk::None) => Risk::None,
            Some(RetryRisk::Low) => Risk::Low,
            Some(RetryRisk::High) | None => Risk::High,
        },
        hint: definition.repeat_guard.hint.clone(),
        warning: definition.repeat_guard.warning.clone(),
    })
}

/// Whether a tool only reads. A tool folder's `effect` decides; tools still defined in
/// code use the lists below, and an unknown tool counts as a write.
pub fn is_read(name: &str) -> bool {
    if let Some(definition) = webagent_abilities::tool_definitions::get(name) {
        return definition.is_read();
    }
    crate::source_control::is_read(name)
        || crate::modules::is_read(name)
        || matches!(
            name,
            "file_list"
                | "file_read"
                | "file_search"
                | "source_review"
                | "state_inspect"
                | "prompt_slot_read"
        )
}

/// Codebase Admin tools handled here, defined in `abilities/Administrator/codebase_admin/tools/`.
/// Without the Administrator pack their folders are absent and the tools unavailable.
pub(crate) const CODEBASE_TOOLS: [&str; 11] = [
    "file_list",
    "file_read",
    "file_search",
    "file_edit",
    "file_write",
    "checkout_edit",
    "prompt_slot_read",
    "prompt_slot_write",
    "state_inspect",
    "shell",
    "python",
];

/// Session-workspace source tools handled here, defined in
/// `abilities/Administrator/native_tools/tools/` since no ability attaches them. Without the
/// Administrator pack their folders are absent and the tools unavailable.
pub(crate) const WORKSPACE_TOOLS: [&str; 3] = ["workspace_seed", "source_review", "source_patch"];

/// Compiled handlers whose schemas, effect and role defaults live in tool folders.
pub(crate) fn folder_handlers() -> impl Iterator<Item = &'static str> {
    [
        native::web_access::TOOLS,
        web_search::TOOLS,
        native::human_escalation::TOOLS,
        native::reply_buttons::TOOLS,
        native::sandbox::TOOLS,
        native::knowledge_files::TOOLS,
        native::task_list::TOOLS,
        native::wiki::TOOLS,
        native::decision::TOOLS,
        native::directory_decision::TOOLS,
        native::user_files::TOOLS,
        native::model_switcher::TOOLS,
        core_agent_management::TOOLS,
        native::image_vision::TOOLS,
        core_image_generation::TOOLS,
        core_config_controller::TOOLS,
        webagent_abilities::base::FOLDER_TOOLS,
        native::app_control::TOOLS,
        native::orchestration::TOOLS,
        webagent_browser::TOOLS,
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
    let pack = administrator::pack_tools();
    let mut handlers: std::collections::BTreeSet<&str> = folder_handlers()
        .chain(
            webagent_abilities::package::AUTOMATION_TOOLS
                .iter()
                .copied(),
        )
        .chain(crate::mcp::TOOLS.iter().copied())
        .collect();
    handlers.extend(pack.iter().map(String::as_str));
    if !pack.is_empty() {
        handlers.extend(CODEBASE_TOOLS);
        handlers.extend(WORKSPACE_TOOLS);
    }
    let mut problems = definitions::problems().to_vec();
    problems.extend(
        handlers
            .iter()
            .filter(|name| definitions::get(name).is_none())
            .map(|name| {
                format!("Compiled tool {name} has no tool folder definition; it is disabled")
            }),
    );
    let pending = pending_tools();
    problems.extend(
        definitions::definitions()
            .keys()
            .filter(|name| !handlers.contains(name.as_str()) && !pending.contains(name.as_str()))
            .map(|name| {
                format!("Tool folder {name} has no compiled handler; it is listed but cannot run")
            }),
    );
    problems
}

/// Tools listed by abilities whose manifest says `"readiness": "pending"`: catalog-only
/// definitions that are expected to have no compiled handler yet.
fn pending_tools() -> std::collections::BTreeSet<String> {
    let Some(root) = webagent_abilities::tool_definitions::root() else {
        return Default::default();
    };
    let groups = std::fs::read_dir(root).into_iter().flatten().flatten();
    groups
        .flat_map(|group| {
            std::fs::read_dir(group.path())
                .into_iter()
                .flatten()
                .flatten()
        })
        .filter_map(|ability| {
            let id = ability.file_name().to_string_lossy().into_owned();
            let bytes = std::fs::read(ability.path().join(format!("{id}.json"))).ok()?;
            let manifest: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            (manifest["readiness"] == "pending").then(|| manifest["tools"].clone())
        })
        .flat_map(|tools| {
            tools
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool.as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Default access for a tool defined in a tool folder; `None` keeps the earlier Auto for
/// tools still defined only in code.
pub(crate) fn tool_defaults(name: &str, rungs: &[String]) -> Option<webagent_access::Permission> {
    // Role defaults edited in the live checkout apply at once.
    if let Some(definition) = crate::ability_drift::current().definition(name) {
        return Some(definition.default_for(rungs));
    }
    webagent_abilities::tool_definitions::get(name).map(|definition| definition.default_for(rungs))
}

/// Let capability levels fall back to tool defaults, owners' tiers gate abilities and
/// tools by their `owner_tier`, and built-in roles by their `role_access`; once per process.
pub(crate) fn install_tool_defaults() {
    webagent_access::capabilities::install_tool_defaults(tool_defaults);
    webagent_access::install_tier_access(
        webagent_abilities::ability_access::ability_allows,
        tool_tiers,
        webagent_abilities::ability_access::administrator_ability,
    );
    webagent_access::install_role_access(
        webagent_abilities::ability_access::ability_role_allows,
        tool_roles,
    );
}

/// A tool's own `owner_tier` for an owner tier; live checkout edits apply at once.
fn tool_tiers(name: &str, tier: &str) -> bool {
    let current = crate::ability_drift::current();
    current
        .definition(name)
        .or_else(|| webagent_abilities::tool_definitions::get(name))
        .and_then(|definition| definition.owner_tier.as_ref())
        .is_none_or(|tiers| tiers.iter().any(|allowed| allowed == tier))
}

/// A tool's own `role_access` for a level's rungs; `None` when the tool sets none, so its
/// abilities' lists decide. Live checkout edits apply at once.
fn tool_roles(name: &str, rungs: &[String]) -> Option<bool> {
    let current = crate::ability_drift::current();
    let role = webagent_abilities::ability_access::built_in_role(rungs);
    current
        .definition(name)
        .or_else(|| webagent_abilities::tool_definitions::get(name))
        .and_then(|definition| definition.role_access.as_ref())
        .map(|roles| roles.iter().any(|allowed| allowed == role))
}

pub fn schemas() -> Vec<Value> {
    let mut schemas = native_schemas();
    schemas.extend(crate::ability_plugins::current_schemas());
    schemas
}
/// Declare the reserved `_repeat_of` on repeat-guarded tools: tool bridges that enforce a
/// schema drop undeclared fields, which would leave a held repeat impossible to confirm.
/// Strict schemas are left alone, since strict mode requires every declared field.
fn advertise_repeat(schemas: &mut [Value]) {
    use webagent_engine::tools::retry_guard::{Risk, REPEAT_ARG};
    for schema in schemas {
        let Some(name) = schema["name"].as_str().map(str::to_owned) else {
            continue;
        };
        if schema["strict"] == true || matches!(retry(&name).risk, Risk::None | Risk::Exempt) {
            continue;
        }
        if let Some(properties) = schema["parameters"]["properties"].as_object_mut() {
            properties.entry(REPEAT_ARG).or_insert_with(|| {
                json!({"type":"string","description":"Only to repeat an earlier identical call on purpose: that call's ID, as the repeat guard's result names it."})
            });
        }
    }
}
/// Argument types of the tools decoded outside the native-tools crate: in root host
/// adapters, the engine and the Administration host.
#[cfg(test)]
pub(crate) fn host_argument_types(
) -> impl Iterator<Item = &'static webagent_abilities::tool_arguments::ArgumentType> {
    [
        administrator::ARGUMENT_TYPES,
        core_config_controller::ARGUMENT_TYPES,
        core_image_generation::ARGUMENT_TYPES,
        core_scripts::ARGUMENT_TYPES,
        core_skills::ARGUMENT_TYPES,
        crate::api::sessions::ARGUMENT_TYPES,
        crate::session_messaging::ARGUMENT_TYPES,
        crate::source_control::ARGUMENT_TYPES,
        webagent_engine::runtime::background::ARGUMENT_TYPES,
    ]
    .into_iter()
    .flatten()
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
        let mut schemas = native::schemas(&WORKSPACE_TOOLS);
        schemas.extend(native::schemas(&CODEBASE_TOOLS));
        schemas.extend(native::schemas(native::web_access::TOOLS));
        schemas.extend(web_search::schemas());
        schemas.extend(native::schemas(native::human_escalation::TOOLS));
        schemas.extend(native::schemas(native::reply_buttons::TOOLS));
        schemas.extend(native::schemas(native::sandbox::TOOLS));
        schemas.extend(native::schemas(native::knowledge_files::TOOLS));
        schemas.extend(native::schemas(native::task_list::TOOLS));
        schemas.extend(native::schemas(native::wiki::TOOLS));
        schemas.extend(native::schemas(native::decision::TOOLS));
        schemas.extend(native::schemas(native::directory_decision::TOOLS));
        schemas.extend(native::schemas(native::user_files::TOOLS));
        schemas.extend(native::schemas(native::model_switcher::TOOLS));
        schemas.extend(core_agent_management::schemas());
        schemas.extend(native::schemas(native::image_vision::TOOLS));
        schemas.extend(core_image_generation::schemas());
        schemas.extend(core_config_controller::schemas());
        schemas.extend(native::schemas(native::app_control::TOOLS));
        schemas.extend(native::schemas(native::orchestration::TOOLS));
        schemas.extend(native::schemas(webagent_browser::TOOLS));
        schemas.extend(native::schemas(webagent_abilities::base::TOOLS));
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
    advertise_repeat(&mut schemas);
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
    /// Native tools still reading their arguments without a registered type. Shrinks as
    /// families convert to `webagent_abilities::tool_arguments`; never grows. "Worker
    /// passthrough" tools go to the tool worker as raw JSON, which decodes and checks them;
    /// "broker passthrough" tools go to the build broker the same way.
    const UNREGISTERED: &[&str] = &[
        "agent_review",
        "browser_extension_action",
        "browser_extension_status",
        "browser_input",
        "browser_read",
        "browser_record",
        "build_work",
        "checkout_edit",   // Worker passthrough.
        "commit_and_push", // Worker passthrough.
        "create_agent",
        "edit_agent_prompt",
        "file_edit",
        "file_list",
        "file_read",
        "file_search",
        "file_write",
        "get_agent",
        "get_tunnel_url",
        "git_tool", // Worker passthrough.
        "knowledge_file_list",
        "list_agent_templates",
        "list_agent_tools",
        "list_models",
        "list_my_agents",
        "manage_agent_chat_config",
        "manage_agent_skills",
        "manage_session",
        "performance_work",
        "python",
        "read_diagnostics",
        "resolve_conflict", // Worker passthrough.
        "sandbox_elements",
        "script_list",
        "set_agent_ability",
        "set_agent_tool",
        "shell",
        "source_patch",  // Broker passthrough.
        "source_review", // Broker passthrough.
        "start_tunnel",
        "terminal_close",
        "terminal_list",
        "terminal_open",
        "terminal_read",
        "terminal_send",
        "terminal_wait",
        "test_owner_purchase",
        "update_agent",
        "watch_sessions",
        "web_read",
        "web_search",
        "workspace_seed", // Broker passthrough.
    ];

    #[test]
    fn native_tools_register_their_argument_types() {
        assert_eq!(
            webagent_abilities::tool_arguments::registry_problems(super::host_argument_types()),
            Vec::<String>::new()
        );
        let registered: std::collections::BTreeSet<&str> = super::native::argument_types()
            .chain(super::host_argument_types())
            .map(|entry| entry.tool)
            .collect();
        let mut missing = Vec::new();
        for schema in super::native_schemas() {
            let name = schema["name"].as_str().unwrap_or_default().to_owned();
            if !registered.contains(name.as_str()) && !UNREGISTERED.contains(&name.as_str()) {
                missing.push(name);
            }
        }
        missing.sort();
        let stale: Vec<&&str> = UNREGISTERED
            .iter()
            .filter(|name| registered.contains(**name))
            .collect();
        assert!(
            missing.is_empty(),
            "register these tools' argument types: {missing:?}"
        );
        assert!(
            stale.is_empty(),
            "remove registered tools from UNREGISTERED: {stale:?}"
        );
    }

    #[test]
    fn every_folder_handler_and_definition_match() {
        assert_eq!(super::definition_problems(), Vec::<String>::new());
    }

    #[test]
    fn guarded_tools_declare_repeat_of_and_reads_do_not() {
        let mut schemas = vec![
            serde_json::json!({"name":"file_write","parameters":{"properties":{"path":{}}}}),
            serde_json::json!({"name":"file_read","parameters":{"properties":{"path":{}}}}),
            serde_json::json!({"name":"file_write","strict":true,"parameters":{"properties":{"path":{}}}}),
        ];
        super::advertise_repeat(&mut schemas);
        assert_eq!(
            schemas[0]["parameters"]["properties"]["_repeat_of"]["type"],
            "string"
        );
        assert!(schemas[1]["parameters"]["properties"]
            .get("_repeat_of")
            .is_none());
        assert!(schemas[2]["parameters"]["properties"]
            .get("_repeat_of")
            .is_none());
    }
}
