//! Root adapter for the browser abilities. Browser Extension: device linking, the extension's
//! action queue and the `browser_extension_status`/`browser_extension_action` tools. Browser
//! Control: authority for the worker's private browser, the user's vault values and saved site
//! sign-ins for it, and reply attachments for its screenshots. Rules live in `webagent_browser`.
//!
//! The API (extension endpoints) and the loop (tools) are separate processes; both read the
//! shared application store, the same way the harness bridge does. A device belongs to one
//! WebAgent account and only ever receives that account's actions.
use crate::*;
use axum::{http::HeaderMap, Extension};
use tokio_util::sync::CancellationToken;
use webagent_accounts_host::AccountsStore;
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

/// The longest lease one Browser Control call gets; the worker accepts at most ten minutes.
const SERVER_LEASE_SECS: u64 = 600;

/// The record of this session's signed-in browser: its sign-in cookie and session end.
fn signed_key(sid: &str) -> String {
    format!("administrator_browser:{sid}")
}

/// A new signed-in browser session for the caller, signing out any earlier one.
async fn sign_in(
    host: &tools::native_host::Host<'_>,
    app: &App,
    sid: &str,
    account: &Value,
    agent: &str,
) -> Result<Value> {
    use tools::native::NativeHost;
    let user = host.actor();
    if let Some(token) = app
        .store
        .get(&signed_key(sid))
        .await?
        .and_then(|old| old["cookie"].as_str().map(str::to_owned))
    {
        app.store.accounts_repository().logout(&token).await?;
    }
    let generation = account["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let session = app
        .store
        .accounts_repository()
        .issue_trusted_browser_session(
            user,
            generation,
            now(),
            "Browser Control signed-in browser",
            rules::SIGN_IN_SECS,
        )
        .await?;
    let authority = json!({"session_id":sid,"user_id":user,"cookie":session.token,
        "expires_at":session.expires,"generation":generation,
        "agent_id":agent,"allowed_session_ids":[sid]});
    app.store.put(&signed_key(sid), &authority).await?;
    Ok(authority)
}

/// The authority for this session's private browser. A navigation to this app signs in as the
/// caller (unless turned off); other calls keep a valid signed-in session, renewed near its end.
/// A navigation anywhere else signs out, so the next browser carries the agent's site lists.
async fn server_authority(
    host: &tools::native_host::Host<'_>,
    app: &App,
    sid: &str,
    args: &Value,
) -> Result<Value> {
    use tools::native::NativeHost;
    let user = host.actor().to_owned();
    let account = app
        .store
        .account_by_id(&user)
        .await?
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let (_, _, visibility, _) = host.ability_controls(sid).await?;
    let settings = &visibility["settings"][rules::SERVER_ABILITY];
    let origin = webagent_webapp::pages::public_origin();
    let navigating = args["action"] == "navigate";
    let agent = host.session_agent_id(sid).await?;
    // One sign-in at a time, so parallel calls share a session instead of revoking each other's.
    let _guard = app.gate.lock().await;
    let saved = app.store.get(&signed_key(sid)).await?.filter(|signed| {
        signed["user_id"] == user.as_str()
            && signed["generation"] == account["generation"]
            && signed["expires_at"].as_u64().unwrap_or(0) > now() + 5
    });
    if rules::signs_in(args, &origin, &settings[rules::SIGN_IN_SETTING])
        || (!navigating && saved.is_some())
    {
        let mut authority = match saved {
            Some(signed)
                if signed["expires_at"].as_u64().unwrap_or(0) > now() + rules::RENEW_SECS =>
            {
                signed
            }
            _ => sign_in(host, app, sid, &account, &agent).await?,
        };
        let end = authority["expires_at"].as_u64().unwrap_or(0);
        authority["expires_at"] = json!(end.min(now() + SERVER_LEASE_SECS));
        return Ok(authority);
    }
    if navigating {
        if let Some(old) = app.store.get(&signed_key(sid)).await? {
            if let Some(token) = old["cookie"].as_str() {
                app.store.accounts_repository().logout(token).await?;
            }
            app.store.delete(&signed_key(sid)).await?;
        }
    }
    let list = |key: &str| rules::server_sites(settings[key].as_str().unwrap_or(""));
    Ok(json!({
        "session_id": sid,
        "user_id": user,
        "agent_id": agent,
        "generation": account["generation"],
        "expires_at": now() + SERVER_LEASE_SECS,
        "allowed_session_ids": [sid],
        "sites": {"allow": list("allow_sites"), "block": list("block_sites")},
    }))
}

/// A saved site sign-in's key in its user's store.
fn site_key(origin: &str) -> String {
    format!("{}{}", rules::SITE_PREFIX, digest(&json!(origin)))
}

/// The caller's vault entries `names`, as the worker types them: `{name: {value, origin}}`.
/// With `origin`, only entries for that site; entries without a site are never typed.
async fn vault_values(store: &Store, names: &[String], origin: Option<&str>) -> Result<Value> {
    let mut values = serde_json::Map::new();
    for name in names
        .iter()
        .filter(|name| webagent_native_tools::vault::valid_name(name))
    {
        let Some(entry) = store.get(&format!("core_vault:{name}")).await? else {
            continue;
        };
        let (Some(value), Some(site)) = (entry["value"].as_str(), entry["origin"].as_str()) else {
            continue;
        };
        if origin.is_none_or(|origin| origin == site) {
            values.insert(name.clone(), json!({"value":value,"origin":site}));
        }
    }
    Ok(Value::Object(values))
}

/// The caller's saved site sign-ins, without their cookies.
pub(crate) async fn sites(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
) -> Result<Json<Value>, ApiError> {
    let store = app.store.for_user(signed_in(&identity)?).await?;
    let sites: Vec<Value> = store
        .list(rules::SITE_PREFIX)
        .await?
        .iter()
        .filter_map(|site| {
            let origin = site["origin"].as_str()?;
            Some(
                json!({"id":digest(&json!(origin)),"origin":origin,"saved_at":site["saved_at"],
                "last_used":site["last_used"],"status":site["status"],"vault":site["vault"],
                "steps":site["steps"].as_array().map_or(0, Vec::len)}),
            )
        })
        .collect();
    Ok(Json(json!({"sites":sites})))
}

/// Delete one of the caller's saved site sign-ins; its vault entries stay.
pub(crate) async fn forget_site(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "Site id required".into()));
    }
    let store = app.store.for_user(signed_in(&identity)?).await?;
    let key = format!("{}{id}", rules::SITE_PREFIX);
    if store.get(&key).await?.is_none() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "Saved sign-in not found".into(),
        ));
    }
    store.delete(&key).await?;
    Ok(Json(json!({"removed":true})))
}

