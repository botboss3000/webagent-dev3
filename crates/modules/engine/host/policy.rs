//! Composition of existing Agents and Chat policy data for Engine enforcement.
use crate::{abilities, access_policy, mcp, modules, tools, App, Store, DEFAULT_AGENT_MODEL};
use anyhow::Result;
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Clone)]
pub(crate) struct NativePolicy {
    pub(crate) host: crate::EngineHostState,
}
impl NativePolicy {
    pub(crate) fn new(app: &App) -> Self {
        Self {
            host: crate::EngineHostState::new(app),
        }
    }
}
impl webagent_engine::runtime::ports::AgentPolicy<Store> for NativePolicy {
    fn default_model() -> &'static str {
        DEFAULT_AGENT_MODEL
    }
    fn selection<'a>(
        &'a self,
        store: &'a Store,
    ) -> impl std::future::Future<Output = Result<webagent_models::provider::ProviderSelection>>
           + Send
           + 'a {
        async move {
            let value = store.agent_model().await?;
            webagent_models::provider::ProviderSelection::parse(&value, DEFAULT_AGENT_MODEL)
        }
    }
    fn modes<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        crate::agents_api::modes_config(store, agent)
    }
    fn resolve_mode(config: &Value, id: &str) -> Result<Value> {
        webagent_agents::modes::resolve(config, id)
    }
    fn removed_mode() -> Value {
        webagent_agents::modes::defaults()[0].clone()
    }
    fn abilities<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        store.agent_abilities_for(agent)
    }
    fn main_chat_schedule_approval<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<String>>> + Send + 'a {
        async move {
            let state = Box::pin(store.for_agent(agent).await?.main_chat_state()).await?;
            Ok(
                (state["settings"]["enabled"] == true && state["session_id"] == sid)
                    .then(|| {
                        state["settings"]["schedule_approval"]
                            .as_str()
                            .map(str::to_owned)
                    })
                    .flatten(),
            )
        }
    }
    fn unresolved_model_roles<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
        roles: &'a [String],
    ) -> impl std::future::Future<Output = Result<Vec<String>>> + Send + 'a {
        async move {
            let catalog = store.model_catalog().await?;
            // Unknown role keys fail closed: the ability stays hidden.
            Ok(roles
                .iter()
                .filter(|role| {
                    serde_json::from_value::<webagent_models::model_selection::ModelRole>(json!(
                        role
                    ))
                    .map_or(true, |parsed| catalog.resolve_role(agent, parsed).is_err())
                })
                .cloned()
                .collect())
        }
    }
}
impl webagent_engine::runtime::ports::NativePolicy for NativePolicy {}

impl webagent_engine::runtime::ports::CredentialBindings<Store> for NativePolicy {
    fn declares(name: &str) -> bool {
        crate::credential_authorization::declaration(name).is_some()
    }
    fn valid<'a>(
        &'a self,
        store: &'a Store,
        run: &'a Value,
        pending: &'a Value,
    ) -> impl std::future::Future<Output = Result<bool>> + Send + 'a {
        crate::credential_authorization::valid(store, run, pending)
    }
    fn resolve<'a>(
        &'a self,
        store: &'a Store,
        run: &'a Value,
        name: &'a str,
        args: &'a Value,
    ) -> impl std::future::Future<Output = Result<Option<Value>>> + Send + 'a {
        crate::credential_authorization::resolve(store, run, name, args)
    }
}

