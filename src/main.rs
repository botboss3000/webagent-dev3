//! The v3 executable has three roles: API (accepts work), loop (executes it),
//! and health (checks that the loop is alive). Start with docs/code-tour.md
//! for a plain-language path from a message to its single published reply.
#[cfg(all(feature = "linked-context", feature = "live-context"))]
compile_error!("Choose exactly one engine mode: linked-context or live-context");
#[cfg(not(any(feature = "linked-context", feature = "live-context")))]
compile_error!("Choose an engine mode: linked-context or live-context");
use webagent_access as access;
use webagent_engine::output as engine_output;
use webagent_usage as usage;
mod abilities;
mod ability_drift;
mod ability_plugins;
#[path = "../crates/modules/access/host/policy.rs"]
mod access_policy;
#[path = "../crates/modules/access/host/repository.rs"]
mod access_store;
#[path = "../crates/modules/actions/host/actions.rs"]
mod actions;
#[path = "../crates/modules/agents/host/storage.rs"]
mod agent_storage;
#[path = "../crates/modules/agents/host/api.rs"]
mod agents_api;
mod background_followup;
mod background_tasks;
#[path = "../crates/modules/access/host/capabilities.rs"]
mod capabilities;
#[path = "../crates/modules/chat/host/agent_config.rs"]
mod chat_config;
#[path = "engine/context/accounting.rs"]
mod context_accounting;
#[path = "../crates/modules/access/host/credential_authorization.rs"]
mod credential_authorization;
mod diagnostics;
#[path = "engine/components.rs"]
mod engine_components;
#[path = "../crates/modules/engine/host/mod.rs"]
mod engine_runtime;
#[path = "engine/sandbox.rs"]
mod engine_sandbox;
#[path = "../crates/modules/extensions/host/plugins.rs"]
mod extension_plugins;
#[path = "../crates/modules/chat/host/followup_queue.rs"]
mod followup_queue;
#[path = "gates/controls.rs"]
mod gate_controls;
#[path = "gates/mod.rs"]
mod gate_registry;
mod live_assets;
mod live_files;
#[path = "runtime_config.rs"]
mod loop_runtime;
#[path = "../crates/modules/extensions/host/mcp/mod.rs"]
mod mcp;
mod model_discovery;
#[cfg(feature = "live-context")]
mod module_services;
mod optimization;
mod optimization_prompt;
mod optimization_settings;
#[path = "engine/tools/batch.rs"]
mod parallel_batch;
#[path = "../crates/modules/access/host/api.rs"]
mod phase3_api;
#[path = "../crates/modules/chat/host/session_messaging.rs"]
mod session_messaging;
#[path = "../crates/modules/chat/host/shared_chat.rs"]
mod shared_chat;
#[path = "../crates/modules/chat/host/shared_projection.rs"]
mod shared_projection;
mod source_control;
mod tool_execution;
mod usage_reports;
#[path = "api/user.rs"]
mod user_api;
use webagent_administration_host::services;
#[cfg(test)]
#[path = "api/accounts/acceptance_tests.rs"]
mod account_acceptance_tests;
#[path = "api/accounts/authentication.rs"]
mod account_authentication;
#[path = "api/accounts/management.rs"]
mod account_management;
mod accounts;
mod administrator_services;
#[path = "api/browser_login.rs"]
mod browser_login;
#[path = "../crates/modules/chat/host/main_chat.rs"]
mod main_chat;
#[path = "../crates/modules/extensions/host/modules.rs"]
mod modules;
#[path = "api/accounts/registration.rs"]
mod registration;
#[path = "api/accounts/request_limits.rs"]
mod request_limits;
#[path = "../crates/modules/chat/host/session_naming/mod.rs"]
mod session_naming;
mod wake;
// Accounts storage methods on `Store` (through `Deref` to `StoreCore`).
use webagent_accounts_host::AccountsStore;
#[path = "engine/context/compaction.rs"]
mod context_compaction;
use webagent_files::attachments;
#[path = "../crates/modules/models/host/account_catalog.rs"]
mod account_catalog;
#[path = "../crates/modules/browser/host/browser.rs"]
mod browser;
#[path = "engine/context/assembly.rs"]
mod context_assembly;
#[path = "engine/context/assets.rs"]
mod context_assets;
#[path = "engine/context/input.rs"]
mod context_input;
#[path = "gates/protocol.rs"]
mod gate_protocol;
#[path = "../crates/modules/models/host/harness_bridge.rs"]
mod harness_bridge;
#[path = "engine/context/memory.rs"]
mod memory;
#[path = "../crates/modules/models/host/selection.rs"]
mod model_selection;
#[path = "../crates/modules/models/host/api/selection.rs"]
mod model_selection_api;
#[path = "../crates/modules/models/host/platform_catalog.rs"]
mod platform_catalog;
#[path = "../crates/modules/models/host/provider/mod.rs"]
mod provider;
#[path = "engine/output/publication.rs"]
mod publication;
#[path = "engine/context/retained_recall.rs"]
mod retained_recall;
#[cfg(test)]
mod runtime_tests;
#[path = "storage/mod.rs"]
mod store;
#[path = "api/progress.rs"]
mod task_progress;
#[path = "engine/context/tool_output.rs"]
mod tool_output;
#[path = "engine/tools/mod.rs"]
mod tools;
#[path = "../crates/modules/webapp/host/ui_pages.rs"]
mod ui_pages;
mod workflow;
use anyhow::{anyhow, Result};
use axum::{
    extract::{DefaultBodyLimit, Path, Query, Request},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive},
        IntoResponse, Response, Sse,
    },
    routing::{get, post, put},
    Json, Router,
};
use fs2::FileExt;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    convert::Infallible,
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use store::Store;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio_util::sync::CancellationToken;
use tower_http::services::ServeDir;
use uuid::Uuid;

