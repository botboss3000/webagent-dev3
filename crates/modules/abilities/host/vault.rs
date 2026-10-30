//! Credential form routes; the credential tools live in `webagent_native_tools::vault`.
use crate::*;
use axum::Extension;
use serde::Deserialize;
use webagent_abilities_host::AbilitiesStore;
use webagent_native_tools::vault::valid_name;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Save {
    request_id: String,
    expected_revision: u64,
    value: String,
}
pub async fn save(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((sid, name)): Path<(String, String)>,
    Json(body): Json<Save>,
) -> Api {
    let actor = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Caller required"))?;
    if !valid_name(&name)
        || body.request_id.len() != 64
        || body.value.is_empty()
        || body.value.len() > 8192
        || body.value.contains('\0')
    {
        return Err(bad("Invalid credential form"));
    }
    Uuid::parse_str(&sid).map_err(|_| bad("Invalid conversation"))?;
    let mut caller = app.clone();
    caller.store = app.store.for_user(actor).await?;
    caller.chat_actor = Some(actor.into());
    caller.chat_access = None;
    let request = caller.store.module_state(&format!("core_ui:{sid}")).await?;
    if request["state"]["actor_id"] != actor {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Credential request unavailable".into(),
        ));
    }
    let rid = request["state"]["run_id"]
        .as_str()
        .ok_or_else(|| bad("Credential request unavailable"))?;
    let run = caller
        .store
        .get(&key("run", rid))
        .await?
        .ok_or_else(|| bad("Credential request unavailable"))?;
    let agent = session_agent_id(&caller, &sid).await?;
    let access = caller.store.live_access(actor, &agent).await?;
    let (policy, _, visibility, state) = ability_controls(&caller, &sid).await?;
    let mode = session_mode(&caller, &sid).await?;
    let decision = live_tool_decision(
        &caller,
        &access,
        "request_credential",
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if !access.admitted() || decision.permission == access::Permission::Deny {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Credential setup access changed".into(),
        ));
    }
    let output = caller
        .store
        .core_save_credential(
            &sid,
            &name,
            &body.request_id,
            body.expected_revision,
            &body.value,
        )
        .await
        .map_err(|_| {
            conflict("Credential request changed or was already used; request a fresh form")
        })?;
    Ok(Json(output))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dismiss {
    request_id: String,
}
/// Dismissing clears the open form for every device; a saved credential is never touched.
pub async fn dismiss(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((sid, name)): Path<(String, String)>,
    Json(body): Json<Dismiss>,
) -> Api {
    let actor = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Caller required"))?;
    if !valid_name(&name) || body.request_id.len() != 64 {
        return Err(bad("Invalid credential form"));
    }
    Uuid::parse_str(&sid).map_err(|_| bad("Invalid conversation"))?;
    let store = app.store.for_user(actor).await?;
    let key = format!("core_ui:{sid}");
    let _guard = app.gate.lock().await;
    let mut saved = store.module_state(&key).await?;
    let state = &saved["state"];
    if state["destination"] != "credential"
        || state["request_id"] != body.request_id.as_str()
        || state["credential_name"] != name.as_str()
        || state["actor_id"] != actor
    {
        return Err(conflict("Credential request already closed"));
    }
    saved["state"] = json!({});
    store
        .module_cas(&key, saved["revision"].as_u64().unwrap_or(0), &saved)
        .await
        .map_err(|_| conflict("Credential request changed; reload the conversation"))?;
    Ok(Json(json!({"dismissed":true})))
}
