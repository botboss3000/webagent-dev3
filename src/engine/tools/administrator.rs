//! Root composition of the Administration module (`webagent-administration-host`): the
//! [`AdministrationHost`] it calls back into, run authority for Administrator calls, and
//! the routing of Source Control adapters that stay with their abilities.
//! Model arguments never choose identity, agent, workspace, credentials, host
//! origin or a Docker command.
use super::failure::{boundary, request, Code, Delivery, Failure, Stage};
use crate::{
    ability_controls, access, live_tool_decision, policy_decision, policy_default,
    session_agent_id, session_mode, task_progress, tools, App,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_administration_host::{AdministrationHost, Pending, SessionAccess};
use webagent_configuration::digest;

/// Whether `name` is an Administrator native tool.
pub(crate) fn supported_schema_name(name: &str) -> bool {
    webagent_administration::tools::supported(name)
}

/// The ability folder whose plugin serves Administrator tool `name`, if a plugin does.
pub(crate) fn plugin(name: &str) -> Option<&'static str> {
    webagent_administration::tools::plugin(name)
}

/// Whether the Administrator handlers execute `name`.
pub fn owns(name: &str) -> bool {
    webagent_administration::tools::owns(name)
}

/// Administrator tools the definition check counts as handled: the compiled ones when the
/// optional pack has folders in the current catalog, else none, since without the pack
/// its compiled tools have no folders by design.
pub(crate) fn pack_tools() -> Vec<String> {
    if !webagent_administration::tools::installed() {
        return Vec::new();
    }
    webagent_administration::tools::TOOLS
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

/// Administrator schemas from the tool folders this process reads definitions from,
/// normalized for the model. Catalog snapshots carry their own copy.
pub fn schemas() -> Vec<Value> {
    let definitions = webagent_administration::tools::TOOLS
        .iter()
        .filter_map(|name| webagent_abilities::tool_definitions::get(name))
        .map(webagent_abilities::tool_definitions::ToolDefinition::schema)
        .collect();
    tools::normalize_native_schemas(definitions)
}

/// The tool worker's live-checkout grant for `name` in this call of `run` in session `sid`:
/// `trusted` with `"checkout": true` added when the Administrator pack is installed and
/// Codebase Admin itself supplies the tool here, else `trusted` unchanged. The engine has
/// already authorized the call itself; without this grant the worker confines file tools
/// and commands to the session workspace. A lookup failure grants nothing.
pub(crate) async fn checkout_grant(
    app: &App,
    sid: &str,
    run: &Value,
    name: &str,
    trusted: Option<Value>,
) -> Option<Value> {
    if !webagent_administration::tools::installed()
        // Boxed so the lookups do not grow the shared dispatch future's stack frame.
        || !Box::pin(codebase_admin_supplies(app, sid, run, name))
            .await
            .unwrap_or(false)
    {
        return trusted;
    }
    let mut trusted = trusted.unwrap_or_else(|| json!({}));
    trusted["checkout"] = json!(true);
    Some(trusted)
}

/// Whether Codebase Admin can supply `name` to this call: it declares the tool, is not off
/// for the agent or suppressed in session `sid` (activation is not needed), and the
/// caller's live access is admitted and does not deny the ability. The same tool offered by another ability (Core's
/// `shell`) does not qualify.
async fn codebase_admin_supplies(app: &App, sid: &str, run: &Value, name: &str) -> Result<bool> {
    if !app.abilities.has_tool(CODEBASE_ADMIN, name) {
        return Ok(false);
    }
    let agent = session_agent_id(app, sid).await?;
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(&agent).await?;
    let (_, _, visibility, state) = ability_controls(&scoped, sid).await?;
    let loaded = &run["loaded_abilities"];
    if !scoped
        .abilities
        .supplies(CODEBASE_ADMIN, name, &visibility, &state, loaded)
    {
        return Ok(false);
    }
    let actor = crate::shared_chat::actor(&scoped);
    let access = crate::shared_chat::live_access(&scoped.store, actor, sid, &agent).await?;
    Ok(access.admitted() && access.ability_permission(CODEBASE_ADMIN) != access::Permission::Deny)
}

const CODEBASE_ADMIN: &str = "codebase_admin";

pub fn build_socket(app: &App) -> std::path::PathBuf {
    app.build_socket.clone()
}

/// The module's view of one caller.
pub(crate) struct Host<'a>(pub(crate) &'a App);

fn refused(code: Code) -> anyhow::Error {
    Failure::new(Stage::Authorization, code, Delivery::NotSent).into()
}

