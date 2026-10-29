//! Root composition of the Administration module (`webagent-administration-host`): the
//! [`AdministrationHost`] it calls back into, run authority for Administrator calls, and
//! the routing of Source Control, Code Index and UI Admin adapters that stay with their
//! abilities. Model arguments never choose identity, agent, workspace, credentials, host
//! origin or a Docker command.
use super::failure::{boundary, request, Code, Delivery, Failure, Stage};
use crate::{
    ability_controls, access, live_tool_decision, policy_decision, policy_default,
    session_agent_id, session_mode, task_progress, tools, App,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use tokio_util::sync::CancellationToken;
use webagent_administration_host::{AdministrationHost, Pending, SessionAccess};
use webagent_configuration::digest;

/// Whether `name` is an Administrator native tool.
pub(crate) fn supported_schema_name(name: &str) -> bool {
    webagent_administration::tools::supported(name)
}

/// Whether the Administrator handlers execute `name`.
pub fn owns(name: &str) -> bool {
    webagent_administration::tools::owns(name)
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
    let work_finish = matches!(name, "v3_build_work_finish" | "build_work_finish");
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
    // A ceiling under an older public name; moving names never removes it.
    let ceiling = |name: &str, read: bool| {
        let installation = access::Permission::from_policy(policy_decision(&policy, name))
            .unwrap_or(access::Permission::Deny);
        access
            .tool_decision_for_abilities(
                name,
                &["v3_build".into()],
                installation,
                true,
                true,
                read_only,
                read,
            )
            .for_execution_mode(&mode)
            .permission
    };
    let exceeds = |ceiling: access::Permission| {
        ceiling == access::Permission::Deny
            || (ceiling == access::Permission::Ask && work_permission != access::Permission::Ask)
    };
    let legacy = match name {
        "file_list" | "read_directory" => Some("v3_source_list"),
        "file_read" | "file_search" | "read_source" | "search_source" => Some("v3_source_read"),
        "file_edit" | "file_write" | "write_source" | "edit_source" | "patch_source"
        | "delete_source" => Some("v3_source_apply"),
        "build_work" | "build_work_finish" | "v3_build_work_finish" => Some("v3_build_work"),
        _ => None,
    };
    if legacy.is_some_and(|legacy| exceeds(ceiling(legacy, tools::is_read(legacy)))) {
        return Err(refused(Code::LegacyAuthorizationRejected));
    }
    if work_finish && exceeds(ceiling("v3_worker_submit", false)) {
        return Err(refused(Code::DeploymentAuthorizationRejected));
    }
    // An Ask finish reaches this host only after explicit approval. Its queued job keeps
    // that authorization while the scoped policy fingerprint stays valid. A finish can
    // replace this loop's ability bundle while its broker job is still live: policy,
    // activation and access stay in the fingerprint, but bundle bytes alone cannot
    // revoke the job during its own deployment.
    let bundle = if work_finish {
        Value::Null
    } else {
        json!(scoped.abilities.live_version())
    };
    let generation = if performance {
        crate::optimization::generation()
    } else {
        None
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
        run["loaded_abilities"],
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
    fn progress(&self, run: &Value) -> Value {
        task_progress::project(run)
    }
    fn abilities_version(&self) -> String {
        self.0.abilities.live_version()
    }
    fn promotion_socket(&self) -> &Path {
        &self.0.promotion_socket
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

/// Run one Administrator tool call. Source Control and Code Index calls go to their
/// ability adapters; everything else runs in the Administration module.
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
        return Box::pin(crate::source_control::execute(
            app, sid, &run, name, args, pending_id, cancel,
        ))
        .await;
    }
    match name {
        "code_index_maintenance" => {
            super::code_index_maintenance::execute(app, sid, run_id, args, cancel).await
        }
        "code_index_lookup" => {
            let trusted = None;
            let mut result =
                tools::execute_trusted(name, args, sid, &app.tool_socket, trusted, cancel.clone())
                    .await?;
            super::code_index_configuration::attach(app, args, &mut result, &cancel).await?;
            Ok(result)
        }
        _ => {
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
    }
}

/// Narrow UI Admin aliases use their own tool decision plus unchanged protected source
/// ceilings.
pub(crate) async fn ui_host(
    app: &App,
    sid: &str,
    rid: &str,
    tool: &str,
    operation: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !super::core_ui_admin::owns(tool)
        || !matches!(
            operation,
            "file_list"
                | "file_read"
                | "file_search"
                | "file_write"
                | "file_edit"
                | "v3_source_apply"
        )
    {
        bail!("Invalid UI broker operation");
    }
    let fingerprint = host_fingerprint(app, sid, rid, tool).await?;
    let trusted = webagent_administration_host::trusted(
        crate::shared_chat::actor(app),
        sid,
        &json!(fingerprint),
    );
    tools::execute_host_trusted(
        operation,
        args,
        sid,
        &build_socket(app),
        Some(trusted),
        cancel,
    )
    .await
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

    #[test]
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
