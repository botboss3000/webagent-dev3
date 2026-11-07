//! Harness bridge, common to every bridge provider (Claude, OpenCode, Muse): a model call waits in
//! the shared application store until a paired client fetches it, runs it with the harness its
//! `provider` names, and posts the reply. Any number of clients, and processes of one client, may
//! serve one user's bridge provider (all of their models of it, on one link): a fetched call is
//! only offered until one process acknowledges it, so a
//! lost poll response returns the call to the queue instead of stranding it.
//! The API (client endpoints) and the loop (calls) are separate processes; both
//! poll the same sealed documents, so a restart simply drops waiting calls.
//! Stop on a call a client already took marks it cancelled; that process's next poll returns its
//! id, and the client interrupts the harness running it.
use crate::*;
use axum::{http::HeaderMap, Extension};
use webagent_configuration::digest;
use webagent_models::settings::{Bridge, Settings};
use webagent_models::{
    model_selection::FailureKind,
    transport::{typed_failure, typed_failure_after, BridgeCall, BridgeQueue},
};

const CALLS: &str = "harness_bridge_call:";
const SEEN: &str = "harness_bridge_seen:";
/// `{link, expires}` for one unused pairing code, keyed by the code's digest.
const PAIRINGS: &str = "harness_bridge_pairing:";
/// `{id, name, created, secret, last_seen, instances}` per paired client, under its link.
const CLIENTS: &str = "harness_bridge_client:";
/// `{link, client}` keyed by a client secret's digest: the client endpoints' admission.
const SECRETS: &str = "harness_bridge_secret:";
/// Timers come from the live `models-runtime.json` (`harness_bridge`), read on use.
fn bridge() -> Bridge {
    Settings::live().harness_bridge
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
use webagent_models::transport::bridge_link_id as link_id;

fn recently_seen(seen: Option<u64>, at: u64, b: &Bridge) -> bool {
    seen.is_some_and(|seen| at <= seen + b.client_online_seconds)
}

/// A call no process holds: never fetched, or offered and left unacknowledged past its lease.
fn open(call: &Value, at: u64, b: &Bridge) -> bool {
    match call["state"].as_str() {
        Some("pending") => true,
        Some("offered") => at > call["offered"].as_u64().unwrap_or(0) + b.offer_lease_seconds,
        _ => false,
    }
}

/// A call the loop has already failed or finished with; the API drops these leftovers.
fn stale(call: &Value, at: u64, b: &Bridge) -> bool {
    let since = |field: &str| call[field].as_u64().unwrap_or(0);
    match call["state"].as_str() {
        Some("pending") => at > since("created") + b.unclaimed_call_seconds,
        Some("offered") => at > since("offered") + b.offer_lease_seconds + b.unclaimed_call_seconds,
        Some("claimed") => at > since("claimed") + b.answer_seconds,
        Some("cancelled") => at > since("cancelled") + b.unclaimed_call_seconds,
        _ => at > since("created") + 2 * b.answer_seconds,
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
            if !recently_seen(seen, now(), &bridge()) {
                return Err(offline());
            }
            let id = Uuid::new_v4().to_string();
            let key = format!("{CALLS}{link}:{id}");
            self.0.put(&key, &json!({"id":id,"session":call.session,"provider":call.provider,"body":call.body,"state":"pending","created":now()})).await?;
            // Stop usually drops this future rather than firing `call.cancel`; the guard settles
            // the call either way.
            let mut guard = Abandoned {
                store: self.0.clone(),
                key: key.clone(),
                armed: true,
            };
            let mut cancelled = false;
            // No bridge failure reaches WebAgent with a partial response or a tool effect (the
            // harness only records tool calls), so each one is reported as not started.
            let result = async {
                loop {
                    let b = bridge();
                    tokio::select! {
                        _ = call.cancel.cancelled() => {
                            cancelled = true;
                            return Err(typed_failure(FailureKind::Cancelled, false, "Provider request cancelled"));
                        }
                        _ = tokio::time::sleep(Duration::from_millis(b.store_poll_ms)) => {}
                    }
                    let Some(doc) = self.0.get(&key).await? else { return Err(offline()) };
                    match doc["state"].as_str() {
                        Some("replied") => return Ok(doc["reply"].clone()),
                        Some("failed") => return Err(failure_of(&doc)),
                        Some("pending") if now() > doc["created"].as_u64().unwrap_or(0) + b.unclaimed_call_seconds => {
                            let seen = self.0.get(&format!("{SEEN}{link}")).await?.and_then(|v| v["at"].as_u64());
                            return Err(if recently_seen(seen, now(), &b) {
                                typed_failure(FailureKind::Transient, false, "Bridge call not picked up; the client polled recently, retrying")
                            } else {
                                offline()
                            });
                        }
                        Some("offered") if stale(&doc, now(), &b) =>
                            return Err(typed_failure(FailureKind::Transient, false, "Bridge call was never acknowledged by a client")),
                        // A heartbeating client must keep beating; one that never beats gets the
                        // whole answer window.
                        Some("claimed") if doc["progress"].as_u64().is_some_and(|beat| now() > beat + b.heartbeat_timeout_seconds) =>
                            return Err(typed_failure(FailureKind::Transient, false, format!(
                                "Bridge client stopped responding (no heartbeat for {} s)", b.heartbeat_timeout_seconds))),
                        Some("claimed") if doc["progress"].is_null() && now() > doc["claimed"].as_u64().unwrap_or(0) + b.answer_seconds =>
                            return Err(typed_failure(FailureKind::Transient, false, "Bridge reply timed out")),
                        _ => {}
                    }
                }
            }.await;
            guard.armed = false;
            if cancelled {
                let _ = settle_cancelled(&self.0, &key).await;
            } else {
                let _ = self.0.delete(&key).await;
            }
            result
        })
    }
}

