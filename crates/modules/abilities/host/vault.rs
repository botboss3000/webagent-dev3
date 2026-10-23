//! Human-entered generic credentials remain encrypted and are never returned to mini apps or models.
use crate::*;
use axum::Extension;
use serde::Deserialize;
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "check_credential" | "request_credential" | "list_vault_keys"
    )
}
pub fn is_read(name: &str) -> bool {
    matches!(name, "check_credential" | "list_vault_keys")
}
pub fn schemas() -> Vec<Value> {
    [
 ("check_credential","Check presence, revision and service origin for one caller-private credential. Returns metadata only; does not validate a remote provider or reveal the secret.",json!({"name":{"type":"string","maxLength":64}}),vec!["name"]),
 ("request_credential","Request a private human credential form in the active conversation. Supply a plain key name and HTTPS service origin. The user enters the value directly; models and mini apps receive only metadata. Does not grant network access or silently use this credential.",json!({"name":{"type":"string","maxLength":64},"origin":{"type":"string","maxLength":500}}),vec!["name","origin"]),
 ("list_vault_keys","List caller-private credential names, origins and revisions without values.",json!({}),vec![])
 ].into_iter().map(|(name,description,properties,required)|json!({"type":"function","name":name,"description":description,"parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}})).collect()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    name: Option<String>,
    origin: Option<String>,
}
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
pub fn origin(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)?;
    if raw.len() > 500
        || url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "Use a plain HTTPS service origin without credentials, path, query or fragment"
        ));
    }
    Ok(url.origin().ascii_serialization())
}
fn public(name: &str, record: &Value) -> Value {
    json!({"name":name,"configured":record["value"].as_str().is_some_and(|s|!s.is_empty()),"revision":record["revision"].as_u64().unwrap_or(0),"origin":record["origin"]})
}
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    tool: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = super::core_authority::check(app, sid, rid, generation, pending, tool, args, &cancel)
        .await?;
    if shared_chat::actor(app) != app.store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private credentials"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    if tool == "list_vault_keys" {
        let keys = app
            .store
            .list("core_vault:")
            .await?
            .iter()
            .filter_map(|v| v["name"].as_str().map(|name| public(name, v)))
            .take(100)
            .collect::<Vec<_>>();
        return Ok(json!({"status":"success","output":{"keys":keys}}));
    }
    let name = q
        .name
        .as_deref()
        .filter(|s| valid_name(s))
        .ok_or_else(|| anyhow!("Plain credential name required"))?;
    let prior = app
        .store
        .get(&format!("core_vault:{name}"))
        .await?
        .unwrap_or(Value::Null);
    if tool == "check_credential" {
        return Ok(json!({"status":"success","output":public(name,&prior)}));
    }
    if tool != "request_credential" {
        return Err(anyhow!("Unknown credential tool"));
    }
    let origin = origin(
        q.origin
            .as_deref()
            .ok_or_else(|| anyhow!("HTTPS service origin required"))?,
    )?;
    let _guard = app.gate.lock().await;
    super::core_authority::check(app, sid, rid, generation, pending, tool, args, &cancel).await?;
    let mut saved = app.store.module_state(&format!("core_ui:{sid}")).await?;
    let request_id = digest(&json!([sid, rid, pending]));
    if saved["state"]["request_id"] != request_id {
        saved["state"] = json!({"request_id":request_id,"destination":"credential","actor_id":shared_chat::actor(app),"agent_id":run["agent_id"],"session_id":sid,"created_at":now(),"expires_at":now()+600,"credential_name":name,"credential_origin":origin,"expected_revision":prior["revision"].as_u64().unwrap_or(0),"run_id":rid});
        app.store
            .module_cas(
                &format!("core_ui:{sid}"),
                saved["revision"].as_u64().unwrap_or(0),
                &saved,
            )
            .await?;
    }
    Ok(
        json!({"status":"success","output":{"requested":true,"name":name,"origin":origin,"delivery":"private_human_form","secret_returned":false}}),
    )
}
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
            conflict(
                "Credential request changed, expired or was already used; request a fresh form",
            )
        })?;
    Ok(Json(output))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn service_origins_do_not_smuggle_paths_credentials_or_javascript() {
        assert_eq!(
            origin("https://example.test").unwrap(),
            "https://example.test"
        );
        for value in [
            "javascript:alert(1)",
            "http://example.test",
            concat!("https://", "user", ":", "pass", "@example.test"),
            "https://example.test/path",
            "https://example.test?x=1",
        ] {
            assert!(origin(value).is_err());
        }
    }
}
