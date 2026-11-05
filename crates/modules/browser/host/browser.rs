//! Root adapter for Browser Extension: device linking, the extension's action queue and the
//! `browser_backend`/`browser_action` tools. Rules live in `webagent_browser`.
//!
//! The API (extension endpoints) and the loop (tools) are separate processes; both read the
//! shared application store, the same way the harness bridge does. A device belongs to one
//! WebAgent account and only ever receives that account's actions.
use crate::*;
use axum::{http::HeaderMap, Extension};
use tokio_util::sync::CancellationToken;
use webagent_browser as rules;
use webagent_configuration::digest;

/// `{id, user, pending, args, sites, state, created, ...}` per queued action, under its user.
const CALLS: &str = "browser_call:";
/// `{id, name, created, secret, last_seen}` per linked device, under its user.
const DEVICES: &str = "browser_device:";
/// `{user, device}` keyed by a device secret's digest: the extension endpoints' admission.
const SECRETS: &str = "browser_secret:";
/// `{user, expires}` per unused link code, keyed by the code's digest.
const LINKS: &str = "browser_link:";
/// How often a waiting side re-reads the call document.
const TICK: Duration = Duration::from_millis(250);

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn signed_in(identity: &Value) -> Result<&str, ApiError> {
    identity["_user_id"]
        .as_str()
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in first".into()))
}

/// Issue a one-time code that links the caller's browser extension to this account. The
/// chat panel asks for one whenever the extension reports a different (or no) account, so
/// linking follows whoever is signed in to the widget. `linked_user` is only that report.
pub(crate) async fn link_code(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = signed_in(&identity)?;
    if input["linked_user"] == user {
        return Ok(Json(json!({"linked":true})));
    }
    let code = rules::link_code();
    app.store
        .put(
            &format!("{LINKS}{}", digest(&json!(code))),
            &json!({"user":user,"expires":now() + rules::LINK_SECS}),
        )
        .await?;
    Ok(Json(json!({"code":code})))
}

/// Trade a link code for this device's own secret, returned once.
pub(crate) async fn link(
    State(app): State<App>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let name = input["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or("Browser");
    if name.chars().count() > 80 || name.chars().any(char::is_control) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Device name: up to 80 characters".into(),
        ));
    }
    let invalid = || {
        ApiError(
            StatusCode::UNAUTHORIZED,
            "Link code is invalid, used or expired".into(),
        )
    };
    let code = input["code"].as_str().ok_or_else(invalid)?;
    let key = format!("{LINKS}{}", digest(&json!(code)));
    let _guard = app.gate.lock().await;
    let pending = app.store.get(&key).await?.ok_or_else(invalid)?;
    // A code works once, even when it has expired.
    app.store.delete(&key).await?;
    if now() > pending["expires"].as_u64().unwrap_or(0) {
        return Err(invalid());
    }
    let user = pending["user"].as_str().ok_or_else(invalid)?;
    let device = Uuid::new_v4().to_string();
    let secret = rules::device_secret();
    let secret_key = digest(&json!(secret));
    app.store
        .put(
            &format!("{SECRETS}{secret_key}"),
            &json!({"user":user,"device":device}),
        )
        .await?;
    app.store
        .put(
            &format!("{DEVICES}{user}:{device}"),
            &json!({"id":device,"name":name,"created":now(),"secret":secret_key,"last_seen":now()}),
        )
        .await?;
    Ok(Json(
        json!({"device_id":device,"secret":secret,"user_id":user}),
    ))
}

/// One linked device, admitted by its secret.
struct Device {
    user: String,
    id: String,
    secret_key: String,
}

async fn admit(store: &Store, headers: &HeaderMap) -> Result<Device, ApiError> {
    let unauthorized = |message: &str| ApiError(StatusCode::UNAUTHORIZED, message.into());
    let secret = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| (8..=512).contains(&t.len()))
        .ok_or_else(|| unauthorized("Device secret required; link this browser first"))?;
    let secret_key = digest(&json!(secret));
    let admission = store
        .get(&format!("{SECRETS}{secret_key}"))
        .await?
        .ok_or_else(|| unauthorized("Unknown or revoked device; link this browser again"))?;
    Ok(Device {
        user: admission["user"].as_str().unwrap_or("").to_owned(),
        id: admission["device"].as_str().unwrap_or("").to_owned(),
        secret_key,
    })
}

async fn forget(store: &Store, user: &str, device: &str, secret_key: &str) -> Result<()> {
    store.delete(&format!("{SECRETS}{secret_key}")).await?;
    store.delete(&format!("{DEVICES}{user}:{device}")).await?;
    // Actions it held return to the queue for the user's other devices.
    for mut call in store.list(&format!("{CALLS}{user}:")).await? {
        if call["device"] == device && call["state"] == "offered" {
            let key = format!("{CALLS}{user}:{}", call["id"].as_str().unwrap_or(""));
            call["state"] = json!("pending");
            call["created"] = json!(now());
            store.put(&key, &call).await?;
        }
    }
    Ok(())
}