// Shared state stays at crate root so API and loop modules use one authority.
include!("app.rs");
include!("ability_state.rs");
mod api;
use api::*;
#[path = "engine/loop/mod.rs"]
mod agent_loop;
#[path = "../crates/modules/manager/host/mod.rs"]
mod manager;
use agent_loop::*;
/// The local tool worker's own process modes, which start without the async runtime's
/// threads; `None` for every other role.
#[cfg(feature = "local-worker")]
fn local_worker_mode(mode: Option<&str>) -> Option<i32> {
    use webagent_worker::{browser, git, launch, runner, terminal};
    // Git runs this executable as its credential prompt (GIT_ASKPASS) with this marker.
    if std::env::var_os("V3_GIT_ASKPASS").is_some_and(|marker| marker == "1") {
        return Some(git::askpass::main());
    }
    let spec = || std::env::args().nth(2).unwrap_or_default();
    Some(match mode? {
        // The container launcher sets up and starts every process of the web container.
        "--launch" => launch::run(),
        "--terminal-supervisor" => terminal::supervise(&spec()),
        // One chat session's private browser; it builds its own single-threaded runtime.
        "--browser-session" => browser::session::run(),
        // The per-command and per-plugin supervisors.
        "--tool-runner" => runner::supervise(&spec()),
        "--plugin-runner" => runner::supervise_plugin(&spec()),
        _ => return None,
    })
}

// Choose one role for this process. The API serves requests; the loop owns
// recovery and execution; health only checks a recent loop heartbeat.
fn main() -> Result<()> {
    #[cfg(feature = "local-worker")]
    if let Some(code) = local_worker_mode(std::env::args().nth(1).as_deref()) {
        std::process::exit(code);
    }
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_process())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn missing_tool_policy_is_denied() {
        assert_eq!(policy_decision(&policy_default(), "unknown"), "deny");
    }
    #[test]
    fn policy_defaults_gate_all_side_effects() {
        let p = policy_default();
        for name in [
            "file_write",
            "file_edit",
            "prompt_slot_write",
            "workspace_seed",
            "save_file",
            "generate_image",
            "create_agent",
            "edit_agent_prompt",
            "register_webhook",
            "request_credential",
            "delegate_to_agent",
            "promote_spawn",
            "source_patch",
            "shell",
            "python",
        ] {
            assert_eq!(policy_decision(&p, name), "require_approval");
        }
        assert_eq!(policy_decision(&p, "file_read"), "allow");
        assert_eq!(policy_decision(&p, "prompt_slot_read"), "allow");
    }
    #[tokio::test]
    async fn existing_policy_keeps_new_tools_denied_and_existing_decisions() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let mut policy = policy_default();
        policy["rules"].as_object_mut().unwrap().remove("file_edit");
        policy["rules"]
            .as_object_mut()
            .unwrap()
            .remove("workspace_seed");
        policy["rules"]["calculate"] = json!("deny");
        store.put("policy", &policy).await.unwrap();
        migrate_tool_policy_defaults(&store).await.unwrap();
        migrate_tool_policy_defaults(&store).await.unwrap();
        let migrated = store.get("policy").await.unwrap().unwrap();
        assert_eq!(migrated["rules"]["file_edit"], "deny");
        assert_eq!(migrated["rules"]["workspace_seed"], "deny");
        assert_eq!(migrated["rules"]["calculate"], "deny");
        assert_eq!(migrated["rules"]["get_time"], "allow");
        assert_eq!(migrated["version"], 2);
    }
    // DISPATCH-DIGEST-PARITY: streaming must retain the historical buffered
    // JSON fingerprint, including Unicode, escaping, nested data and empty arrays.
    #[test]
    fn streaming_digest_matches_buffered_json() {
        for value in [
            json!([]),
            json!({"text":"é\n\"quoted\"","nested":[null,true,42]}),
            json!({"output":"x".repeat(65536)}),
        ] {
            assert_eq!(
                digest(&value),
                hex::encode(Sha256::digest(serde_json::to_vec(&value).unwrap()))
            );
        }
    }
    #[test]
    fn arguments_digest_changes_with_action() {
        assert_ne!(
            digest(&json!({"command":"echo hello"})),
            digest(&json!({"command":"rm file"}))
        );
    }
    #[test]
    fn consumed_tool_result_advances_once() {
        let mut r = json!({"input":[],"pending":{"call_id":"c"},"call_index":0});
        append_tool(&mut r, &json!({"output":"done"})).unwrap();
        assert_eq!(r["call_index"], 1);
        assert!(r["pending"].is_null());
        assert_eq!(r["input"][0]["call_id"], "c");
    }
}

#[cfg(test)]
mod context_summary_tests {
    use super::*;
    // CONTEXT-SUMMARY-REGRESSION: neither private fields nor malformed counts escape.
    #[test]
    fn summary_is_numeric_and_allowlisted() {
        let mut run = json!({"context_assembly_report":{"version":1,"scope":"logical_call_envelope","envelope_bytes":100,"max_bytes":200,"history_json_bytes":50,"instruction_json_bytes":30,"schemas_json_bytes":20,"turn":1,"call":2,"private":"SECRET"}});
        assert!(!public_context_summary(&run).to_string().contains("SECRET"));
        assert_eq!(public_context_summary(&run)["envelope_bytes"], 100);
        run["context_assembly_report"]["call"] = json!("SECRET");
        assert!(public_context_summary(&run).is_null());
    }
}
