//! Harness bridge, common to every bridge provider (Claude, OpenCode, Muse): a model call waits in
//! the shared application store until a paired client fetches it, runs it with the harness its
//! `provider` names, and posts the reply. Any number of clients, and processes of one client, may
//! serve a connection: a fetched call is only offered until one process acknowledges it, so a
//! lost poll response returns the call to the queue instead of stranding it.
//! The API (client endpoints) and the loop (calls) are separate processes; both
//! poll the same sealed documents, so a restart simply drops waiting calls.
use crate::*;
use axum::{http::HeaderMap, Extension};
use webagent_configuration::digest;
use webagent_models::{
    model_selection::FailureKind,
    transport::{typed_failure, BridgeCall, BridgeQueue},
};

const CALLS: &str = "harness_bridge_call:";
const SEEN: &str = "harness_bridge_seen:";
/// `{link, expires}` for one unused pairing code, keyed by the code's digest.
const PAIRINGS: &str = "harness_bridge_pairing:";
/// `{id, name, created, secret, last_seen, instances}` per paired client, under its link.
const CLIENTS: &str = "harness_bridge_client:";
/// `{link, client}` keyed by a client secret's digest: the client endpoints' admission.
const SECRETS: &str = "harness_bridge_secret:";
/// A queued call no client fetches within this window fails.
const OFFLINE_SECS: u64 = 60;
/// A link that polled within this window is online; a process counts as running for this long.
const RECENT_SECS: u64 = 120;
/// A fetched call is offered this long; unacknowledged, it returns to the queue.
const LEASE_SECS: u64 = 60;
/// An acknowledged call waits this long for its reply.
const ANSWER_SECS: u64 = 30 * 60;
/// Client long-poll window.
const POLL_SECS: u64 = 25;
/// A pairing code is valid for this long, once.
const PAIRING_SECS: u64 = 10 * 60;
/// How often a waiting side re-reads the call document. The API and the loop are separate
/// processes, so polling is the only signal; a short tick makes a queued call and its reply
/// visible within a fraction of a second instead of a whole one.
const TICK: Duration = Duration::from_millis(250);

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
use webagent_models::transport::bridge_link_id as link_id;

fn recently_seen(seen: Option<u64>, at: u64) -> bool {
    seen.is_some_and(|seen| at <= seen + RECENT_SECS)
}

/// A call no process holds: never fetched, or offered and left unacknowledged past its lease.
fn open(call: &Value, at: u64) -> bool {
    match call["state"].as_str() {
        Some("pending") => true,
        Some("offered") => at > call["offered"].as_u64().unwrap_or(0) + LEASE_SECS,
        _ => false,
    }
}

/// A call the loop has already failed or finished with; the API drops these leftovers.
fn stale(call: &Value, at: u64) -> bool {
    let since = |field: &str| call[field].as_u64().unwrap_or(0);
    match call["state"].as_str() {
        Some("pending") => at > since("created") + OFFLINE_SECS,
        Some("offered") => at > since("offered") + LEASE_SECS + OFFLINE_SECS,
        Some("claimed") => at > since("claimed") + ANSWER_SECS,
        _ => at > since("created") + 2 * ANSWER_SECS,
    }
}

/// Short pairing code from an unambiguous alphabet, shown as `XXXX-XXXX`.
fn pairing_code() -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    // Skip the version and variant bytes, which are not random.
    let bytes = Uuid::new_v4().into_bytes();
    let code: String = bytes[..6]
        .iter()
        .chain(&bytes[9..11])
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect();
    format!("{}-{}", &code[..4], &code[4..])
}

/// The code as typed: case, spaces and the dash do not matter.
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