/// The extension unlinks itself, for example before linking to a different account.
pub(crate) async fn unlink(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let device = admit(&app.store, &headers).await?;
    let _guard = app.gate.lock().await;
    forget(&app.store, &device.user, &device.id, &device.secret_key).await?;
    Ok(Json(json!({"unlinked":true})))
}

/// Long-poll for the user's next open action. It is only offered: the device must
/// acknowledge it before acting.
pub(crate) async fn next(
    State(app): State<App>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let device = admit(&app.store, &headers).await?;
    let deadline = now() + rules::POLL_SECS;
    let device_key = format!("{DEVICES}{}:{}", device.user, device.id);
    loop {
        {
            let _guard = app.gate.lock().await;
            if let Some(mut record) = app.store.get(&device_key).await? {
                record["last_seen"] = json!(now());
                app.store.put(&device_key, &record).await?;
            }
            for mut call in app.store.list(&format!("{CALLS}{}:", device.user)).await? {
                let key = format!(
                    "{CALLS}{}:{}",
                    device.user,
                    call["id"].as_str().unwrap_or("")
                );
                if rules::stale(&call, now()) {
                    app.store.delete(&key).await?;
                    continue;
                }
                if !rules::open(&call, now()) {
                    continue;
                }
                call["state"] = json!("offered");
                call["offered"] = json!(now());
                call["device"] = json!(device.id);
                app.store.put(&key, &call).await?;
                return Ok(Json(json!({"call":{
                    "id":call["id"],"args":call["args"],"sites":call["sites"],"session":call["session"],
                }})));
            }
        }
        if now() >= deadline {
            return Ok(Json(json!({"call":null})));
        }
        tokio::time::sleep(TICK).await;
    }
}

fn call_id(input: &Value) -> Result<&str, ApiError> {
    input["id"]
        .as_str()
        .filter(|id| id.len() == 64 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "Call id required".into()))
}

/// Take an offered action before running it, so each action runs on exactly one device.
pub(crate) async fn ack(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let device = admit(&app.store, &headers).await?;
    let key = format!("{CALLS}{}:{}", device.user, call_id(&input)?);
    let _guard = app.gate.lock().await;
    let mut call = app
        .store
        .get(&key)
        .await?
        .filter(|c| {
            c["state"] == "offered" && !rules::open(c, now()) && c["device"] == device.id.as_str()
        })
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "Action is no longer offered to this device".into(),
            )
        })?;
    call["state"] = json!("claimed");
    call["claimed"] = json!(now());
    app.store.put(&key, &call).await?;
    Ok(Json(json!({"acknowledged":true})))
}

/// Accept the outcome of an acknowledged action: `output` (any JSON, at most 200 KB) or
/// an `error`. `started` says whether a failed action may already have touched the page.
pub(crate) async fn result(
    State(app): State<App>,
    headers: HeaderMap,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let device = admit(&app.store, &headers).await?;
    let id = call_id(&input)?;
    let outcome = match input["error"].as_str() {
        Some(error) => json!({
            "state":"failed","error":error.chars().take(500).collect::<String>(),
            "started":input["started"] == true,
        }),
        None if serde_json::to_vec(&input["output"]).map_or(0, |b| b.len()) > 200_000 => {
            return Err(ApiError(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Result over 200 KB; return less".into(),
            ))
        }
        None => json!({"state":"replied","output":input["output"]}),
    };
    let key = format!("{CALLS}{}:{id}", device.user);
    let _guard = app.gate.lock().await;
    let mut call = app
        .store
        .get(&key)
        .await?
        .filter(|c| c["state"] == "claimed" && c["device"] == device.id.as_str())
        .ok_or_else(|| {
            ApiError(
                StatusCode::NOT_FOUND,
                "Action not waiting for a result".into(),
            )
        })?;
    for (field, value) in outcome.as_object().into_iter().flatten() {
        call[field] = value.clone();
    }
    app.store.put(&key, &call).await?;
    Ok(Json(json!({"accepted":true})))
}

/// The caller's linked browsers.
pub(crate) async fn devices(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = signed_in(&identity)?;
    Ok(Json(
        json!({"devices":device_list(&app.store, user).await?}),
    ))
}

/// Revoke one of the caller's linked browsers.
pub(crate) async fn revoke(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(device): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let user = signed_in(&identity)?;
    let _guard = app.gate.lock().await;
    let record = app
        .store
        .get(&format!("{DEVICES}{user}:{device}"))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Device not found".into()))?;
    forget(
        &app.store,
        user,
        &device,
        record["secret"].as_str().unwrap_or(""),
    )
    .await?;
    Ok(Json(json!({"revoked":true})))
}

