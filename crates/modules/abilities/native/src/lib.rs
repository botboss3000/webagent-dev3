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
use webagent_access::Relationship;
use webagent_files::uploads::AttachmentLimits;
use webagent_host_core::StoreCore;
use webagent_models::model_selection::ModelRole;

pub mod app_control;
pub mod functions;
pub mod image_vision;
pub mod knowledge_files;
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
