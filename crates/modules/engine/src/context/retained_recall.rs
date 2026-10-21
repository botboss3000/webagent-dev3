use crate::runtime::{Runtime,config,ports::*,models::ModelRecords};
use crate::ports::*;
use crate::context::abilities::Catalog;
use crate::tools::execution::ActionCatalog;
use crate::tool_output;
use anyhow::Result;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use webagent_access as access;
impl<S:ScopedStorage+ModelRecords+ToolJournal+ChatEffects+AdmissionEffects+NotificationEffects+UsageEffects+RunObservation+RunPresentation,C:config::SchemaCatalog,B:Catalog,A:crate::runtime::actions::Actions<S>+Clone+PartialEq,H:ChatAuthority<S>,M:ModuleBindings<S>,X:AgentPolicy<S>+NativePolicy> Runtime<S,C,B,A,H,M,X> {
//! Restore promoted source evidence from the immutable journal after compaction.
//! This is a caller-scoped read with fresh origin-tool permission, never a write replay.

pub async fn restore_retained(
    &self,
    sid: &str,
    agent: &str,
    run: &mut Value,
    policy: &tool_output::Policy,
) -> Result<bool> {let app=self;
    if run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id())!=app.store.current_user_id() {return Ok(false);}
    if !policy.enabled {
        return Ok(false);
    }
    let entries = run["tool_context_state"]["entries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let present = run["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["call_id"].as_str().map(str::to_owned))
        .collect::<std::collections::HashSet<_>>();
    let mut candidates = entries
        .into_iter()
        .filter(|e| e["promoted"] == true)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|e| std::cmp::Reverse(e["last_used"].as_u64().unwrap_or(0)));
    let (permission, _, visibility, state) = app.ability_controls(sid).await?;
    let mode = app.session_mode(sid).await?;
    let access=app.chat.live_access(app.store.records(),run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id()),sid,agent).await?;
    if !access.admitted() {
        return Ok(false);
    }
    let mut chars = 0usize;
    let mut changed = false;
    for e in candidates {
        let Some(id) = e["call_id"].as_str() else {
            continue;
        };
        if present.contains(id) {
            continue;
        }
        let Some(origin) = e["origin_run_id"].as_str() else {
            continue;
        };
        let Some(name) = e["tool_name"].as_str() else {
            continue;
        };
        if !policy.eligible(name)
            || app.live_tool_decision(
                &access,
                name,
                &permission,
                &visibility,
                &state,
                &run["loaded_abilities"],
                mode["mode"] == "read_only",
            )
            .for_execution_mode(&mode)
            .permission
                != access::Permission::Auto
        {
            continue;
        }
        let source = match app.run(origin).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        if source["session_id"] != sid
            || source["agent_id"].as_str().unwrap_or("shared_default") != agent
        {
            continue;
        }
        let Some(call) = app.store.tool_call_record(sid, origin, id).await? else {
            continue;
        };
        if call["name"] != name {
            continue;
        }
        let Some(output) = app.store.tool_result(sid, origin, id).await? else {
            continue;
        };
        let Some(raw) = output["output"].as_str() else {
            continue;
        };
        if hex::encode(Sha256::digest(raw.as_bytes())) != e["output_sha256"] {
            continue;
        }
        let size = raw.chars().count();
        if chars.saturating_add(size) > policy.promoted_chars {
            continue;
        }
        chars += size;
        run["input"].as_array_mut().unwrap().extend([call, output]);
        changed = true;
    }
    Ok(changed)
}

}