pub(crate) struct Queue(pub(crate) Store);
impl BridgeQueue for Queue {
    fn exchange(
        &self,
        call: BridgeCall,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>> {
        Box::pin(async move {
            let link = link_id(&call.link);
            let offline = || {
                typed_failure(
                    FailureKind::ModelUnavailable,
                    false,
                    "Bridge client offline; start a paired client for this model",
                )
            };
            let seen = self
                .0
                .get(&format!("{SEEN}{link}"))
                .await?
                .and_then(|v| v["at"].as_u64());
            if !recently_seen(seen, now()) {
                return Err(offline());
            }
            let id = Uuid::new_v4().to_string();
            let key = format!("{CALLS}{link}:{id}");
            self.0.put(&key, &json!({"id":id,"session":call.session,"provider":call.provider,"body":call.body,"state":"pending","created":now()})).await?;
            // No bridge failure reaches WebAgent with a partial response or a tool effect (the
            // harness only records tool calls), so each one is reported as not started.
            let result = async {
                loop {
                    tokio::select! {
                        _ = call.cancel.cancelled() => return Err(typed_failure(FailureKind::Cancelled, false, "Provider request cancelled")),
                        _ = tokio::time::sleep(TICK) => {}
                    }
                    let Some(doc) = self.0.get(&key).await? else { return Err(offline()) };
                    match doc["state"].as_str() {
                        Some("replied") => return Ok(doc["reply"].clone()),
                        Some("failed") => {
                            let error = doc["error"].as_str().unwrap_or("no detail");
                            return Err(typed_failure(
                                FailureKind::Transient,
                                false,
                                format!("Bridge client failed: {error}"),
                            ));
                        }
                        Some("pending") if now() > doc["created"].as_u64().unwrap_or(0) + OFFLINE_SECS => {
                            let seen = self.0.get(&format!("{SEEN}{link}")).await?.and_then(|v| v["at"].as_u64());
                            return Err(if recently_seen(seen, now()) {
                                typed_failure(FailureKind::Transient, false, "Bridge call not picked up; the client polled recently, retrying")
                            } else {
                                offline()
                            });
                        }
                        Some("offered") if stale(&doc, now()) =>
                            return Err(typed_failure(FailureKind::Transient, false, "Bridge call was never acknowledged by a client")),
                        Some("claimed") if now() > doc["claimed"].as_u64().unwrap_or(0) + ANSWER_SECS =>
                            return Err(typed_failure(FailureKind::Transient, false, "Bridge reply timed out")),
                        _ => {}
                    }
                }
            }.await;
            let _ = self.0.delete(&key).await;
            result
        })
    }
}

/// One paired client process, admitted by its client secret.
struct Caller {
    link: String,
    client: String,
    instance: String,
}

/// Admit only a paired client's secret; the body names the calling process.
async fn admit(store: &Store, headers: &HeaderMap, input: &Value) -> Result<Caller, ApiError> {
    let unauthorized = |message: &str| ApiError(StatusCode::UNAUTHORIZED, message.into());
    let secret = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| (8..=512).contains(&t.len()))
        .ok_or_else(|| unauthorized("Client secret required; pair this client first"))?;
    let admission = store
        .get(&format!("{SECRETS}{}", digest(secret)))
        .await?
        .ok_or_else(|| unauthorized("Unknown or revoked client; pair this client again"))?;
    let instance = input["instance"]
        .as_str()
        .filter(|id| Uuid::parse_str(id).is_ok())
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Instance id required".into()))?;
    Ok(Caller {
        link: admission["link"].as_str().unwrap_or("").to_owned(),
        client: admission["client"].as_str().unwrap_or("").to_owned(),
        instance: instance.to_owned(),
    })
}

fn call_id(input: &Value) -> Result<&str, ApiError> {
    input["id"]
        .as_str()
        .filter(|id| Uuid::parse_str(id).is_ok())
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Call id required".into()))
}

/// Mark the link online and this process running.
async fn mark_seen(app: &App, caller: &Caller) -> Result<(), ApiError> {
    let at = now();
    app.store
        .put(&format!("{SEEN}{}", caller.link), &json!({"at":at}))
        .await?;
    let _guard = app.gate.lock().await;
    let key = format!("{CLIENTS}{}:{}", caller.link, caller.client);
    if let Some(mut client) = app.store.get(&key).await? {
        let mut instances = client["instances"].as_object().cloned().unwrap_or_default();
        instances.retain(|_, seen| recently_seen(seen.as_u64(), at));
        instances.insert(caller.instance.clone(), json!(at));
        client["instances"] = Value::Object(instances);
        client["last_seen"] = json!(at);
        app.store.put(&key, &client).await?;
    }
    Ok(())
}

