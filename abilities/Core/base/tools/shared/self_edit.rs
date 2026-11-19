//! Self-edit tools act only on the authenticated run's current agent.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_host_core::{digest, key, StoreCore};

#[path = "../edit_own_prompt/edit_own_prompt.rs"]
mod edit_own_prompt;
#[path = "../remove_own_skill/remove_own_skill.rs"]
mod remove_own_skill;
#[path = "../save_own_skill/save_own_skill.rs"]
mod save_own_skill;
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("edit_own_prompt", edit_own_prompt::PromptArgs),
    webagent_abilities::argument_type!("remove_own_skill", remove_own_skill::RemoveArgs),
    webagent_abilities::argument_type!("save_own_skill", save_own_skill::SaveArgs),
];

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "edit_own_prompt" | "save_own_skill" | "remove_own_skill"
    )
}
struct View {
    store: StoreCore,
    fingerprint: String,
}
async fn view(host: &dyn NativeHost, sid: &str, run_id: &str, name: &str) -> Result<View> {
    if host.actor() != host.store().current_user_id() {
        bail!("Shared chat membership does not authorize caller-owned private records");
    }

    // The user-owned session fixes the agent, and the run fixes the call.
    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = scoped
        .get(&key("run", run_id))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let (mode_revision, visibility, state) = host.scoped_ability_controls(&scoped, sid).await?;
    let mode = host.scoped_session_mode(&scoped, sid).await?;
    let access = host
        .scoped_live_access(&scoped, host.actor(), sid, &agent)
        .await?;
    if !access.admitted()
        || host
            .live_tool_decision(&access, name, &visibility, &state, &run["loaded_abilities"])
            .permission
            == Permission::Deny
    {
        bail!("Self-edit tool unavailable under current access");
    }
    let fingerprint = digest(&json!([
        agent,
        mode_revision,
        visibility,
        state,
        mode,
        run["loaded_abilities"],
        access.context_fingerprint(),
        host.abilities_version()
    ]));
    Ok(View {
        store: scoped,
        fingerprint,
    })
}
async fn work(
    host: &dyn NativeHost,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    pending: &str,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown self-edit tool");
    }
    let current = view(host, sid, run_id, name).await?;
    // Reject an access change before the transactional write; the normal
    // dispatcher also rechecks policy/approval at its safe checkpoint.
    if view(host, sid, run_id, name).await?.fingerprint != current.fingerprint {
        bail!("Self-edit access changed before write");
    }
    // Bind the idempotency receipt to the authenticated actor and run.
    let scope = digest(&json!([
        sid,
        run_id,
        host.actor(),
        current.store.current_agent_id()
    ]));
    match name {
        "edit_own_prompt" => {
            edit_own_prompt::edit(host, &current.store, args, pending, &scope).await
        }
        "save_own_skill" => save_own_skill::save(host, &current.store, args, pending, &scope).await,
        "remove_own_skill" => {
            remove_own_skill::remove(host, &current.store, args, pending, &scope).await
        }
        _ => unreachable!(),
    }
}
/// Run self-edit tool `name` on `call`'s session agent under `call`'s pending ID as the
/// idempotency receipt; a rejection is an error result.
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
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = work(host,call.sid,call.run_id,name,args,call.pending_id) => result,
    };
    // Once storage commits, its receipt makes recovery idempotent. Cancellation
    // after commit cannot undo the edit; the caller must reconcile the receipt.
    Ok(match result {
        Ok(value) => value,
        Err(error) => json!({"status":"error","error_kind":"self_edit_rejected",
            "output":error.to_string()}),
    })
}
#[cfg(test)]
#[path = "../../tests/self_edit.rs"]
mod tests;
