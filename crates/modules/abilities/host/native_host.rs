//! Root composition of the native tools crate (`webagent-native-tools`): the
//! [`NativeHost`] its handlers call back into for run authority, session identity,
//! uploads and media lanes.
use crate::{api, shared_chat, tools, App};
use anyhow::{anyhow, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use webagent_access::Relationship;
use webagent_files::uploads::AttachmentLimits;
use webagent_host_core::StoreCore;
use webagent_models::model_selection::ModelRole;
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
}