/// Keep the sign-in the worker returned from an approved `save_login`, in the caller's store.
async fn save_site(store: &Store, args: &Value, login: &Value) -> Result<Value> {
    let origin = login["origin"].as_str().unwrap_or("");
    if args["origin"] != origin || rules::site_origin(origin).as_deref() != Some(origin) {
        return Ok(
            json!({"status":"error","error_kind":"browser_invalid_request",
            "output":"The saved sign-in does not match the site asked for"}),
        );
    }
    let record = json!({"origin":origin,"cookies":login["cookies"],"steps":login["steps"],
        "login_url":login["login_url"],"vault":rules::step_vaults(&login["steps"]),
        "saved_at":now(),"last_used":now(),"status":"signed_in"});
    if serde_json::to_vec(&record)?.len() > rules::SITE_MAX_BYTES {
        return Ok(
            json!({"status":"error","error_kind":"browser_invalid_request",
            "output":"This site's sign-in is too large to save"}),
        );
    }
    store.put(&site_key(origin), &record).await?;
    Ok(Value::Null)
}

/// Save one browser screenshot as an attachment of this run's reply.
async fn attach_screenshot(
    app: &App,
    call: tools::native::Call<'_>,
    authority: &Value,
    id: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let image = tools::execute_trusted(
        "administrator_artifact_get",
        &json!({"artifact_id": id}),
        call.sid,
        &app.tool_socket,
        Some(authority.clone()),
        cancel,
    )
    .await?;
    let data = image["png_base64"]
        .as_str()
        .ok_or_else(|| anyhow!("Screenshot bytes unavailable"))?;
    let name = format!("screenshot-{}.png", id.get(..8).unwrap_or(id));
    crate::api::uploads::publish_reply_file(
        app,
        call.sid,
        call.run_id,
        id,
        &name,
        "image/png",
        data,
    )
    .await
    .map_err(|error| anyhow!(error.1))
}