/// Long-poll for the next open call on this link, optionally for one WebAgent chat. The call is
/// only offered: the process must acknowledge it before running it.
pub async fn next(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let caller = admit(&app.store, &headers, &input).await?;
    let session = input["session"].as_str();
    let deadline = now() + POLL_SECS;
    // Each poll lasts under OFFLINE_SECS, so one mark per poll keeps a running client online.
    mark_seen(&app, &caller).await?;
    loop {
        {
            let _guard = app.gate.lock().await;
            for mut call in app.store.list(&format!("{CALLS}{}:", caller.link)).await? {
                let key = format!(
                    "{CALLS}{}:{}",
                    caller.link,
                    call["id"].as_str().unwrap_or("")
                );
                if stale(&call, now()) {
                    app.store.delete(&key).await?;
                    continue;
                }
                if !open(&call, now()) || session.is_some_and(|s| call["session"] != s) {
                    continue;
                }
                call["state"] = json!("offered");
                call["offered"] = json!(now());
                call["client"] = json!(caller.client);
                call["instance"] = json!(caller.instance);
                app.store.put(&key, &call).await?;
                return Ok(Json(
                    json!({"call":{"id":call["id"],"session":call["session"],"provider":call["provider"],"body":call["body"]}}),
                ));
            }
        }
        if now() >= deadline {
            return Ok(Json(json!({"call":null})));
        }
        tokio::time::sleep(TICK).await;
    }
}

/// Take an offered call before running it. Only the process it was offered to, within its lease,
/// can take it, so every call runs on exactly one process.
pub async fn ack(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let caller = admit(&app.store, &headers, &input).await?;
    let key = format!("{CALLS}{}:{}", caller.link, call_id(&input)?);
    let _guard = app.gate.lock().await;
    let mut call = app
        .store
        .get(&key)
        .await?
        .filter(|c| {
            c["state"] == "offered"
                && !open(c, now())
                && c["client"] == caller.client.as_str()
                && c["instance"] == caller.instance.as_str()
        })
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "Call is no longer offered to this client".into(),
            )
        })?;
    call["state"] = json!("claimed");
    call["claimed"] = json!(now());
    app.store.put(&key, &call).await?;
    Ok(Json(json!({"acknowledged":true})))
}

/// Accept the outcome of an acknowledged call from the process that took it: a chat-completions
/// assistant `message` with optional `usage`, or an `error` when the harness failed. Content is
/// validated by the transport exactly like any provider reply.
pub async fn reply(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let caller = admit(&app.store, &headers, &input).await?;
    let id = call_id(&input)?;
    let error = input["error"].as_str();
    if error.is_none()
        && (!input["message"].is_object()
            || !(input["usage"].is_null() || input["usage"].is_object()))
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Reply needs a message object or an error".into(),
        ));
    }
    let key = format!("{CALLS}{}:{id}", caller.link);
    let _guard = app.gate.lock().await;
    let call = app
        .store
        .get(&key)
        .await?
        .filter(|c| {
            c["state"] == "claimed"
                && c["client"] == caller.client.as_str()
                && c["instance"] == caller.instance.as_str()
        })
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Call not waiting for a reply".into()))?;
    let outcome = match error {
        Some(error) => {
            json!({"state":"failed","error":error.chars().take(300).collect::<String>()})
        }
        None => {
            json!({"state":"replied","reply":{"message":input["message"],"usage":input["usage"]}})
        }
    };
    let mut done = outcome;
    done["id"] = json!(id);
    done["session"] = call["session"].clone();
    done["created"] = call["created"].clone();
    app.store.put(&key, &done).await?;
    Ok(Json(json!({"accepted":true})))
}

/// The link of a bridge connection the signed-in caller owns.
async fn owned_link(app: &App, identity: &Value, connection: &str) -> Result<String, ApiError> {
    let secret = webagent_models::api::selection::bridge_link_secret(
        &app.models_runtime(),
        identity,
        connection,
    )
    .await
    .map_err(ApiError::from)?;
    Ok(link_id(&secret))
}

/// Issue a one-time code that pairs a new client with this connection.
pub async fn pairing(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(connection): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &connection).await?;
    let code = pairing_code();
    app.store
        .put(
            &format!("{PAIRINGS}{}", digest(&normalize_code(&code))),
            &json!({"link":link,"expires":now() + PAIRING_SECS}),
        )
        .await?;
    Ok(Json(json!({"code":code,"expires_in":PAIRING_SECS})))
}

