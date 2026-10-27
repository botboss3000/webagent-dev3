//! Claude Bridge: a model call waits in the shared application store until the
//! client holding the connection's link token fetches it and posts the reply.
//! The API (client endpoints) and the loop (calls) are separate processes; both
//! poll the same sealed documents, so a restart simply drops waiting calls.
use crate::*;
use axum::http::HeaderMap;
use webagent_models::{model_selection::FailureKind, transport::{typed_failure, BridgeCall, BridgeQueue}};

const CALLS: &str = "claude_bridge_call:";
const SEEN: &str = "claude_bridge_seen:";
/// Written when a bridge connection's link token is saved; the client endpoints admit only these.
const LINKS: &str = "claude_bridge_link:";
/// No client fetch within this window means the client is offline.
const OFFLINE_SECS: u64 = 60;
/// A fetched call waits this long for its reply.
const ANSWER_SECS: u64 = 30 * 60;
/// Client long-poll window.
const POLL_SECS: u64 = 25;

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()
}
use webagent_models::transport::bridge_link_id as link_id;

pub(crate) struct Queue(pub(crate) Store);
impl BridgeQueue for Queue {
    fn exchange(&self, call: BridgeCall) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>> {
        Box::pin(async move {
            let link = link_id(&call.link);
            let offline = || typed_failure(FailureKind::ModelUnavailable, false,
                "Claude Bridge client offline; start the client for this link");
            let seen = self.0.get(&format!("{SEEN}{link}")).await?.and_then(|v| v["at"].as_u64());
            if seen.is_none_or(|at| now() > at + OFFLINE_SECS) { return Err(offline()); }
            let id = Uuid::new_v4().to_string();
            let key = format!("{CALLS}{link}:{id}");
            self.0.put(&key, &json!({"id":id,"session":call.session,"body":call.body,"state":"pending","created":now()})).await?;
            let result = async {
                loop {
                    tokio::select! {
                        _ = call.cancel.cancelled() => return Err(typed_failure(FailureKind::Cancelled, false, "Provider request cancelled")),
                        _ = tokio::time::sleep(Duration::from_secs(1)) => {}
                    }
                    let Some(doc) = self.0.get(&key).await? else { return Err(offline()) };
                    match doc["state"].as_str() {
                        Some("replied") => return Ok(doc["reply"].clone()),
                        Some("pending") if now() > doc["created"].as_u64().unwrap_or(0) + OFFLINE_SECS => return Err(offline()),
                        Some("claimed") if now() > doc["claimed"].as_u64().unwrap_or(0) + ANSWER_SECS =>
                            return Err(typed_failure(FailureKind::Transient, true, "Claude Bridge reply timed out")),
                        _ => {}
                    }
                }
            }.await;
            let _ = self.0.delete(&key).await;
            result
        })
    }
}

/// Admit only a bearer token saved on some bridge connection; return its link id.
async fn admit(store: &Store, headers: &HeaderMap) -> Result<String, ApiError> {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ")).filter(|t| (8..=512).contains(&t.len()))
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Link token required".into()))?;
    let link = link_id(token);
    if store.get(&format!("{LINKS}{link}")).await?.is_none() {
        return Err(ApiError(StatusCode::UNAUTHORIZED, "Unknown link token".into()));
    }
    Ok(link)
}

/// Long-poll for the next pending call on this link, optionally for one WebAgent chat.
pub async fn next(State(app): State<App>, headers: HeaderMap, Json(input): Json<Value>) -> Result<Json<Value>, ApiError> {
    let link = admit(&app.store, &headers).await?;
    let session = input["session"].as_str();
    let deadline = now() + POLL_SECS;
    // Each poll lasts under OFFLINE_SECS, so one mark per poll keeps a running client online.
    app.store.put(&format!("{SEEN}{link}"), &json!({"at":now()})).await?;
    loop {
        {
            let _guard = app.gate.lock().await;
            for mut call in app.store.list(&format!("{CALLS}{link}:")).await? {
                let key = format!("{CALLS}{link}:{}", call["id"].as_str().unwrap_or(""));
                // The loop has already failed these calls; drop the leftovers.
                let stale = match call["state"].as_str() {
                    Some("pending") => now() > call["created"].as_u64().unwrap_or(0) + OFFLINE_SECS,
                    Some("claimed") => now() > call["claimed"].as_u64().unwrap_or(0) + ANSWER_SECS,
                    _ => now() > call["created"].as_u64().unwrap_or(0) + 2 * ANSWER_SECS,
                };
                if stale { app.store.delete(&key).await?; continue; }
                if call["state"] != "pending" || session.is_some_and(|s| call["session"] != s) { continue; }
                call["state"] = json!("claimed");
                call["claimed"] = json!(now());
                app.store.put(&key, &call).await?;
                return Ok(Json(json!({"call":{"id":call["id"],"session":call["session"],"body":call["body"]}})));
            }
        }
        if now() >= deadline { return Ok(Json(json!({"call":null}))); }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// Accept the reply for a fetched call: a chat-completions assistant `message` and optional `usage`.
/// Content is validated by the transport exactly like any provider reply.
pub async fn reply(State(app): State<App>, headers: HeaderMap, Json(input): Json<Value>) -> Result<Json<Value>, ApiError> {
    let link = admit(&app.store, &headers).await?;
    let id = input["id"].as_str().filter(|id| Uuid::parse_str(id).is_ok())
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Call id required".into()))?;
    if !input["message"].is_object() || !(input["usage"].is_null() || input["usage"].is_object()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Reply needs a message object".into()));
    }
    let key = format!("{CALLS}{link}:{id}");
    let _guard = app.gate.lock().await;
    let call = app.store.get(&key).await?.filter(|c| c["state"] == "claimed")
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Call not waiting for a reply".into()))?;
    app.store.put(&key, &json!({"id":id,"session":call["session"],"state":"replied","created":call["created"],
        "reply":{"message":input["message"],"usage":input["usage"]}})).await?;
    Ok(Json(json!({"accepted":true})))
}

#[cfg(test)]
mod tests {
    #[test]
    fn link_ids_are_stable_digests() {
        let id = super::link_id("fixture-link-value");
        assert!(!id.contains("fixture"));
        assert_eq!(id, super::link_id("fixture-link-value"));
        assert_ne!(id, super::link_id("fixture-link-other"));
    }
}
