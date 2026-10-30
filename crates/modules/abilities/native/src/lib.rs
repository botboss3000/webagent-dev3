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
use webagent_chat::shared::SharedChatPolicy;
use webagent_files::uploads::AttachmentLimits;
use webagent_host_core::StoreCore;
use webagent_models::account_catalog::PlatformPin;
use webagent_models::model_selection::{Catalog, ModelRole, SelectionPlan};

pub mod app_control;
pub mod base_extras;
pub mod decision;
pub mod directory_decision;
pub mod functions;
pub mod image_vision;
pub mod knowledge_files;
pub mod mode;
pub mod model_switcher;
pub mod orchestration;
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