/// The authority fingerprint for Administrator call `name` in run `run_id`: the caller,
/// session, agent, live access, policy, ability controls, mode and loaded abilities. An
/// error means the caller may not run it now; a changed fingerprint revokes a running
/// job's lease.
pub(crate) async fn host_fingerprint(
    app: &App,
    sid: &str,
    run_id: &str,
    name: &str,
) -> Result<String> {
    if app.shutoff().await? {
        return Err(refused(Code::AuthorityStopped));
    }
    let agent = session_agent_id(app, sid).await?;
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(&agent).await?;
    let run = scoped.run(run_id).await?;
    if run["session_id"] != sid {
        return Err(refused(Code::ScopeChanged));
    }
    let performance = name == "performance_work";
    if performance && !webagent_administration::arguments::performance_run_live(&run) {
        return Err(refused(Code::AuthorityStopped));
    }
    let (policy, revision, visibility, state) = ability_controls(&scoped, sid).await?;
    let mode = session_mode(&scoped, sid).await?;
    let actor = crate::shared_chat::actor(&scoped);
    let access = crate::shared_chat::live_access(&scoped.store, actor, sid, &agent).await?;
    if performance && !crate::optimization::available() {
        return Err(refused(Code::AuthorityStopped));
    }
    let read_only = mode["mode"] == "read_only";
    let work_finish = name == "build_work_finish";
    let policy_name = if work_finish { "build_work" } else { name };
    let loaded = &run["loaded_abilities"];
    let work_permission = live_tool_decision(
        &scoped,
        &access,
        policy_name,
        &policy,
        &visibility,
        &state,
        loaded,
        read_only,
    )
    .for_execution_mode(&mode)
    .permission;
    if !access.principal.administrator()
        || !access.admitted()
        || work_permission == access::Permission::Deny
    {
        return Err(refused(Code::AuthorizationRejected));
    }
    // A deploy (finish or decide) asks first unless the session runs in Auto mode, even
    // when build_work itself is allowed: this gate is fixed, not a policy rule.
    let deploy_gate = access
        .tool_decision_for_abilities(
            "build_work",
            &["codebase_admin".into()],
            access::Permission::Ask,
            true,
            true,
            read_only,
            false,
        )
        .for_execution_mode(&mode)
        .permission;
    if work_finish
        && (deploy_gate == access::Permission::Deny
            || (deploy_gate == access::Permission::Ask
                && work_permission != access::Permission::Ask))
    {
        return Err(refused(Code::DeploymentAuthorizationRejected));
    }
    // An Ask finish reaches this host only after explicit approval. Its queued job keeps
    // that authorization while the scoped policy fingerprint stays valid. Any deployment
    // replaces the ability bundle while broker work (finish or check) is queued or
    // running: policy, activation and access stay in the fingerprint, but bundle bytes
    // alone cannot revoke broker work.
    let bundle = if policy_name == "build_work" {
        Value::Null
    } else {
        json!(scoped.abilities.live_version())
    };
    let generation = if performance {
        crate::optimization::generation()
    } else {
        None
    };
    // Broker work keeps running while the turn goes on, so loading or activating an
    // unrelated ability must not revoke it: build work tracks only whether Codebase Admin
    // stays active, unsuppressed and loaded. Other tools track the whole ability state.
    let (state, loaded) = if policy_name == "build_work" {
        let has = |list: &Value| {
            list.as_array()
                .is_some_and(|a| a.iter().any(|id| id == "codebase_admin"))
        };
        (
            json!([has(&state["active"]), has(&state["suppressed"])]),
            json!(has(loaded)),
        )
    } else {
        (state, loaded.clone())
    };
    Ok(digest(&json!([
        actor,
        sid,
        agent,
        access.context_fingerprint(),
        policy,
        revision,
        visibility,
        state,
        mode,
        loaded,
        bundle,
        generation
    ])))
}

