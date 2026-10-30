//! Root composition of the native tools crate (`webagent-native-tools`): the
//! [`NativeHost`] its handlers call back into for run authority, session identity,
//! uploads and media lanes.
use crate::{api, shared_chat, tools, App};
use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use webagent_access::{authority::LiveAccess, Relationship, ToolDecision};
use webagent_files::uploads::AttachmentLimits;
use webagent_host_core::StoreCore;
use webagent_models::account_catalog::PlatformPin;
use webagent_models::model_selection::{Catalog, ModelRole, SelectionPlan};
use webagent_native_tools::{Call, NativeHost, Pending};

pub(crate) struct Host<'a>(pub(crate) &'a App);

impl NativeHost for Host<'_> {
    fn store(&self) -> &StoreCore {
        &self.0.store
    }
    fn agent_store<'a>(&'a self, agent: &'a str) -> Pending<'a, Result<StoreCore>> {
        Box::pin(async move { Ok((*self.0.store.for_agent(agent).await?).clone()) })
    }
    fn user_store<'a>(&'a self, user: &'a str) -> Pending<'a, Result<StoreCore>> {
        Box::pin(async move { Ok((*self.0.store.for_user(user).await?).clone()) })
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.0.gate
    }
    fn actor(&self) -> &str {
        shared_chat::actor(self.0)
    }
    fn authorize<'a>(
        &'a self,
        call: Call<'a>,
        name: &'a str,
        args: &'a Value,
        cancel: &'a CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(async move {
            tools::core_authority::check(
                self.0,
                call.sid,
                call.run_id,
                call.generation,
                call.pending_id,
                name,
                args,
                cancel,
            )
            .await
        })
    }
    fn relationship<'a>(
        &'a self,
        sid: &'a str,
        agent: &'a str,
    ) -> Pending<'a, Result<Relationship>> {
        Box::pin(async move {
            let actor = shared_chat::actor(self.0);
            Ok(shared_chat::live_access(&self.0.store, actor, sid, agent)
                .await?
                .relationship)
        })
    }
    fn worker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        sid: &'a str,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::execute_trusted(
            name,
            args,
            sid,
            &self.0.tool_socket,
            None,
            cancel,
        ))
    }
    fn write_artifact<'a>(
        &'a self,
        call: Call<'a>,
        kind: &'a str,
        id: &'a str,
        expected_revision: u64,
        artifact: &'a Value,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(self.0.store.base_write_artifact(
            call.sid,
            call.run_id,
            call.pending_id,
            call.generation,
            kind,
            id,
            expected_revision,
            artifact,
        ))
    }
    fn attachment_limits<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<AttachmentLimits>> {
        Box::pin(async move {
            api::uploads::limits(self.0, sid)
                .await
                .map_err(|e| anyhow!(e.1))
        })
    }
    fn ui_admin<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        tool: &'a str,
        operation: &'a str,
        args: &'a Value,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::administrator::ui_host(
            self.0, sid, run_id, tool, operation, args, cancel,
        ))
    }
    fn upload<'a>(&'a self, sid: &'a str, body: Value) -> Pending<'a, Result<Value>> {
        Box::pin(async move {
            let upload = api::uploads::upload(
                crate::State(self.0.clone()),
                axum::extract::Path(sid.to_owned()),
                crate::Json(body),
            )
            .await
            .map_err(|e| anyhow!(e.1))?;
            Ok(upload.0)
        })
    }
    fn attachment<'a>(&'a self, sid: &'a str, id: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(async move {
            api::uploads::lookup(self.0, sid, id)
                .await
                .map_err(|e| anyhow!(e.1))
        })
    }
    fn media_lane<'a>(&'a self, sid: &'a str, role: ModelRole) -> Pending<'a, Result<Value>> {
        Box::pin(async move {
            api::media_lane::lane_run(self.0, sid, role)
                .await
                .map_err(|e| anyhow!(e.1))
        })
    }
    fn media_answer<'a>(
        &'a self,
        lane: &'a Value,
        input: Vec<Value>,
        instructions: &'a str,
    ) -> Pending<'a, Result<String>> {
        Box::pin(async move {
            api::media_lane::lane_answer(self.0, lane, input, instructions)
                .await
                .map_err(|e| anyhow!(e.1))
        })
    }
    fn session_agent_id<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<String>> {
        Box::pin(crate::session_agent_id(self.0, sid))
    }
    fn live_access<'a>(&'a self, user: &'a str, agent: &'a str) -> Pending<'a, Result<LiveAccess>> {
        Box::pin(self.0.store.live_access(user, agent))
    }
    fn model_catalog(&self) -> Pending<'_, Result<Catalog>> {
        Box::pin(self.0.store.model_catalog())
    }
    fn pin_model_plan<'a>(&'a self, plan: &'a mut SelectionPlan) -> Pending<'a, Result<()>> {
        Box::pin(self.0.pin_model_plan(plan))
    }
    fn dispatch_tokens<'a>(
        &'a self,
        user: &'a str,
        agent: &'a str,
        pin: &'a PlatformPin,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(self.0.store.platform_dispatch_tokens(user, agent, pin))
    }
    fn dependency<'a>(
        &'a self,
        sid: &'a str,
        run: &'a Value,
        tool: &'a str,
    ) -> Pending<'a, Result<()>> {
        Box::pin(tools::core_authority::dependency(self.0, sid, run, tool))
    }
    fn agent_request<'a>(
        &'a self,
        method: &'a str,
        path: String,
        body: Value,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::core_agent_management::request(
            self.0, method, path, body,
        ))
    }
    fn shared_chat_policy<'a>(
        &'a self,
        agent: &'a str,
    ) -> Pending<'a, Result<shared_chat::SharedChatPolicy>> {
        Box::pin(shared_chat::policy(&self.0.store, agent))
    }
    fn prompt_slot<'a>(
        &'a self,
        user: &'a str,
        agent: &'a str,
        slot: &'a str,
    ) -> Pending<'a, Result<Option<Value>>> {
        Box::pin(async move {
            self.0
                .store
                .for_user(user)
                .await?
                .for_agent(agent)
                .await?
                .get_prompt_slot(slot)
                .await
        })
    }
    fn module_tool_call<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        tool: &'a str,
        args: &'a Value,
        pending_id: &'a str,
        cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(crate::modules::tool_call(
            self.0, sid, run_id, tool, args, pending_id, cancel,
        ))
    }
    fn session_access<'a>(
        &'a self,
        sid: &'a str,
        agent: &'a str,
    ) -> Pending<'a, Result<LiveAccess>> {
        Box::pin(shared_chat::live_access(
            &self.0.store,
            shared_chat::actor(self.0),
            sid,
            agent,
        ))
    }
    fn ability_controls<'a>(
        &'a self,
        sid: &'a str,
    ) -> Pending<'a, Result<(Value, u64, Value, Value)>> {
        Box::pin(crate::ability_controls(self.0, sid))
    }
    fn session_mode<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(crate::session_mode(self.0, sid))
    }
    fn live_tool_decision(
        &self,
        access: &LiveAccess,
        name: &str,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        read_only: bool,
    ) -> ToolDecision {
        crate::live_tool_decision(
            self.0, access, name, policy, visibility, state, loaded, read_only,
        )
    }
    fn default_agent_model(&self) -> &'static str {
        crate::DEFAULT_AGENT_MODEL
    }
    fn active_runs(&self) -> Pending<'_, Result<Vec<Value>>> {
        Box::pin(self.0.store.active_runs())
    }
    fn followup_queue<'a>(&'a self, sid: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(self.0.store.followup_queue(sid))
    }
    fn terminal(&self, status: &str) -> bool {
        webagent_engine::runtime::failure::terminal(status)
    }
    fn selected_state(&self, visibility: &Value, state: &Value, loaded: &Value) -> Value {
        self.0.abilities.selected_state(visibility, state, loaded)
    }
    fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value> {
        self.0.abilities.catalog_with(policy, visibility, state)
    }
    fn all_schemas(&self) -> Vec<Value> {
        self.0.abilities.all_schemas()
    }
    fn tool_gate(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> (bool, bool) {
        self.0.abilities.tool_gate(name, visibility, state, loaded)
    }
    fn change_session_mode<'a>(
        &'a self,
        agent: &'a str,
        call: Call<'a>,
        mode: &'a str,
        expected_revision: u64,
    ) -> Pending<'a, Result<Result<Value>>> {
        Box::pin(async move {
            let mut scoped = self.0.clone();
            scoped.store = self.0.store.for_agent(agent).await?;
            let tool = api::ModeCall {
                run_id: call.run_id,
                generation: call.generation,
                pending_id: call.pending_id,
            };
            Ok(api::change_session_mode(
                &scoped,
                call.sid,
                mode,
                Some(expected_revision),
                Some(tool),
            )
            .await
            .map(|response| response.0)
            .map_err(|e| anyhow!(e.1)))
        })
    }
    fn rating_asset<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        args: &'a Value,
    ) -> Pending<'a, Result<Value>> {
        Box::pin(tools::core_skills::rating_asset(self.0, sid, run_id, args))
    }
}