/// Trade a pairing code for this client's own secret, returned once.
pub async fn pair(
    State(app): State<App>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let code = normalize_code(input["code"].as_str().unwrap_or(""));
    let name = input["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Bridge client");
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Client name: up to 80 characters".into(),
        ));
    }
    let invalid = || {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "Pairing code is invalid, used or expired".into(),
        )
    };
    let key = format!("{PAIRINGS}{}", digest(&code));
    let _guard = app.gate.lock().await;
    let pairing = app.store.get(&key).await?.ok_or_else(invalid)?;
    // A code works once, even when it has expired.
    app.store.delete(&key).await?;
    if now() > pairing["expires"].as_u64().unwrap_or(0) {
        return Err(invalid());
    }
    let link = pairing["link"].as_str().unwrap_or("");
    let client = Uuid::new_v4().to_string();
    let secret = format!("hbc_{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let secret_key = digest(&secret);
    app.store
        .put(
            &format!("{SECRETS}{secret_key}"),
            &json!({"link":link,"client":client}),
        )
        .await?;
    app.store
        .put(
            &format!("{CLIENTS}{link}:{client}"),
            &json!({"id":client,"name":name,"created":now(),"secret":secret_key,"last_seen":null,"instances":{}}),
        )
        .await?;
    Ok(Json(json!({"client_id":client,"secret":secret})))
}

/// The clients paired with this connection, with when each last polled and how many of its
/// processes are running.
pub async fn clients(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(connection): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &connection).await?;
    let at = now();
    let clients: Vec<Value> = app
        .store
        .list(&format!("{CLIENTS}{link}:"))
        .await?
        .into_iter()
        .map(|c| {
            let running = c["instances"]
                .as_object()
                .map_or(0, |i| i.values().filter(|seen| recently_seen(seen.as_u64(), at)).count());
            json!({"id":c["id"],"name":c["name"],"created":c["created"],"last_seen":c["last_seen"],"processes":running})
        })
        .collect();
    Ok(Json(json!({"clients":clients})))
}

/// Revoke one paired client. Calls it holds return to the queue for the other clients.
pub async fn revoke(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((connection, client)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &connection).await?;
    let key = format!("{CLIENTS}{link}:{client}");
    let _guard = app.gate.lock().await;
    let record = app
        .store
        .get(&key)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Client not found".into()))?;
    app.store
        .delete(&format!(
            "{SECRETS}{}",
            record["secret"].as_str().unwrap_or("")
        ))
        .await?;
    app.store.delete(&key).await?;
    for mut call in app.store.list(&format!("{CALLS}{link}:")).await? {
        if call["client"] == client.as_str()
            && matches!(call["state"].as_str(), Some("offered" | "claimed"))
        {
            let call_key = format!("{CALLS}{link}:{}", call["id"].as_str().unwrap_or(""));
            call["state"] = json!("pending");
            call["created"] = json!(now());
            app.store.put(&call_key, &call).await?;
        }
    }
    Ok(Json(json!({"revoked":true})))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn link_ids_are_stable_digests() {
        let id = super::link_id("fixture-link-value");
        assert!(!id.contains("fixture"));
        assert_eq!(id, super::link_id("fixture-link-value"));
        assert_ne!(id, super::link_id("fixture-link-other"));
    }

    #[test]
    fn unclaimed_call_is_retryable_only_on_a_recently_seen_link() {
        assert!(super::recently_seen(Some(1000), 1000 + super::RECENT_SECS));
        assert!(!super::recently_seen(Some(1000), 1001 + super::RECENT_SECS));
        assert!(!super::recently_seen(None, 1000));
    }

    /// An offered call is held only for its lease; then any process may take it again.
    #[test]
    fn offered_calls_return_to_the_queue_after_their_lease() {
        let offered = json!({"state":"offered","offered":1000,"created":990});
        assert!(!super::open(&offered, 1000 + super::LEASE_SECS));
        assert!(super::open(&offered, 1001 + super::LEASE_SECS));
        assert!(super::open(
            &json!({"state":"pending","created":1000}),
            1000
        ));
        assert!(!super::open(
            &json!({"state":"claimed","claimed":1000}),
            99_999
        ));
        assert!(!super::stale(
            &offered,
            1000 + super::LEASE_SECS + super::OFFLINE_SECS
        ));
        assert!(super::stale(
            &offered,
            1001 + super::LEASE_SECS + super::OFFLINE_SECS
        ));
    }

    #[test]
    fn pairing_codes_are_short_and_typed_loosely() {
        let code = super::pairing_code();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert_eq!(
            super::normalize_code(&code.to_lowercase().replace('-', " ")),
            code.replace('-', "")
        );
        assert_ne!(super::pairing_code(), code);
    }
}
