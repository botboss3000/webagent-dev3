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
impl webagent_engine::runtime::ports::NativePolicy for NativePolicy {
    fn default_policy_schemas() -> Vec<Value> {
        tools::administrator::schemas()
            .into_iter()
            .chain(tools::native::mode::schemas())
            .chain(tools::native::search::schemas())
            .chain(tools::native::compaction::schemas())
            .chain(tools::native::recall::schemas())
            .chain(tools::native::self_edit::schemas())
            .chain(crate::session_messaging::schemas())
            .chain(crate::main_chat::schemas())
            .chain(tools::native::task::schemas())
            .chain(tools::native::tool_search::schemas())
            .chain(tools::native::schemas(tools::native::decision::TOOLS))
            .chain(tools::native::schemas(
                tools::native::directory_decision::TOOLS,
            ))
            .chain(tools::native::schemas(tools::native::user_files::TOOLS))
            .chain(tools::native::schemas(tools::native::model_switcher::TOOLS))
            .chain(tools::core_agent_management::schemas())
            .chain(tools::native::schemas(tools::native::image_vision::TOOLS))
            .chain(tools::core_image_generation::schemas())
            .chain(tools::native::schemas(tools::native::app_control::TOOLS))
            .chain(tools::native::schemas(tools::native::ui_admin::TOOLS))
            .chain(tools::native::functions::schemas())
            .chain(tools::native::schemas(tools::native::orchestration::TOOLS))
            .chain(tools::native::base_extras::schemas())
            .chain(tools::native::webhooks::schemas())
            .chain(tools::native::schemas(tools::native::visualizer::TOOLS))
            .chain(tools::native::schemas(tools::native::vault::TOOLS))
            .chain(tools::native::schemas(tools::native::visual_capture::TOOLS))
            .chain(tools::native::schemas(
                tools::native::human_escalation::TOOLS,
            ))
            .chain(tools::native::schemas(tools::native::reply_buttons::TOOLS))
            .chain(tools::native::schemas(
                tools::native::knowledge_files::TOOLS,
            ))
            .chain(tools::native::schemas(tools::native::wiki::TOOLS))
            .chain(modules::schemas())
            .chain(mcp::all_schemas())
            .collect()
    }
}

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
        tools::native::tool_search::marker(name)
    }
    fn apply_tool_selection(run: &mut Value, result: &Value) {
        tools::native::tool_search::apply_selection(run, result)
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
        origin: Option<&'a mut Value>,
    ) -> impl std::future::Future<Output = Result<bool>> + Send + 'a {
        store.followup_consume(sid, choice, run, origin)
    }
    fn followup_hold<'a>(
        &'a self,
        store: &'a Store,
        sid: &'a str,
        reason: &'a str,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        store.followup_hold(sid, reason)
    }
}