/// A client-reported failure, kept apart by where it happened: between the harness and its own
/// provider ("Bridge provider …", typed so overload and quota are handled like any provider's),
/// or in the client and harness themselves ("Bridge client failed").
fn failure_of(doc: &Value) -> anyhow::Error {
    let error = doc["error"].as_str().unwrap_or("no detail");
    let retry_after = doc["retry_after"].as_u64();
    if doc["source"] != "upstream" {
        return typed_failure_after(
            FailureKind::Transient,
            false,
            format!("Bridge client failed: {error}"),
            retry_after,
        );
    }
    let (kind, label) = match doc["category"].as_str() {
        Some("overloaded") => (FailureKind::Transient, "overloaded"),
        Some("stalled") => (FailureKind::Transient, "not responding"),
        Some("quota") => (FailureKind::QuotaExhausted, "usage limit reached"),
        Some("auth") => (FailureKind::InvalidCredentials, "authorization failed"),
        _ => (FailureKind::Transient, "error"),
    };
    typed_failure_after(
        kind,
        false,
        format!("Bridge provider {label}: {error}"),
        retry_after,
    )
}

/// Settles a stopped call: one a client is running is marked cancelled, so that client's next
/// poll interrupts its harness; any other is removed.
async fn settle_cancelled(store: &Store, key: &str) -> Result<()> {
    match store.get(key).await? {
        Some(mut doc) if doc["state"] == "claimed" => {
            doc["state"] = json!("cancelled");
            doc["cancelled"] = json!(now());
            store.put(key, &doc).await?;
        }
        _ => {
            let _ = store.delete(key).await;
        }
    }
    Ok(())
}

/// Settles a call whose waiter was dropped before an outcome, as WebAgent's Stop does.
struct Abandoned {
    store: Store,
    key: String,
    armed: bool,
}

impl Drop for Abandoned {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let (store, key) = (self.store.clone(), std::mem::take(&mut self.key));
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _ = settle_cancelled(&store, &key).await;
            });
        }
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
        let b = bridge();
        instances.retain(|_, seen| recently_seen(seen.as_u64(), at, &b));
        instances.insert(caller.instance.clone(), json!(at));
        client["instances"] = Value::Object(instances);
        client["last_seen"] = json!(at);
        app.store.put(&key, &client).await?;
    }
    Ok(())
}

