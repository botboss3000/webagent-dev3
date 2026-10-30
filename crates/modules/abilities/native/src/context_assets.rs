//! Native list/unload of the authenticated requester's conversation selections.
//! Unload returns a pure preview; append_tool and fenced Store save own the effect.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_assets_contracts::{valid_id, validate_selector, Operation, Plan, Request, State};
use webagent_host_core::key;

/// Tools this module executes; their schemas are built in below.
pub const TOOLS: &[&str] = &["context_assets"];
/// Register this fixed operation; non-strict wire schemas preserve list/unload optional fields.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "context_assets"
}

/// Publish content-free scope controls; selectors are validated again by the fixed contract.
#[must_use]
pub fn schemas() -> Vec<Value> {
    vec![
        json!({"type":"function","name":"context_assets","strict":false,
  "description":"List or unload this requester's dynamically selected conversation tools and skills. Changes selection only; never changes permissions, files or configured visible guidance.",
  "parameters":{"type":"object","properties":{
   "action":{"type":"string","enum":["list","unload"]},
   "selector":{"anyOf":[
    {"type":"object","properties":{"type":{"type":"string","enum":["ability"]},"ability_id":{"type":"string","minLength":1}},
     "required":["type","ability_id"],"additionalProperties":false},
    {"type":"object","properties":{"type":{"type":"string","enum":["tool"]},"tool_name":{"type":"string","minLength":1}},
     "required":["type","tool_name"],"additionalProperties":false},
    {"type":"object","properties":{"type":{"type":"string","enum":["skill"]},"ability_id":{"type":"string","minLength":1},
     "kind":{"type":"string","enum":["ability","tool","selectable","own"]},"skill_id":{"type":"string","minLength":1}},
     "required":["type","ability_id","kind","skill_id"],"additionalProperties":false}]},
   "expected_revision":{"type":"integer","minimum":0},
   "request_id":{"type":"string","minLength":1,"maxLength":128}},
   "required":["action"],"additionalProperties":false}}),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    action: String,
    #[serde(default)]
    selector: Option<Value>,
    #[serde(default)]
    expected_revision: Option<u64>,
    #[serde(default)]
    request_id: Option<String>,
}
struct View {
    run: Value,
    agent: String,
    requester: String,
    state: State,
    fingerprint: String,
}

/// Resolve actor, canonical session, selected agent and live tool policy before exposing even IDs.
async fn view(host: &dyn NativeHost, sid: &str, run_id: &str) -> Result<View> {
    let actor = host.actor().to_owned();
    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = scoped
        .get(&key("run", run_id))
        .await?
        .ok_or_else(|| anyhow::anyhow!("Run not found"))?;
    if run["session_id"] != sid
        || run["agent_id"].as_str().unwrap_or("shared_default") != agent
        || run["requester_user_id"]
            .as_str()
            .unwrap_or(scoped.current_user_id())
            != actor
    {
        bail!("Context assets run scope mismatch");
    }
    let (policy, _, visibility, state) = host.scoped_ability_controls(&scoped, sid).await?;
    let mode = host.scoped_session_mode(&scoped, sid).await?;
    let access = host
        .scoped_live_access(&scoped, &actor, sid, &agent)
        .await?;
    if !access.admitted()
        || host
            .live_tool_decision(
                &access,
                "context_assets",
                &policy,
                &visibility,
                &state,
                &run["loaded_abilities"],
                mode["mode"] == "read_only" || run["post_run_followup"] == true,
            )
            .permission
            == Permission::Deny
    {
        bail!("Context assets tool unavailable");
    }
    let state = host.context_assets(&scoped, sid, &agent, &actor).await?;
    let fingerprint = host
        .context_fingerprint(
            &scoped,
            sid,
            &agent,
            &run["loaded_abilities"],
            &run["selected_tools"],
            &run["loaded_skills"],
            &run["optimization_request"],
            &actor,
        )
        .await?;
    Ok(View {
        run,
        agent,
        requester: actor,
        state,
        fingerprint,
    })
}

/// Recheck live visibility and ledger state after component IO; previews never authorize a commit.
async fn work(
    host: &dyn NativeHost,
    sid: &str,
    run_id: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    let query: Args = serde_json::from_value(args.clone())?;
    let current = view(host, sid, run_id).await?;
    let output = match query.action.as_str() {
        "list" => {
            if query.selector.is_some()
                || query.expected_revision.is_some()
                || query.request_id.is_some()
            {
                bail!("List accepts no mutation fields");
            }
            json!({"scope":"conversation","revision":current.state.revision,"assets":current.state.selectors,"permissions_changed":false})
        }
        "unload" => {
            let selector = query
                .selector
                .ok_or_else(|| anyhow::anyhow!("Unload selector required"))?;
            validate_selector(&selector)?;
            let expected_revision = query
                .expected_revision
                .ok_or_else(|| anyhow::anyhow!("Unload revision required"))?;
            let request_id = query
                .request_id
                .filter(|id| id.len() <= 128 && valid_id(id))
                .ok_or_else(|| anyhow::anyhow!("Unload request ID required"))?;
            let operation = Operation::Unload {
                selector: selector.clone(),
                expected_revision,
                request_id: request_id.clone(),
            };
            let request = Request::Transition {
                version: 1,
                state: current.state.clone(),
                operation: operation.clone(),
            };
            let plan = host.assets_plan(&request, cancel).await?;
            let Plan::Transition {
                removed,
                result_revision,
                ..
            } = plan
            else {
                bail!("Context assets preview kind");
            };
            json!({"scope":"conversation","revision":result_revision,"removed":removed,"selector":selector,
             "request_id":request_id,"expected_revision":expected_revision,"operation":operation,"permissions_changed":false})
        }
        _ => bail!("Unknown context assets action"),
    };
    let after = view(host, sid, run_id).await?;
    if after.fingerprint != current.fingerprint
        || after.state != current.state
        || after.agent != current.agent
        || after.requester != current.requester
        || after.run["generation"] != current.run["generation"]
    {
        bail!("Context assets access changed during read");
    }
    Ok(json!({"status":"success","output":output}))
}

/// Cancellation interrupts only pure preparation; no selection effect exists until the caller saves.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown context assets operation");
    }
    tokio::select! { _=cancel.cancelled()=>bail!("Context assets cancelled"), result=work(host,call.sid,call.run_id,args,&cancel)=>result }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// The wire contract cannot accept model-selected user/session authority.
    #[test]
    fn rejects_forged_scope_fields() {
        assert!(
            serde_json::from_value::<Args>(json!({"action":"list","user_id":"other"})).is_err()
        );
        assert!(
            serde_json::from_value::<Args>(json!({"action":"unload","expected_revision":-1}))
                .is_err()
        );
        assert_eq!(schemas()[0]["name"], "context_assets");
    }
}