/// Run a Browser Control tool in the worker's private browser. Screenshots, and recorded
/// frames when recording stops, are attached to the reply so the user sees them after the
/// browser closes; a failed attachment keeps the result and says why.
async fn execute_server(
    app: &App,
    call: tools::native::Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    use tools::native::NativeHost;
    let host = tools::native_host::Host(app);
    host.authorize(call, name, args, &cancel).await?;
    let authority = server_authority(&host, app, call.sid, args).await?;
    // Signed out, the caller's own vault values and saved sign-in go to the worker for this
    // call only; the model never sees them.
    let store = app.store.for_user(host.actor()).await?;
    let mut trusted = authority.clone();
    let mut site = None;
    if authority["cookie"].is_null() {
        if let Some(vault) = args["vault"].as_str().filter(|_| name == "browser_input") {
            trusted["secrets"] = vault_values(&store, &[vault.to_owned()], None).await?;
        } else if name == "browser_read" && args["action"] == "navigate" {
            let origin = args["url"].as_str().and_then(rules::site_origin);
            if let Some(origin) = origin {
                if let Some(record) = store.get(&site_key(&origin)).await? {
                    let names = rules::step_vaults(&record["steps"]);
                    trusted["secrets"] = vault_values(&store, &names, Some(&origin)).await?;
                    trusted["site"] = json!({"origin":origin,"cookies":record["cookies"],
                        "steps":record["steps"],"login_url":record["login_url"]});
                    site = Some((origin, record));
                }
            }
        }
    }
    let mut output = tools::execute_trusted(
        name,
        args,
        call.sid,
        &app.tool_socket,
        Some(trusted),
        cancel.clone(),
    )
    .await?;
    if let Some(login) = output.as_object_mut().and_then(|o| o.remove("site_login")) {
        let refused = save_site(&store, args, &login).await?;
        if refused.is_null() {
            output["saved"] = json!(true);
        } else {
            output = refused;
        }
    }
    if let Some((origin, mut record)) = site {
        if let Some(cookies) = output
            .as_object_mut()
            .and_then(|o| o.remove("site_cookies"))
        {
            record["cookies"] = cookies;
        }
        record["status"] = match (output["site_login"].as_str(), output["error_kind"].as_str()) {
            (Some("expired"), _) => json!("expired"),
            (Some(_), _) => json!("signed_in"),
            (_, Some("signin_failed")) => json!("failed"),
            _ => record["status"].clone(),
        };
        record["last_used"] = json!(now());
        store.put(&site_key(&origin), &record).await?;
    }
    let ids: Vec<String> = if output["status"] != "success" {
        Vec::new()
    } else if name == "browser_read" && args["action"] == "screenshot" {
        output["artifact_id"]
            .as_str()
            .map(str::to_owned)
            .into_iter()
            .collect()
    } else if name == "browser_record" && args["action"] == "stop" {
        output["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|frame| frame["artifact_id"].as_str().map(str::to_owned))
            .collect()
    } else {
        Vec::new()
    };
    for (index, id) in ids.iter().enumerate() {
        match attach_screenshot(app, call, &authority, id, cancel.clone()).await {
            Ok(file) if name == "browser_read" => output["attachment"] = file,
            Ok(file) => output["artifacts"][index]["attachment"] = file,
            Err(error) => output["attachment_error"] = json!(error.to_string()),
        }
    }
    Ok(output)
}

/// Run a browser tool: Browser Control's in the worker, the extension's for the chatting user.
pub(crate) async fn execute(
    app: &App,
    call: tools::native::Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    use tools::native::NativeHost;
    if rules::SERVER_TOOLS.contains(&name) {
        return execute_server(app, call, name, args, cancel).await;
    }
    let host = tools::native_host::Host(app);
    host.authorize(call, name, args, &cancel).await?;
    // The person in the chat, also in a shared conversation: only their browser is used.
    let user = host.actor().to_owned();
    let sites = agent_sites(&host, call.sid).await?;
    let devices = device_list(&app.store, &user).await?;
    let online = devices.iter().any(|d| d["online"] == true);
    if name == "browser_extension_status" {
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
    let id = digest(&json!([
        "browser_extension_action",
        call.sid,
        call.pending_id
    ]));
    let key = format!("{CALLS}{user}:{id}");
    if app.store.get(&key).await?.is_none() {
        if !online {
            return Ok(
                json!({"status":"failed","output":"The user's browser extension is not connected; call browser_extension_status for setup steps"}),
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