impl webagent_engine::runtime::ports::SkillSources<Store> for NativePolicy {
    fn own_skills<'a>(
        &'a self,
        store: &'a Store,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        store.list_own_skills()
    }
    fn tool_marker(name: &str) -> String {
        webagent_abilities::selection::marker(name)
    }
    fn apply_skill_selection(run: &mut Value, result: &Value) {
        tools::core_skills::apply_selection(run, result)
    }
}
impl webagent_engine::runtime::ports::PromptSlots<Store> for NativePolicy {
    fn prompt_slots<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
    ) -> impl std::future::Future<Output = Result<Vec<Value>>> + Send + 'a {
        store.list_agent_prompt_slots(agent)
    }
}
impl webagent_engine::runtime::ports::MainGuidance<Store> for NativePolicy {
    fn main_guidance<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<String>>> + Send + 'a {
        crate::main_chat::guidance_store(store, agent, sid)
    }
    fn main_notes<'a>(
        &'a self,
        store: &'a Store,
        agent: &'a str,
        sid: &'a str,
        requester: &'a str,
        basis: &'a str,
    ) -> impl std::future::Future<Output = Result<(Vec<(String, String)>, Vec<(u64, String)>)>> + Send + 'a
    {
        crate::main_chat::notes_context_store(store, agent, sid, requester, basis)
    }
    fn session_tasks<'a>(
        &'a self,
        store: &'a Store,
        sid: &'a str,
        basis: &'a str,
        calls: u64,
        filed: bool,
    ) -> impl std::future::Future<Output = Result<webagent_engine::runtime::ports::TaskContext>>
           + Send
           + 'a {
        store.task_list_context(sid, basis, calls, filed)
    }
    fn session_tasks_end<'a>(
        &'a self,
        store: &'a Store,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Option<String>>> + Send + 'a {
        store.task_list_end_reminder(sid)
    }
}

impl webagent_engine::runtime::ports::ModelDiagnostics<Store> for NativePolicy {
    fn provider_enabled(&self, store: &Store) -> bool {
        crate::diagnostics::provider_enabled(store)
    }
    fn failure(&self, store: &Store, evidence: &Value, error: &anyhow::Error, action: &str) {
        crate::diagnostics::failure(store, evidence, error, action)
    }
}

impl webagent_engine::runtime::ports::InstallationAccess<Store> for NativePolicy {
    fn installation_access<'a>(
        &'a self,
        store: &'a Store,
        user: &'a str,
        agent: &'a str,
    ) -> impl std::future::Future<Output = Result<access_policy::LiveAccess>> + Send + 'a {
        store.live_access(user, agent)
    }
}

impl webagent_engine::runtime::ports::ContextDiagnostics<Store> for NativePolicy {
    fn assembly(
        &self,
        store: &Store,
        run: &Value,
        request: &crate::context_input::Request<'_>,
        result: &Result<crate::context_assembly::Prepared<'_>>,
    ) -> Option<Value> {
        crate::diagnostics::assembly(store, run, request, result)
    }
    fn validation(
        &self,
        store: &Store,
        run: &Value,
        code: &'static str,
        stage: &'static str,
    ) -> Option<Value> {
        crate::diagnostics::validation(store, run, code, stage)
    }
    fn summary(
        &self,
        store: &Store,
        run: &Value,
        request: &crate::context_input::Request<'_>,
        result: &Result<()>,
    ) -> Option<Value> {
        crate::diagnostics::summary(store, run, request, result)
    }
}

impl webagent_engine::runtime::ports::ProviderUsage for NativePolicy {
    fn record_usage(run: &mut Value, response: &Value) {
        crate::usage::record(run, response)
    }
}

impl webagent_engine::runtime::ports::StartupPolicy<Store> for NativePolicy {
    fn startup_resume<'a>(
        &'a self,
        store: &'a Store,
    ) -> impl std::future::Future<Output = Result<bool>> + Send + 'a {
        crate::services::startup_resume_enabled(store)
    }
}

impl webagent_engine::runtime::ports::PublicationObservation<Store, abilities::Catalog>
    for NativePolicy
{
    fn record_source_adoption(&self, store: &Store, catalog: &Arc<abilities::Catalog>) {
        match crate::live_assets::catalog(catalog) {
            Ok(snapshot) => {
                let _ = crate::live_assets::record_adoption(&store.root, "loop", &snapshot);
            }
            Err(_) => tracing::error!("Ability catalog unavailable; adoption skipped"),
        }
    }
}

impl webagent_engine::runtime::followup::FollowupQueue<Store> for NativePolicy {
    fn followup_queue<'a>(
        &'a self,
        store: &'a Store,
        sid: &'a str,
    ) -> impl std::future::Future<Output = Result<Value>> + Send + 'a {
        store.followup_queue(sid)
    }
    fn followup_consume<'a>(
        &'a self,
        store: &'a Store,
        sid: &'a str,
        choice: &'a str,
        run: &'a mut Value,
    ) -> impl std::future::Future<Output = Result<bool>> + Send + 'a {
        store.followup_consume(sid, choice, run)
    }
}
