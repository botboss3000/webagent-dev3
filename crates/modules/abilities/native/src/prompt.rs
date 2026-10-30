//! Read current-agent prompt configuration through the authenticated session.
//! Read grants reveal configuration; they never grant administration or editing.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::{storage::Repository, Permission};
use webagent_host_core::{digest, key, StoreCore};
const MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "read_own_prompt"
}
/// The model-facing schema of `read_own_prompt`.
#[must_use]
pub fn schemas() -> Vec<Value> {
    vec![json!({"type":"function","name":"read_own_prompt",
        "description":"Read the current agent's ordered prompt slots and revisions; internal slots are omitted",
        "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}})]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptArgs {}
#[derive(Debug)]
struct ConfigurationError(&'static str);
impl std::fmt::Display for ConfigurationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for ConfigurationError {}
struct View {
    store: StoreCore,
    agent: String,
    manager: bool,
    fingerprint: String,
}
/// The access authority of `user` over `agent` on agent store `view`'s plane, as the root
/// store reads it.
async fn agent_authority(view: &StoreCore, user: &str, agent: &str) -> Result<Value> {
    Repository::new(
        &view.root,
        &view.app_pool,
        &view.agent_pool,
        &view.agent_id,
        &view.user_id,
        view,
    )
    .agent_authority(user, agent)
    .await
}
async fn view(host: &dyn NativeHost, sid: &str, run_id: &str) -> Result<View> {
    // The caller-owned session determines the agent. Never trust an app view
    // inherited from a previous API call or accept a model-supplied target.
    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = scoped
        .get(&key("run", run_id))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let loaded = run["loaded_abilities"].clone();
    let (policy, mode_revision, visibility, state) =
        host.scoped_ability_controls(&scoped, sid).await?;
    let mode = host.scoped_session_mode(&scoped, sid).await?;
    let access = host
        .scoped_live_access(&scoped, host.actor(), sid, &agent)
        .await?;
    if !access.admitted()
        || host
            .live_tool_decision(
                &access,
                "read_own_prompt",
                &policy,
                &visibility,
                &state,
                &loaded,
                mode["mode"] == "read_only",
            )
            .permission
            == Permission::Deny
    {
        bail!("Current-agent prompt read unavailable under current access");
    }
    let authority = agent_authority(&scoped, host.actor(), &agent).await?;
    let manager = authority["can_manage"].as_bool().unwrap_or(false);
    let fingerprint = digest(&json!([
        agent,
        policy,
        mode_revision,
        visibility,
        state,
        mode,
        loaded,
        access.context_fingerprint(),
        authority,
        host.abilities_version()
    ]));
    Ok(View {
        store: scoped,
        agent,
        manager,
        fingerprint,
    })
}
fn render_slots(agent: &str, slots: Vec<Value>, manager: bool) -> Result<Value> {
    let mut projected = Vec::new();
    for slot in slots {
        let name = slot["slot_name"]
            .as_str()
            .ok_or(ConfigurationError("Prompt slot name unavailable"))?;
        if name.starts_with("__") {
            continue;
        }
        let locked = slot["lock"]
            .as_bool()
            .ok_or(ConfigurationError("Prompt slot lock unavailable"))?;
        let revision = slot["revision"]
            .as_u64()
            .filter(|r| *r > 0)
            .ok_or(ConfigurationError("Prompt slot revision unavailable"))?;
        let order = slot["order_index"]
            .as_u64()
            .ok_or(ConfigurationError("Prompt slot order unavailable"))?;
        let content = slot["content"]
            .as_str()
            .ok_or(ConfigurationError("Prompt slot content unavailable"))?;
        projected.push(json!({"slot_name":name,"order":order,"locked":locked,
            "editable":manager&&!locked,"revision":revision,"content":content}));
    }
    let result = json!({"status":"success","output":{"agent_id":agent,"slots":projected,
        "count":projected.len(),"source_kind":"agent_prompt_configuration","read_only":true}});
    if serde_json::to_vec(&result)?.len() > MAX_OUTPUT_BYTES {
        return Err(ConfigurationError("Prompt configuration exceeds the 64 KiB read limit; use bounded administrator slot reads").into());
    }
    Ok(result)
}
async fn work(
    host: &dyn NativeHost,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown prompt read tool");
    }
    let _: PromptArgs = serde_json::from_value(args.clone())
        .map_err(|_| anyhow::anyhow!("read_own_prompt accepts only an empty object"))?;
    let current = view(host, sid, run_id).await?;
    let slots = host.prompt_slots(&current.store).await?;
    let output = render_slots(&current.agent, slots, current.manager)?;
    if view(host, sid, run_id).await?.fingerprint != current.fingerprint {
        bail!("Prompt read access changed during read");
    }
    Ok(output)
}
/// Run `read_own_prompt` for `call`'s session agent; a rejection is an error result.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let result = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=work(host,call.sid,call.run_id,name,args)=>result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match result {
        Ok(output) => output,
        Err(error) => json!({"status":"error","error_kind":
            if error.downcast_ref::<ConfigurationError>().is_some() {"prompt_configuration_invalid"}
            else {"prompt_read_rejected"},"output":error.to_string()}),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn slot(name: &str, content: &str, locked: bool, order: u64) -> Value {
        json!({"slot_name":name,"content":content,"lock":locked,"order_index":order,"revision":3})
    }
    #[test]
    fn prompt_projection_hides_internal_slots_preserves_revisions_and_limits_editability() {
        let slots = vec![
            slot("identity", "locked content", true, 0),
            slot("__skills__", "INTERNAL SECRET", false, 1),
            slot("style", "short", false, 10),
        ];
        let owner = render_slots("agent-a", slots.clone(), true).unwrap();
        assert_eq!(owner["output"]["count"], 2);
        assert_eq!(owner["output"]["slots"][0]["revision"], 3);
        assert_eq!(owner["output"]["slots"][0]["locked"], true);
        assert_eq!(owner["output"]["slots"][0]["editable"], false);
        assert_eq!(owner["output"]["slots"][1]["editable"], true);
        assert_eq!(owner["output"]["slots"][1]["order"], 10);
        assert!(!owner.to_string().contains("INTERNAL SECRET"));
        let member = render_slots("agent-a", slots, false).unwrap();
        assert!(member["output"]["slots"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["editable"] == false));
        assert_eq!(member["output"]["read_only"], true);
    }
    #[test]
    fn output_bound_is_serialized_bytes_and_never_truncates_slot_content() {
        let oversized = (0..4)
            .map(|i| slot(&format!("large{i}"), &"\n".repeat(8192), false, i))
            .collect();
        let error = render_slots("agent-a", oversized, true).unwrap_err();
        assert!(error.downcast_ref::<ConfigurationError>().is_some());
        assert!(error.to_string().contains("64 KiB"));
        assert!(serde_json::from_value::<PromptArgs>(json!({"agent_id":"foreign"})).is_err());
        assert!(serde_json::from_value::<PromptArgs>(json!({"user_id":"foreign"})).is_err());
        assert!(serde_json::from_value::<PromptArgs>(Value::Null).is_err());
        assert!(serde_json::from_value::<PromptArgs>(json!({})).is_ok());
    }
}
