//! Administration over the shared host core.
//!
//! The Administrator tool handlers, session notices and artifacts, source promotion,
//! build dashboard broker calls, the renewal and repository watchers and the
//! installation service setting live here. What belongs to other modules (run
//! authority, live tool decisions, sessions, policy and the worker and broker sockets)
//! comes through [`AdministrationHost`], which the root implements; a trait object keeps
//! this code compiled here rather than instantiated in the root crate.
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_host_core::StoreCore;

pub mod artifacts;
pub mod dashboard;
pub mod notices;
pub mod promotion;
pub mod services;
pub mod tools;
pub mod watch;
mod workspace;

pub use workspace::workspace_host;

/// A boxed `Send` future borrowed from the host.
pub type Pending<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The caller's live access in one session: one decision per requested tool and a
/// fingerprint of every control the decisions read, so a caller can detect a change
/// across slow I/O.
#[derive(Clone, Debug)]
pub struct SessionAccess {
    /// The session's agent.
    pub agent: String,
    /// Whether the session is active rather than archived or closed.
    pub active: bool,
    /// Whether the caller's live membership admits them to the agent.
    pub admitted: bool,
    /// One decision per requested `(tool, ability)`, in order.
    pub permissions: Vec<Permission>,
    /// The agent's ability settings, by ability.
    pub settings: Value,
    /// Digest of the caller, session, agent, policy, ability controls, mode and access.
    pub fingerprint: String,
}

/// What Administration needs from the rest of the application for one caller.
pub trait AdministrationHost: Sync {
    /// The caller's storage handle.
    fn store(&self) -> &StoreCore;
    /// The user a session acts for: the caller, or a shared chat's owner.
    fn actor(&self) -> &str;
    /// The application's write gate; receipts and promotions are claimed under it.
    fn gate(&self) -> &tokio::sync::Mutex<()>;
    /// The agent a session belongs to.
    fn session_agent<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<String>>;
    /// One run record.
    fn run<'a>(&'a self, run_id: &'a str) -> Pending<'a, Result<Value>>;
    /// Live decisions for `tools` (each `(tool, ability)`) in session `sid`, or `None`
    /// when the caller has no such session.
    fn access<'a>(
        &'a self,
        sid: &'a str,
        tools: &'a [(&'a str, &'a str)],
    ) -> Pending<'a, Result<Option<SessionAccess>>>;
    /// Whether the session's actor is admitted to its agent and an installation
    /// administrator now.
    fn administrator<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<bool>>;
    /// The authority fingerprint for Administrator call `name` in run `run_id`; an error
    /// when the caller may not run it now.
    fn fingerprint<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        name: &'a str,
    ) -> Pending<'a, Result<String>>;
    /// [`fingerprint`](Self::fingerprint) for a call being admitted, with a refusal
    /// classified for the run record.
    fn admit<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        name: &'a str,
        args: &'a Value,
    ) -> Pending<'a, Result<String>>;
    /// Run `name` on the isolated tool worker.
    fn worker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        trusted: Option<Value>,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// Send `name` to the protected build broker.
    fn broker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        trusted: Option<Value>,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>>;
    /// Whether the global shutoff is active.
    fn shutoff(&self) -> Pending<'_, Result<bool>>;
    /// The installation tool policy.
    fn policy(&self) -> Pending<'_, Result<Value>>;
    /// One tool's rule in `policy`: `allow`, `require_approval` or `deny`.
    fn rule(&self, policy: &Value, name: &str) -> String;
    /// Write `docs` and one `kind` event for session `sid` in one transaction, in
    /// `agent`'s storage view when given.
    fn record<'a>(
        &'a self,
        agent: Option<&'a str>,
        docs: Vec<(String, Value)>,
        sid: &'a str,
        kind: &'a str,
        payload: &'a Value,
    ) -> Pending<'a, Result<()>>;
    /// The caller's repository connections for session `sid`, with credential
    /// references.
    fn repository<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>>;
    /// Validated performance settings: a run's requested `settings`, else the defaults.
    fn optimization_settings(&self, settings: Option<Value>) -> Result<Value>;
    /// The installed optimization plugin's generation, if one is installed.
    fn optimization_generation(&self) -> Option<String>;
    /// The content-free progress view of a run.
    fn progress(&self, run: &Value) -> Value;
    /// The live ability bundle version.
    fn abilities_version(&self) -> String;
    /// The promotion broker socket.
    fn promotion_socket(&self) -> &Path;
    /// A token cancelled when the application stops.
    fn stopping(&self) -> CancellationToken;
}

/// The trusted identity block the broker checks on every request.
#[must_use]
pub fn trusted(user: &str, sid: &str, fingerprint: &Value) -> Value {
    serde_json::json!({"user_id":user,"session_id":sid,
        "lease_until":webagent_host_core::now()+30,"access_fingerprint":fingerprint})
}

#[cfg(test)]
mod tests;