impl AdministrationHost for Host<'_> {
    fn store(&self) -> &webagent_host_core::StoreCore {
        &self.0.store
    }
    fn actor(&self) -> &str {
        crate::shared_chat::actor(self.0)
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.0.gate
    }
    fn session_agent<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<String>> {
        Box::pin(session_agent_id(self.0, sid))
    }
    fn run<'a>(&'a self, run_id: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(self.0.run(run_id))
    }
    fn rank<'a>(
        &'a self,
        query: &'a str,
        items: Vec<Vec<String>>,
    ) -> Pending<'a, Option<Vec<f32>>> {
        Box::pin(crate::api::inference::rank(query, items))
    }
    fn session_title<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Option<String>>> {
        Box::pin(async move {
            let session = self.0.store.get(&crate::key("session", sid)).await?;
            Ok(session.and_then(|session| session["title"].as_str().map(str::to_owned)))
        })
    }
    /// Decisions use the caller's own live membership (`store.live_access`) and each
    /// tool's ability as the loaded set.
    fn access<'a>(
        &'a self,
        sid: &'a str,
        tools: &'a [(&'a str, &'a str)],
    ) -> Pending<'a, Result<Option<SessionAccess>>> {
        Box::pin(async move {
            let app = self.0;
            let Some(session) = app.store.get(&crate::key("session", sid)).await? else {
                return Ok(None);
            };
            let agent = session["agent_id"]
                .as_str()
                .unwrap_or("shared_default")
                .to_owned();
            let mut scoped = app.clone();
            scoped.store = app.store.for_agent(&agent).await?;
            let (policy, revision, visibility, state) = ability_controls(&scoped, sid).await?;
            let mode = session_mode(&scoped, sid).await?;
            let user = scoped.store.current_user_id();
            let access = scoped.store.live_access(user, &agent).await?;
            let read_only = mode["mode"] == "read_only";
            let permissions = tools
                .iter()
                .map(|(tool, ability)| {
                    let loaded = json!([ability]);
                    let decision = live_tool_decision(
                        &scoped,
                        &access,
                        tool,
                        &policy,
                        &visibility,
                        &state,
                        &loaded,
                        read_only,
                    );
                    decision.permission
                })
                .collect();
            let fingerprint = digest(&json!([
                app.store.current_user_id(),
                sid,
                agent,
                policy,
                revision,
                visibility,
                state,
                mode,
                access.context_fingerprint()
            ]));
            Ok(Some(SessionAccess {
                active: session["status"] == "active",
                admitted: access.admitted(),
                permissions,
                settings: visibility["settings"].clone(),
                fingerprint,
                agent,
            }))
        })
    }
    fn administrator<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<bool>> {
        Box::pin(async move {
            let app = self.0;
            let agent = session_agent_id(app, sid).await?;
            let actor = crate::shared_chat::actor(app);
            let access = crate::shared_chat::live_access(&app.store, actor, sid, &agent).await?;
            Ok(access.admitted() && access.principal.administrator())
        })
    }
    fn fingerprint<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        name: &'a str,
    ) -> Pending<'a, Result<String>> {
        Box::pin(host_fingerprint(self.0, sid, run_id, name))
    }
    fn admit<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        name: &'a str,
        args: &'a Value,
    ) -> Pending<'a, Result<String>> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            host_fingerprint(self.0, sid, run_id, name)
                .await
                .map_err(|error| boundary(error, Stage::Authorization, Delivery::NotSent, started))
                .map_err(|error| request(error, args))
        })
    }
    fn worker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        trusted: Option<Value>,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::execute_trusted(
            name,
            args,
            sid,
            &self.0.tool_socket,
            trusted,
            cancel,
        ))
    }
    fn checkout<'a>(
        &'a self,
        sid: &'a str,
        run: &'a Value,
        name: &'a str,
        trusted: Option<Value>,
    ) -> Pending<'a, Option<Value>> {
        Box::pin(checkout_grant(self.0, sid, run, name, trusted))
    }
    fn broker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        trusted: Option<Value>,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::execute_host_trusted(
            name,
            args,
            sid,
            &self.0.build_socket,
            trusted,
            cancel,
        ))
    }
    fn shutoff(&self) -> Pending<'_, Result<bool>> {
        Box::pin(self.0.shutoff())
    }
    fn policy(&self) -> Pending<'_, Result<Value>> {
        Box::pin(async move {
            Ok(self
                .0
                .store
                .get("policy")
                .await?
                .unwrap_or_else(policy_default))
        })
    }
    fn rule(&self, policy: &Value, name: &str) -> String {
        policy_decision(policy, name).to_owned()
    }
    fn record<'a>(
        &'a self,
        agent: Option<&'a str>,
        docs: Vec<(String, Value)>,
        sid: &'a str,
        kind: &'a str,
        payload: &'a Value,
    ) -> Pending<'a, Result<()>> {
        Box::pin(async move {
            match agent {
                Some(agent) => {
                    let store = self.0.store.for_agent(agent).await?;
                    store.atomic_docs_event(docs, sid, kind, payload).await
                }
                None => {
                    self.0
                        .store
                        .atomic_docs_event(docs, sid, kind, payload)
                        .await
                }
            }
        })
    }
    fn repository<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(crate::api::repository::transport(self.0, sid))
    }
    fn optimization_settings(&self, settings: Option<Value>) -> Result<Value> {
        let settings: crate::optimization_settings::OptimizationSettings = match settings {
            Some(value) => serde_json::from_value(value)?,
            None => Default::default(),
        };
        settings.validate().map_err(anyhow::Error::msg)?;
        Ok(serde_json::to_value(settings)?)
    }
    fn optimization_generation(&self) -> Option<String> {
        crate::optimization::generation()
    }
    fn abilities_version(&self) -> String {
        self.0.abilities.live_version()
    }
    fn stopping(&self) -> CancellationToken {
        self.0.stop.child_token()
    }
}