/// Long-poll for the next open call on this link, optionally for one WebAgent chat. The call is
/// only offered: the process must acknowledge it before running it. `cancelled` lists calls this
/// process took that WebAgent has since stopped; the client interrupts their harnesses.
pub async fn next(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let caller = admit(&app.store, &headers, &input).await?;
    let session = input["session"].as_str();
    let b = bridge();
    let deadline = now() + b.poll_hold_seconds;
    // Each poll lasts under unclaimed_call_seconds, so one mark per poll keeps a running client online.
    // Every response carries the client's live settings.
    let settings = json!(b.client);
    mark_seen(&app, &caller).await?;
    loop {
        {
            let _guard = app.gate.lock().await;
            let mut cancelled = Vec::new();
            for mut call in app.store.list(&format!("{CALLS}{}:", caller.link)).await? {
                let key = format!(
                    "{CALLS}{}:{}",
                    caller.link,
                    call["id"].as_str().unwrap_or("")
                );
                if stale(&call, now(), &b) {
                    app.store.delete(&key).await?;
                    continue;
                }
                if call["state"] == "cancelled" && call["instance"] == caller.instance.as_str() {
                    app.store.delete(&key).await?;
                    cancelled.push(call["id"].clone());
                    continue;
                }
                if !open(&call, now(), &b) || session.is_some_and(|s| call["session"] != s) {
                    continue;
                }
                call["state"] = json!("offered");
                call["offered"] = json!(now());
                call["client"] = json!(caller.client);
                call["instance"] = json!(caller.instance);
                app.store.put(&key, &call).await?;
                return Ok(Json(
                    json!({"call":{"id":call["id"],"session":call["session"],"provider":call["provider"],"body":call["body"]},"cancelled":cancelled,"settings":settings}),
                ));
            }
            if !cancelled.is_empty() {
                return Ok(Json(
                    json!({"call":null,"cancelled":cancelled,"settings":settings}),
                ));
            }
        }
        if now() >= deadline {
            return Ok(Json(json!({"call":null,"settings":settings})));
        }
        tokio::time::sleep(Duration::from_millis(b.store_poll_ms)).await;
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
                && !open(c, now(), &bridge())
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

/// Heartbeat from the process running an acknowledged call. Once a client beats, silence longer
/// than `heartbeat_timeout_seconds` fails the call, so a crashed client is noticed in about a
/// minute instead of the whole answer window.
pub async fn progress(
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
            c["state"] == "claimed"
                && c["client"] == caller.client.as_str()
                && c["instance"] == caller.instance.as_str()
        })
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                "Call not running on this client".into(),
            )
        })?;
    call["progress"] = json!(now());
    app.store.put(&key, &call).await?;
    Ok(Json(json!({"ok":true})))
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
            // The client says whether its harness failed or the harness's own provider did.
            let source = match input["source"].as_str() {
                Some("upstream") => "upstream",
                _ => "harness",
            };
            let category = input["category"]
                .as_str()
                .filter(|c| matches!(*c, "overloaded" | "quota" | "auth" | "stalled" | "upstream"));
            let retry_after = input["retry_after"].as_u64().filter(|s| *s <= 600);
            json!({"state":"failed","error":error.chars().take(300).collect::<String>(),
                "source":source,"category":category,"retry_after":retry_after})
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

/// The signed-in caller's link for one bridge provider, shared by all their models of it.
async fn owned_link(app: &App, identity: &Value, provider: &str) -> Result<String, ApiError> {
    let secret = webagent_models::api::selection::bridge_link_secret(
        &app.models_runtime(),
        identity,
        provider,
    )
    .await
    .map_err(ApiError::from)?;
    Ok(link_id(&secret))
}

/// Issue a one-time code that pairs a new client with the caller's models of this provider.
pub async fn pairing(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &provider).await?;
    let pairing_seconds = bridge().pairing_code_seconds;
    let code = pairing_code();
    app.store
        .put(
            &format!("{PAIRINGS}{}", digest(&normalize_code(&code))),
            &json!({"link":link,"expires":now() + pairing_seconds}),
        )
        .await?;
    Ok(Json(json!({"code":code,"expires_in":pairing_seconds})))
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
    // Wrong codes from all clients share one cap, so guessing needs no address at all.
    let wait = crate::request_limits::pairing_wait(&app).await;
    if wait > 0 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            format!("Too many wrong pairing codes. Try again in {wait} seconds."),
        ));
    }
    let key = format!("{PAIRINGS}{}", digest(&code));
    let _guard = app.gate.lock().await;
    let Some(pairing) = app.store.get(&key).await? else {
        crate::request_limits::pairing_failed(&app).await;
        return Err(invalid());
    };
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

