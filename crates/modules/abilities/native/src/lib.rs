//! Native tool handlers over the shared host core.
//!
//! Each family module exposes its tool names (`TOOLS`), `owns` and `execute`. What belongs
//! to other modules (run authority, session identity, uploads and model lanes) comes
//! through [`NativeHost`], which the root implements; a trait object keeps the handlers
//! compiled here rather than instantiated in the root crate.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;
use webagent_access::{authority::LiveAccess, Relationship, ToolDecision};
use webagent_assets_contracts as assets;
use webagent_chat::shared::SharedChatPolicy;
use webagent_context_contracts::context_compaction::Config as CompactionConfig;
use webagent_files::uploads::AttachmentLimits;
use webagent_host_core::StoreCore;
use webagent_models::account_catalog::PlatformPin;
use webagent_models::model_selection::{Catalog, ModelRole, SelectionPlan};

pub mod app_control;
pub mod base_extras;
pub mod compaction;
pub mod context_assets;
pub mod decision;
pub mod directory_decision;
pub mod front_desk;
pub mod functions;
pub mod image_vision;
pub mod knowledge_files;
pub mod mode;
pub mod model_switcher;
pub mod orchestration;
pub mod prompt;
pub mod recall;
pub mod search;
pub mod self_edit;
pub mod session;
pub mod task;
pub mod tool_search;
pub mod ui_admin;
pub mod user_files;
pub mod utilities;
pub mod vault;
pub mod visual_capture;
pub mod visualizer;
pub mod web_access;
pub mod webhooks;

/// A boxed `Send` future borrowed from the host.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The run position of one native tool call.
#[derive(Clone, Copy, Debug)]
pub struct Call<'a> {
    pub sid: &'a str,
    pub run_id: &'a str,
    pub generation: &'a str,
    /// The pending tool call's ID; retries of one call reuse it.
    pub pending_id: &'a str,
}