async fn device_list(store: &Store, user: &str) -> Result<Vec<Value>> {
    let at = now();
    Ok(store
        .list(&format!("{DEVICES}{user}:"))
        .await?
        .into_iter()
        .map(|d| {
            let online = rules::recently_seen(d["last_seen"].as_u64(), at);
            json!({"id":d["id"],"name":d["name"],"created":d["created"],"last_seen":d["last_seen"],"online":online})
        })
        .collect())
}

pub(crate) fn owns(name: &str) -> bool {
    rules::TOOLS.contains(&name)
}

/// The agent's allowlisted sites for session `sid`.
async fn agent_sites(host: &tools::native_host::Host<'_>, sid: &str) -> Result<Vec<String>> {
    use tools::native::NativeHost;
    let (_, _, visibility, _) = host.ability_controls(sid).await?;
    let setting = visibility["settings"][rules::ABILITY]["sites"]
        .as_str()
        .unwrap_or("");
    Ok(rules::sites(setting))
}

fn install_url() -> String {
    format!(
        "{}/v3-ui/shared/browser-extension/install.html",
        webagent_webapp::pages::public_origin()
    )
}

/// Run `browser_backend` or `browser_action` for the chatting user.
pub(crate) async fn execute(
    app: &App,
    call: tools::native::Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    use tools::native::NativeHost;
    let host = tools::native_host::Host(app);
    host.authorize(call, name, args, &cancel).await?;
    // The person in the chat, also in a shared conversation: only their browser is used.
    let user = host.actor().to_owned();
    let sites = agent_sites(&host, call.sid).await?;
    let devices = device_list(&app.store, &user).await?;
    let online = devices.iter().any(|d| d["online"] == true);
    if name == "browser_backend" {
        let status = match (devices.is_empty(), online) {
            (true, _) => "not_linked",
            (false, true) => "online",
            (false, false) => "offline",
        };
        let mut output = json!({"status":status,"devices":devices,"sites":sites});
        if status == "not_linked" {
            output["install_url"] = json!(install_url());
            output["next_step"] = json!("Give the user install_url: it explains installing the WebAgent browser extension in Chrome. Once installed it links to their account by itself while they are signed in here.");
        } else if status == "offline" {
            output["next_step"] =
                json!("Ask the user to open Chrome with the WebAgent extension installed.");
        }
        return Ok(json!({"status":"success","output":output}));
    }
    if sites.is_empty() {
        return Ok(
            json!({"status":"denied","output":"No sites are allowed for this agent yet; an operator adds them in the Browser Extension ability settings"}),
        );
    }
    if let Err(error) = rules::check_action(args, &sites) {
        return Ok(json!({"status":"denied","output":error.to_string()}));
    }
    // One document per tool call: a retried call finds its earlier action instead of
    // queuing it again.
    let id = digest(&json!(["browser_action", call.sid, call.pending_id]));
    let key = format!("{CALLS}{user}:{id}");
    if app.store.get(&key).await?.is_none() {
        if !online {
            return Ok(
                json!({"status":"failed","output":"The user's browser extension is not connected; call browser_backend for setup steps"}),
            );
        }
        app.store
            .put(
                &key,
                &json!({"id":id,"session":call.sid,"args":args,"sites":sites,"state":"pending","created":now()}),
            )
            .await?;
    }
    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = withdraw(&app.store, &key).await;
                return Ok(json!({"status":"failed","output":"Browser action cancelled"}));
            }
            _ = tokio::time::sleep(TICK) => {}
        }
        let Some(doc) = app.store.get(&key).await? else {
            return Ok(
                json!({"status":"failed","output":"Browser action was dropped before it ran"}),
            );
        };
        let at = now();
        match doc["state"].as_str() {
            Some("replied") => return Ok(json!({"status":"success","output":doc["output"]})),
            Some("failed") => {
                let error = doc["error"].as_str().unwrap_or("no detail");
                let status = if doc["started"] == true {
                    "uncertain"
                } else {
                    "failed"
                };
                return Ok(json!({"status":status,"output":format!("Browser: {error}")}));
            }
            Some("pending" | "offered") if rules::stale(&doc, at) => {
                let _ = withdraw(&app.store, &key).await;
                return Ok(
                    json!({"status":"failed","output":"The browser extension did not pick up the action; it may have disconnected"}),
                );
            }
            Some("claimed") if rules::stale(&doc, at) => {
                return Ok(
                    json!({"status":"uncertain","output":"The browser took the action but sent no result; read the page before acting again"}),
                );
            }
            _ => {}
        }
    }
}

/// Remove an action no device has taken yet; a taken one stays for its result.
async fn withdraw(store: &Store, key: &str) -> Result<()> {
    if store
        .get(key)
        .await?
        .is_some_and(|c| matches!(c["state"].as_str(), Some("pending" | "offered")))
    {
        store.delete(key).await?;
    }
    Ok(())
}