/// Run a workspace operation on the broker under a renewed lease.
pub async fn workspace_host(
    app: &App,
    sid: &str,
    name: &str,
    args: &Value,
    _trusted: Option<Value>,
    cancel: CancellationToken,
) -> Result<Value> {
    webagent_administration_host::workspace_host(&Host(app), sid, name, args, cancel).await
}

/// Run one Administrator tool call. Source Control calls go to their ability adapter;
/// everything else runs in the Administration module.
#[allow(clippy::too_many_arguments)]
pub async fn execute(
    app: &App,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    pending_id: &str,
    policy_version: u64,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("Tool execution cancelled");
    }
    let agent = session_agent_id(app, sid).await?;
    let run = app.run(run_id).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Current run/session mismatch");
    }
    if crate::source_control::owns(name) {
        return Box::pin(crate::source_control::execute(app, sid, name, args, cancel)).await;
    }
    let call = webagent_administration_host::tools::Call {
        sid,
        run_id,
        run: &run,
        agent: &agent,
        name,
        args,
        pending_id,
        policy_version,
    };
    webagent_administration_host::tools::execute(&Host(app), call, cancel).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_control_tools_are_administrator_tools() {
        for name in crate::source_control::TOOLS {
            assert!(
                supported_schema_name(name),
                "{name} needs an Administrator tool folder"
            );
        }
    }

    /// The worker sees the live checkout only when Codebase Admin can supply the tool to
    /// this run: not off or suppressed, and not denied to the caller. Activation is not
    /// needed. Denied, the same tool stays in the workspace.
    #[tokio::test]
    #[cfg_attr(not(administrator_pack), ignore = "needs the Administrator pack")]
    async fn checkout_grant_needs_codebase_admin_to_supply_the_call() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let revision = json!({"codebase_revision":"r"});
        let loaded = json!({"loaded_abilities":["codebase_admin"]});
        let other = json!({"loaded_abilities":[]});
        let granted = Some(json!({"codebase_revision":"r","checkout":true}));
        // Not a Codebase Admin tool, whatever is loaded.
        assert_eq!(
            checkout_grant(&app, &sid, &loaded, "web_search", Some(revision.clone())).await,
            Some(revision.clone())
        );
        assert_eq!(
            checkout_grant(&app, &sid, &loaded, "read_attachment", None).await,
            None
        );
        // Allowed but neither active nor loaded: a discoverable ability still supplies it.
        assert_eq!(
            checkout_grant(&app, &sid, &other, "shell", Some(revision.clone())).await,
            granted
        );
        // Loaded by this run, or active in the session.
        assert_eq!(
            checkout_grant(&app, &sid, &loaded, "shell", Some(revision.clone())).await,
            granted
        );
        crate::runtime_tests::activate_codebase(&app, &sid).await;
        assert_eq!(
            checkout_grant(&app, &sid, &other, "file_read", None).await,
            Some(json!({"checkout":true}))
        );
        // Denied to the caller: no grant even while active and loaded.
        let mut levels = app.store.capability_levels("shared_default").await.unwrap();
        for id in ["operator", "member", "visitor"] {
            let mut level = levels.levels[id].clone();
            level
                .ability_permissions
                .insert("codebase_admin".into(), access::Permission::Deny);
            levels = app
                .store
                .save_capability_level("shared_default", level, levels.revision)
                .await
                .unwrap();
        }
        assert_eq!(
            checkout_grant(&app, &sid, &loaded, "shell", Some(revision.clone())).await,
            Some(revision)
        );
    }

    #[test]
    #[cfg_attr(not(administrator_pack), ignore = "needs the Administrator pack")]
    fn process_schemas_cover_every_administrator_tool() {
        let names: Vec<_> = schemas()
            .iter()
            .map(|schema| schema["name"].clone())
            .collect();
        let expected: Vec<_> = webagent_administration::tools::TOOLS
            .map(Value::from)
            .to_vec();
        assert_eq!(names, expected);
    }
}