/// What native tools need from the rest of the application for one caller.
pub trait NativeHost: Sync {
    /// The caller's storage handle.
    fn store(&self) -> &StoreCore;
    /// Storage scoped to `agent`'s database plane.
    fn agent_store<'a>(&'a self, agent: &'a str) -> Pending<'a, Result<StoreCore>>;
    /// Storage scoped to `user`'s private plane.
    fn user_store<'a>(&'a self, user: &'a str) -> Pending<'a, Result<StoreCore>>;
    /// The application's write gate; state changes that must not interleave with a run's
    /// dispatch are made under it.
    fn gate(&self) -> &tokio::sync::Mutex<()>;
    /// The user a session acts for: the caller, or a shared chat's owner.
    fn actor(&self) -> &str;
    /// Fresh run authority for tool `name` with `args` at `call`'s position: the current
    /// run, or an error when the call may no longer run.
    fn authorize<'a>(
        &'a self,
        call: Call<'a>,
        name: &'a str,
        args: &'a Value,
        cancel: &'a CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// The actor's live relationship to `agent` in session `sid`.
    fn relationship<'a>(
        &'a self,
        sid: &'a str,
        agent: &'a str,
    ) -> Pending<'a, Result<Relationship>>;
    /// Run `name` on the isolated tool worker for session `sid`.
    fn worker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// Write session artifact `kind`/`id` at `call`'s position under a stable receipt, so a
    /// retried call is harmless; fails unless the stored revision is `expected_revision`.
    fn write_artifact<'a>(
        &'a self,
        call: Call<'a>,
        kind: &'a str,
        id: &'a str,
        expected_revision: u64,
        artifact: &'a Value,
    ) -> Pending<'a, Result<Value>>;
    /// The attachment limits of session `sid`'s agent.
    fn attachment_limits<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<AttachmentLimits>>;
    /// Forward an authorized UI Admin `operation` for `tool` to the Administration host.
    fn ui_admin<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        tool: &'a str,
        operation: &'a str,
        args: &'a Value,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// Attach a file to session `sid` as the actor's upload; `body` is an upload request
    /// (`client_upload_id`, `name`, `media_type`, `data`). Returns the upload record.
    fn upload<'a>(&'a self, sid: &'a str, body: Value) -> Pending<'a, Result<Value>>;
    /// The upload record `id` in session `sid`; "Attachment not found" otherwise.
    fn attachment<'a>(&'a self, sid: &'a str, id: &'a str) -> Pending<'a, Result<Value>>;
    /// A media lane pinned to `role` for session `sid`.
    fn media_lane<'a>(&'a self, sid: &'a str, role: ModelRole) -> Pending<'a, Result<Value>>;
    /// The bounded private answer of one call on `lane`.
    fn media_answer<'a>(
        &'a self,
        lane: &'a Value,
        input: Vec<Value>,
        instructions: &'a str,
    ) -> Pending<'a, Result<String>>;
    /// The agent session `sid` belongs to; fails for a missing or archived session.
    fn session_agent_id<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<String>>;
    /// `user`'s live access to `agent`.
    fn live_access<'a>(&'a self, user: &'a str, agent: &'a str) -> Pending<'a, Result<LiveAccess>>;
    /// The current model catalog.
    fn model_catalog(&self) -> Pending<'_, Result<Catalog>>;
    /// Rehydrate `plan`'s platform metadata for the actor's session and freeze it.
    fn pin_model_plan<'a>(&'a self, plan: &'a mut SelectionPlan) -> Pending<'a, Result<()>>;
    /// Dispatch tokens of platform model `pin` for `user` and `agent`.
    fn dispatch_tokens<'a>(
        &'a self,
        user: &'a str,
        agent: &'a str,
        pin: &'a PlatformPin,
    ) -> Pending<'a, Result<Value>>;
    /// Session `sid`'s effective policy, mode revision, ability visibility and ability state.
    fn ability_controls<'a>(
        &'a self,
        sid: &'a str,
    ) -> Pending<'a, Result<(Value, u64, Value, Value)>>;
    /// Session `sid`'s validated capability mode.
    fn session_mode<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>>;
    /// The engine's live permission decision for tool `name` under these controls.
    #[allow(clippy::too_many_arguments)]
    fn live_tool_decision(
        &self,
        access: &LiveAccess,
        name: &str,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        read_only: bool,
    ) -> ToolDecision;
    /// The model a provider selection without one falls back to.
    fn default_agent_model(&self) -> &'static str;
    /// Live dependency authority: the wrapper run `run` in `sid` may still use `tool`.
    fn dependency<'a>(
        &'a self,
        sid: &'a str,
        run: &'a Value,
        tool: &'a str,
    ) -> Pending<'a, Result<()>>;
    /// Send a fixed management API request as the actor.
    fn agent_request<'a>(
        &'a self,
        method: &'a str,
        path: String,
        body: Value,
    ) -> Pending<'a, Result<Value>>;
    /// `agent`'s shared-conversation policy.
    fn shared_chat_policy<'a>(&'a self, agent: &'a str) -> Pending<'a, Result<SharedChatPolicy>>;
    /// Prompt slot `slot` of `agent` as `user` stores it, if any.
    fn prompt_slot<'a>(
        &'a self,
        user: &'a str,
        agent: &'a str,
        slot: &'a str,
    ) -> Pending<'a, Result<Option<Value>>>;
    /// Run installed module tool `tool` for session `sid` at run `run_id`'s pending call.
    fn module_tool_call<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        tool: &'a str,
        args: &'a Value,
        pending_id: &'a str,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// Bounded nonterminal run views in admission order.
    fn active_runs(&self) -> Pending<'_, Result<Vec<Value>>>;
    /// Session `sid`'s follow-up queue.
    fn followup_queue<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>>;
    /// Whether run `status` is terminal in the Engine's run lifecycle.
    fn terminal(&self, status: &str) -> bool;
    /// The actor's live access to `agent` in session `sid`, shared-chat membership included.
    fn session_access<'a>(
        &'a self,
        sid: &'a str,
        agent: &'a str,
    ) -> Pending<'a, Result<LiveAccess>>;
    /// The abilities catalog's ability state with conversation selections `loaded` applied.
    fn selected_state(&self, visibility: &Value, state: &Value, loaded: &Value) -> Value;
    /// The abilities catalog's entries under `policy`, `visibility` and `state`.
    fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value>;
    /// Every tool schema the abilities catalog knows.
    fn all_schemas(&self) -> Vec<Value>;
    /// Whether tool `name` is (available, visible) in the abilities catalog.
    fn tool_gate(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> (bool, bool);
    /// Change session `call.sid` in `agent`'s plane to `mode` for the tool call at `call`; the
    /// outer error is the agent scope, the inner one the rejected change.
    fn change_session_mode<'a>(
        &'a self,
        agent: &'a str,
        call: Call<'a>,
        mode: &'a str,
        expected_revision: u64,
    ) -> Pending<'a, Result<Result<Value>>>;
    /// The skill asset `args` selects for rating, without its text, under live skill access.
    fn rating_asset<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        args: &'a Value,
    ) -> Pending<'a, Result<Value>>;
    /// The agent that owns session `sid`, read through agent store `store`'s planes.
    fn scoped_session_agent_id<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
    ) -> Pending<'a, Result<String>>;
    /// Session `sid`'s effective policy, mode revision, ability visibility and ability state,
    /// read through agent store `store`'s planes.
    fn scoped_ability_controls<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
    ) -> Pending<'a, Result<(Value, u64, Value, Value)>>;
    /// Session `sid`'s validated capability mode, read through agent store `store`'s planes.
    fn scoped_session_mode<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// `user`'s live access to `agent` on agent store `store`'s plane, admitting a shared
    /// chat's participant in `sid`.
    fn scoped_live_access<'a>(
        &'a self,
        store: &'a StoreCore,
        user: &'a str,
        sid: &'a str,
        agent: &'a str,
    ) -> Pending<'a, Result<LiveAccess>>;
    /// The live abilities catalog version.
    fn abilities_version(&self) -> String;
    /// The run that produced tool result `call_id` in session `sid`, with the result.
    fn tool_result_origin<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        call_id: &'a str,
    ) -> Pending<'a, Result<Option<(String, Value)>>>;
    /// The tool result `call_id` of run `rid` in session `sid`.
    fn tool_result<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        rid: &'a str,
        call_id: &'a str,
    ) -> Pending<'a, Result<Option<Value>>>;
    /// The tool name the model called as `call_id` in run `rid` of session `sid`.
    fn tool_call_name<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        rid: &'a str,
        call_id: &'a str,
    ) -> Pending<'a, Result<Option<String>>>;
    /// A page of session `sid`'s public chat after `after`, context-excluded rows withheld.
    fn model_chat_page<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        after: i64,
        limit: i64,
    ) -> Pending<'a, Result<Value>>;
    /// A bounded literal search of `store`'s caller-owned sessions.
    fn search_private_sessions<'a>(
        &'a self,
        store: &'a StoreCore,
        query: &'a str,
        status: &'a str,
        limit: usize,
        continuation: Option<&'a str>,
    ) -> Pending<'a, Result<Value>>;
    /// Whether search `hit` may be shown to the model.
    fn model_search_hit_allowed<'a>(
        &'a self,
        store: &'a StoreCore,
        hit: &'a Value,
    ) -> Pending<'a, Result<bool>>;
    /// The bounded summary of session `sid` in `store`; an error when absent.
    fn session_summary<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// Whether the message of run `rid` in session `sid` may enter the agent's context.
    fn model_message_allowed<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        rid: &'a str,
    ) -> Pending<'a, Result<bool>>;
    /// Agent store `store`'s ordered prompt slots.
    fn prompt_slots<'a>(&'a self, store: &'a StoreCore) -> Pending<'a, Result<Vec<Value>>>;
    /// Edit agent store `store`'s unlocked prompt slot under receipt `pending` bound to
    /// `scope`.
    #[allow(clippy::too_many_arguments)]
    fn self_edit_prompt_slot<'a>(
        &'a self,
        store: &'a StoreCore,
        slot: &'a str,
        content: &'a str,
        mode: &'a str,
        expected_revision: i64,
        pending: &'a str,
        scope: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// Save or (`remove`) delete agent store `store`'s own skill under receipt `pending`
    /// bound to `scope`.
    #[allow(clippy::too_many_arguments)]
    fn mutate_own_skill<'a>(
        &'a self,
        store: &'a StoreCore,
        name: &'a str,
        description: Option<&'a str>,
        instructions: Option<&'a str>,
        mode: Option<&'a str>,
        expected_revision: i64,
        remove: bool,
        pending: &'a str,
        scope: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// The storage owner of session `sid` as `user` may reach it.
    fn shared_chat_owner<'a>(&'a self, sid: &'a str, user: &'a str) -> Pending<'a, Result<String>>;
    /// Record `actor`'s human-help request `request` on owner store `store`'s shared session
    /// `sid`.
    fn shared_help_request<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        actor: &'a str,
        request: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// The chat components of session `sid` in `store`.
    fn chat_components<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
    ) -> Pending<'a, Result<Vec<Value>>>;
    /// The live context compaction policy.
    fn compaction_config(&self) -> Result<CompactionConfig>;
    /// The requester-owned action statuses of `run`.
    fn action_statuses<'a>(&'a self, run: &'a Value) -> Pending<'a, Result<Vec<Value>>>;
    /// The saved result of action `id` in `run`.
    fn action_lookup<'a>(&'a self, run: &'a Value, id: &'a str) -> Pending<'a, Result<Value>>;
    /// Session `sid`'s Base task checkpoint in `store`, fenced to run `rid`.
    fn task_checkpoint<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        rid: &'a str,
    ) -> Pending<'a, Result<Value>>;
    /// `requester`'s context asset selections in session `sid` on agent store `store`'s plane.
    fn context_assets<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        agent: &'a str,
        requester: &'a str,
    ) -> Pending<'a, Result<assets::State>>;
    /// The Engine's context fingerprint of these selections on agent store `store`'s plane.
    #[allow(clippy::too_many_arguments)]
    fn context_fingerprint<'a>(
        &'a self,
        store: &'a StoreCore,
        sid: &'a str,
        agent: &'a str,
        loaded: &'a Value,
        selected: &'a Value,
        skills: &'a Value,
        optimization: &'a Value,
        requester: &'a str,
    ) -> Pending<'a, Result<String>>;
    /// The Engine assets component's bounded plan for `request`.
    fn assets_plan<'a>(
        &'a self,
        request: &'a assets::Request,
        cancel: &'a CancellationToken,
    ) -> Pending<'a, Result<assets::Plan>>;
}

/// Whether tool `name` only reads, from its tool folder's `effect`; an unknown tool counts
/// as a write.
#[must_use]
pub fn is_read(name: &str) -> bool {
    webagent_abilities::tool_definitions::get(name).is_some_and(|d| d.is_read())
}

/// Schemas for `names` from their tool folders. A tool whose folder is missing or invalid
/// is left out, so the model never sees it and the catalog marks it unsupported.
#[must_use]
pub fn schemas(names: &[&str]) -> Vec<Value> {
    use webagent_abilities::tool_definitions as definitions;
    names
        .iter()
        .filter_map(|name| definitions::get(name))
        .map(definitions::ToolDefinition::schema)
        .collect()
}
