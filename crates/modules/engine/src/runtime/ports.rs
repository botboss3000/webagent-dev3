//! Borrowed existing owner resources; no Engine state transition is delegated here.
use crate::ports::{AgentMetadata, Records};
use anyhow::Result;
use serde_json::Value;
use sqlx::SqlitePool;
use std::future::Future;
pub trait ScopedStorage:
    Records + AgentMetadata<Agent = Self> + Clone + Send + Sync + 'static
{
    fn user_pool(&self) -> &SqlitePool;
    fn application_pool(&self) -> &SqlitePool;
    fn agent_id(&self) -> &str;
    fn configure_cache(&self, documents: usize, contexts: usize);
    fn for_user<'a>(&'a self, user: &'a str) -> impl Future<Output = Result<Self>> + Send + 'a;
    fn all_user_ids(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    /// Accounts a periodic scan must visit now; idle, unchanged accounts are left closed.
    fn scan_user_ids(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn get<'a>(&'a self, key: &'a str) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
    fn put<'a>(
        &'a self,
        key: &'a str,
        value: &'a Value,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    /// Fails unless the run is still the session's live, leased dispatch of `pending`.
    fn native_fence<'a>(
        &'a self,
        sid: &'a str,
        rid: &'a str,
        pending: &'a str,
        generation: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}
/// Existing public API projection remains with its Usage/Abilities privacy owners.
pub trait RunPresentation: Sync {
    fn present_run(&self, run: &Value) -> Value;
}
/// The existing diagnostics observer is notified only for an actually committed revision.
pub trait RunObservation: Sync {
    fn committed(&self, run: &Value, kind: &str, override_save: bool);
}

/// A resolved Chat binding keeps its original concrete owner type; no JSON conversion or cached live grant.
pub trait SessionBinding: Clone + Send + Sync + 'static {
    fn can_write(&self) -> bool;
    fn session_id(&self) -> &str;
}
pub trait ChatAuthority<S>: Clone + Send + Sync + 'static {
    type Binding: SessionBinding;
    fn resolve<'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
        user: &'a str,
    ) -> impl Future<Output = Result<Self::Binding>> + Send + 'a;
    fn live_access<'a>(
        &'a self,
        store: &'a S,
        user: &'a str,
        sid: &'a str,
        agent: &'a str,
    ) -> impl Future<Output = Result<webagent_access::authority::LiveAccess>> + Send + 'a;
    fn operation<'a, T: Send + 'a, F: Future<Output = Result<T>> + Send + 'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
        cancel: tokio_util::sync::CancellationToken,
        operation: F,
    ) -> impl Future<Output = Result<T>> + Send + 'a;
    fn agent_id<'a>(
        &'a self,
        store: &'a S,
        sid: &'a str,
    ) -> impl Future<Output = Result<String>> + Send + 'a;
    fn messaging_owns(name: &str) -> bool;
    fn messaging_policy_defaults(policy: Value) -> Value;

    fn participation_allows_run<'a>(
        &'a self,
        store: &'a S,
        run: &'a Value,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;
}

/// Agents owns named execution mode definitions and encrypted agent capability settings.
pub trait AgentPolicy<S>: Clone + Send + Sync + 'static {
    fn default_model() -> &'static str;
    fn selection<'a>(
        &'a self,
        store: &'a S,
    ) -> impl Future<Output = Result<webagent_models::provider::ProviderSelection>> + Send + 'a;
    fn modes<'a>(
        &'a self,
        store: &'a S,
        agent: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    fn resolve_mode(config: &Value, id: &str) -> Result<Value>;
    fn removed_mode() -> Value;
    fn abilities<'a>(
        &'a self,
        store: &'a S,
        agent: &'a str,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    /// Role keys (for example `image_out`) the agent's model roster cannot resolve now.
    fn unresolved_model_roles<'a>(
        &'a self,
        store: &'a S,
        agent: &'a str,
        roles: &'a [String],
    ) -> impl Future<Output = Result<Vec<String>>> + Send + 'a;
}
/// Existing native owners enumerate their supported policy defaults; Engine composes enforcement.
pub trait NativePolicy: Clone + Send + Sync + 'static {
    fn default_policy_schemas() -> Vec<Value>;
}