/// The clients paired with this provider, with when each last polled and how many of its
/// processes are running.
pub async fn clients(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(provider): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &provider).await?;
    let at = now();
    let b = bridge();
    let clients: Vec<Value> = app
        .store
        .list(&format!("{CLIENTS}{link}:"))
        .await?
        .into_iter()
        .map(|c| {
            let running = c["instances"]
                .as_object()
                .map_or(0, |i| i.values().filter(|seen| recently_seen(seen.as_u64(), at, &b)).count());
            json!({"id":c["id"],"name":c["name"],"created":c["created"],"last_seen":c["last_seen"],"processes":running})
        })
        .collect();
    Ok(Json(json!({"clients":clients})))
}

/// Revoke one paired client. Calls it holds return to the queue for the other clients.
pub async fn revoke(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((provider, client)): Path<(String, String)>,
) -> Result<Json<Value>, ApiError> {
    let link = owned_link(&app, &identity, &provider).await?;
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
        let b = webagent_models::settings::Settings::default().harness_bridge;
        assert!(super::recently_seen(
            Some(1000),
            1000 + b.client_online_seconds,
            &b
        ));
        assert!(!super::recently_seen(
            Some(1000),
            1001 + b.client_online_seconds,
            &b
        ));
        assert!(!super::recently_seen(None, 1000, &b));
    }

    /// An offered call is held only for its lease; then any process may take it again.
    #[test]
    fn offered_calls_return_to_the_queue_after_their_lease() {
        let b = webagent_models::settings::Settings::default().harness_bridge;
        let offered = json!({"state":"offered","offered":1000,"created":990});
        assert!(!super::open(&offered, 1000 + b.offer_lease_seconds, &b));
        assert!(super::open(&offered, 1001 + b.offer_lease_seconds, &b));
        assert!(super::open(
            &json!({"state":"pending","created":1000}),
            1000,
            &b
        ));
        assert!(!super::open(
            &json!({"state":"claimed","claimed":1000}),
            99_999,
            &b
        ));
        assert!(!super::stale(
            &offered,
            1000 + b.offer_lease_seconds + b.unclaimed_call_seconds,
            &b
        ));
        assert!(super::stale(
            &offered,
            1001 + b.offer_lease_seconds + b.unclaimed_call_seconds,
            &b
        ));
    }

    /// A stopped call is never offered again and is dropped soon if its client never polls.
    #[test]
    fn cancelled_calls_are_closed_and_expire() {
        let b = webagent_models::settings::Settings::default().harness_bridge;
        let cancelled = json!({"state":"cancelled","cancelled":1000,"created":900});
        assert!(!super::open(&cancelled, 99_999, &b));
        assert!(!super::stale(
            &cancelled,
            1000 + b.unclaimed_call_seconds,
            &b
        ));
        assert!(super::stale(
            &cancelled,
            1001 + b.unclaimed_call_seconds,
            &b
        ));
    }

    /// Upstream overload, quota and auth stay distinct from a failing client, with the wait kept.
    #[test]
    fn client_failures_keep_their_source() {
        use webagent_models::transport::ProviderFailure;
        let kind = |doc: serde_json::Value| {
            let error = super::failure_of(&doc);
            let failure = error.downcast_ref::<ProviderFailure>().unwrap();
            (failure.kind, failure.retry_after, error.to_string())
        };
        let (k, wait, text) = kind(
            json!({"error":"[overloaded] busy","source":"upstream","category":"overloaded","retry_after":30}),
        );
        assert_eq!((k, wait), (super::FailureKind::Transient, Some(30)));
        assert!(text.starts_with("Bridge provider overloaded"));
        assert_eq!(
            kind(json!({"error":"funds","source":"upstream","category":"quota"})).0,
            super::FailureKind::QuotaExhausted
        );
        assert_eq!(
            kind(json!({"error":"key","source":"upstream","category":"auth"})).0,
            super::FailureKind::InvalidCredentials
        );
        assert!(kind(json!({"error":"crash"}))
            .2
            .starts_with("Bridge client failed"));
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
