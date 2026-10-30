//! Private bounded webhook inbox routes. Bearer setup stays in the authenticated human page;
//! the webhook tools are `webagent_native_tools::webhooks`.
use crate::*;
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    response::Html,
    Extension,
};
use webagent_abilities_host::AbilitiesStore;
/// The tool logic lives in the native tools crate; the root keeps the HTTP routes.
fn unavailable() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Receiver unavailable".into())
}
fn decode_token(store: &Store, id: &str, headers: &HeaderMap) -> Result<(Value, String), ApiError> {
    Uuid::parse_str(id).map_err(|_| unavailable())?;
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|v| v.len() <= 1024)
        .ok_or_else(unavailable)?;
    webagent_native_tools::webhooks::verify_token(store, id, token).ok_or_else(unavailable)
}
/// This one bearer-only callback ignores cookies; it has no browser-session or tool authority.
pub async fn receive(
    State(mut app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let (binding, token_digest) = decode_token(&app.store, &id, &headers)?;
    if body.len() > 16384
        || headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_none_or(|v| v.split(';').next() != Some("application/json"))
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Bounded JSON body required".into(),
        ));
    }
    let event_id = headers
        .get("x-webhook-event-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| {
            !v.is_empty()
                && v.len() <= 128
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
        })
        .ok_or_else(|| {
            ApiError(
                StatusCode::BAD_REQUEST,
                "X-Webhook-Event-ID required".into(),
            )
        })?;
    let payload: Value = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "Invalid JSON body".into()))?;
    let owner = binding["owner_id"].as_str().ok_or_else(unavailable)?;
    let account = app
        .store
        .account_by_id(owner)
        .await?
        .ok_or_else(unavailable)?;
    if account["generation"] != binding["generation"]
        || !access::Principal::from_account(&account)
            .map_err(|_| unavailable())?
            .active()
    {
        return Err(unavailable());
    }
    app.store = app.store.for_user(owner).await.map_err(|_| unavailable())?;
    app.chat_actor = Some(owner.to_owned());
    app.chat_access = None;
    let record = app
        .store
        .get(&format!("core_webhook:{id}"))
        .await?
        .ok_or_else(unavailable)?;
    let agent = record["artifact"]["agent_id"]
        .as_str()
        .ok_or_else(unavailable)?;
    let access = app
        .store
        .live_access(owner, agent)
        .await
        .map_err(|_| unavailable())?;
    let sid = record["session_id"].as_str().ok_or_else(unavailable)?;
    let run = app
        .store
        .get(&key(
            "run",
            record["updated_by_run"].as_str().ok_or_else(unavailable)?,
        ))
        .await?
        .ok_or_else(unavailable)?;
    let (policy, _, visibility, state) = ability_controls(&app, sid)
        .await
        .map_err(|_| unavailable())?;
    let mode = session_mode(&app, sid).await.map_err(|_| unavailable())?;
    let decision = live_tool_decision(
        &app,
        &access,
        "register_webhook",
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if !access.admitted() || decision.permission == access::Permission::Deny {
        return Err(unavailable());
    }
    let result = app
        .store
        .core_webhook_ingest(
            &id,
            record["revision"].as_u64().ok_or_else(unavailable)?,
            &token_digest,
            event_id,
            &payload,
        )
        .await
        .map_err(|_| {
            ApiError(
                StatusCode::CONFLICT,
                "Receiver rejected event; check expiry, revision, event identity and quota".into(),
            )
        })?;
    wake::notify(&app).await;
    Ok((StatusCode::ACCEPTED, Json(result)))
}
fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
/// Authenticated human setup is the sole credential projection; browser diagnostics masks data-secret.
pub async fn setup(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Result<Html<String>, ApiError> {
    Uuid::parse_str(&id).map_err(|_| unavailable())?;
    let owner = identity["_user_id"].as_str().ok_or_else(unavailable)?;
    let store = app.store.for_user(owner).await.map_err(|_| unavailable())?;
    let record = store
        .get(&format!("core_webhook:{id}"))
        .await?
        .ok_or_else(unavailable)?;
    if record["artifact"]["deleted"] == true
        || record["artifact"]["expires_at"].as_u64().unwrap_or(0) <= now()
    {
        return Err(unavailable());
    }
    let label = html_escape(record["artifact"]["name"].as_str().unwrap_or("Webhook"));
    let token = html_escape(
        record["artifact"]["token"]
            .as_str()
            .ok_or_else(unavailable)?,
    );
    Ok(Html(format!("<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>Webhook setup</title><main><h1>{label}</h1><p>Use your WebAgent origin with this POST path:</p><code>/api/v3/core/webhook-receive/{id}</code><p>Send Content-Type: application/json and a unique X-Webhook-Event-ID header. JSON bodies are limited to 16 KiB. Event IDs are deduplicated within the retained log.</p><p>Keep this header private:</p><pre data-secret=\"true\">Authorization: Bearer {token}</pre><p>Receiving stores untrusted data. Agent follow-up requires a separately permitted and armed Automation subscription to webhook.received, filtered by webhook_id. Read private data with get_webhook_log.</p></main></html>")))
}
#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    #[tokio::test]
    async fn callback_credentials_are_bound_to_purpose_id_and_expiry() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let id = uuid();
        let token=URL_SAFE_NO_PAD.encode(app.store.seal(&json!({"purpose":"core-webhook-receiver-v1","webhook_id":id,"owner_id":"admin","expires_at":now()+60})).unwrap());
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        assert!(decode_token(&app.store, &id, &headers).is_ok());
        assert!(decode_token(&app.store, &uuid(), &headers).is_err());
        let wrong = URL_SAFE_NO_PAD.encode(
            app.store
                .seal(&json!({"purpose":"search-cursor","webhook_id":id,"expires_at":now()+60}))
                .unwrap(),
        );
        headers.insert("authorization", format!("Bearer {wrong}").parse().unwrap());
        assert!(decode_token(&app.store, &id, &headers).is_err());
        assert_eq!(html_escape("<script>\"&"), "&lt;script&gt;&quot;&amp;");
    }
}