/// Modules retains its current admission and package lease authority.
pub trait ModuleBindings<S>: Clone + Send + Sync + 'static {
    fn binding_valid<'a>(
        &'a self,
        store: &'a S,
        run: &'a Value,
    ) -> impl Future<Output = bool> + Send + 'a;
    fn binding_lease<'a>(
        &'a self,
        store: &'a S,
        run: &'a Value,
    ) -> impl Future<Output = Result<Option<std::fs::File>>> + Send + 'a;
}

pub trait CredentialBindings<S>: Clone + Send + Sync + 'static {
    fn declares(name: &str) -> bool;
    fn valid<'a>(
        &'a self,
        store: &'a S,
        run: &'a Value,
        pending: &'a Value,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;
    fn resolve<'a>(
        &'a self,
        store: &'a S,
        run: &'a Value,
        name: &'a str,
        args: &'a Value,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
}

/// Current immutable Abilities selection/body sources; no grant decisions cross this boundary.
pub trait SkillSources<S>: Clone + Send + Sync + 'static {
    fn own_skills<'a>(&'a self, store: &'a S) -> impl Future<Output = Result<Value>> + Send + 'a;
    fn tool_marker(name: &str) -> String;
    fn apply_tool_selection(run: &mut Value, result: &Value);
    fn apply_skill_selection(run: &mut Value, result: &Value);
}
pub trait PromptSlots<S>: Clone + Send + Sync + 'static {
    fn prompt_slots<'a>(
        &'a self,
        store: &'a S,
        agent: &'a str,
    ) -> impl Future<Output = Result<Vec<Value>>> + Send + 'a;
}
pub trait MainGuidance<S>: Clone + Send + Sync + 'static {
    fn main_guidance<'a>(
        &'a self,
        store: &'a S,
        agent: &'a str,
        sid: &'a str,
    ) -> impl Future<Output = Result<Option<String>>> + Send + 'a;
}

pub trait ModelDiagnostics<S>: Clone + Send + Sync + 'static {
    fn provider_enabled(&self, store: &S) -> bool;
    fn failure(&self, store: &S, evidence: &Value, error: &anyhow::Error, action: &str);
}

/// Access remains the owner of fresh installation/account/agent authority.
pub trait InstallationAccess<S>: Clone + Send + Sync + 'static {
    fn installation_access<'a>(
        &'a self,
        store: &'a S,
        user: &'a str,
        agent: &'a str,
    ) -> impl Future<Output = Result<webagent_access::authority::LiveAccess>> + Send + 'a;
}

/// Diagnostics is an optional external observer; it neither validates nor authorizes Engine transitions.
pub trait ContextDiagnostics<S>: Clone + Send + Sync + 'static {
    fn assembly(
        &self,
        store: &S,
        run: &Value,
        request: &crate::context_input::Request<'_>,
        result: &anyhow::Result<crate::context_assembly::Prepared<'_>>,
    ) -> Option<Value>;
    fn validation(
        &self,
        store: &S,
        run: &Value,
        code: &'static str,
        stage: &'static str,
    ) -> Option<Value>;
    fn summary(
        &self,
        store: &S,
        run: &Value,
        request: &crate::context_input::Request<'_>,
        result: &anyhow::Result<()>,
    ) -> Option<Value>;
}

/// Usage retains normalized response accounting; Engine only chooses when a completed response is recorded.
pub trait ProviderUsage: Clone + Send + Sync + 'static {
    fn record_usage(run: &mut Value, response: &Value);
}

/// Services owns the existing optional boot resume setting.
pub trait StartupPolicy<S>: Clone + Send + Sync + 'static {
    fn startup_resume<'a>(&'a self, store: &'a S)
        -> impl Future<Output = Result<bool>> + Send + 'a;
}

/// Storage/Chat retain canonical history and physical queue/retirement infrastructure.
pub trait SessionRecords: Sync {
    fn context<'a>(&'a self, sid: &'a str) -> impl Future<Output = Result<Vec<Value>>> + Send + 'a;
    fn shared_model_context<'a>(
        &'a self,
        sid: &'a str,
    ) -> impl Future<Output = Result<Vec<Value>>> + Send + 'a;
    fn drain_message_queues(&self) -> impl Future<Output = Result<()>> + Send;
    fn cleanup_one_reasoning(&self) -> impl Future<Output = Result<bool>> + Send;
}
/// Existing publication provenance is observed by its owner, with no Engine state decision.
pub trait PublicationObservation<S, B>: Clone + Send + Sync + 'static {
    fn record_source_adoption(&self, store: &S, catalog: &std::sync::Arc<B>);
}

pub use super::followup::FollowupQueue;
