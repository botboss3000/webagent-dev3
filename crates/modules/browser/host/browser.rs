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

/// `{id, user, pending, args, sites, blocked_hosts, state, created, ...}` per queued action,
/// under its user.
const CALLS: &str = "browser_call:";
/// `{id, name, created, secret, last_seen}` per linked device, under its user.
const DEVICES: &str = "browser_device:";
/// `{user, device, source, generation}` keyed by a device secret's digest: the extension
/// endpoints' admission. `source` is the sign-in that linked the device: it ends with it.
const SECRETS: &str = "browser_secret:";
/// `{user, expires, source, generation}` per unused link code, keyed by the code's digest.
const LINKS: &str = "browser_link:";
/// `{id, agent, host, state: "asked"|"allowed", at}` per agent and listed site, in the user's
/// own store: a member's agreement that a shared agent may use that site in their browser (D3).
/// Under `administrator_browser:` because storage routes that namespace to each user's own
/// database; a new prefix would land in the shared application database.
const CONSENTS: &str = "administrator_browser:extension_consent:";
/// Unanswered consent requests one user keeps; asking again drops their oldest.
const LIVE_ASKS: usize = 20;
/// The header the extension sends its version in with every request (D4).
const VERSION_HEADER: &str = "x-webagent-extension";
/// Unexpired link codes one user may hold; issuing another drops their oldest.
const LIVE_CODES: usize = 3;
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
/// linking follows whoever is signed in. `linked_user` is only that report. Only a browser
/// sign-in may issue one, and the device it links lasts only as long as that sign-in.
pub(crate) async fn link_code(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Value>,
) -> Result<Json<Value>, ApiError> {
    let user = signed_in(&identity)?;
    if input["linked_user"] == user {
        return Ok(Json(json!({"linked":true})));
    }
    let source = identity["_auth_key"]
        .as_str()
        .filter(|key| key.starts_with("auth:"))
        .ok_or_else(|| {
            ApiError(
                StatusCode::FORBIDDEN,
                "Link the browser extension from a signed-in WebAgent page".into(),
            )
        })?;
    let at = now();
    let _guard = app.gate.lock().await;
    // Expired codes go, and the user keeps at most `LIVE_CODES` codes counting this one.
    let mut own = Vec::new();
    for doc in app.store.list(LINKS).await? {
        let key = doc["key"].as_str().unwrap_or_default().to_owned();
        if doc["expires"].as_u64().unwrap_or(0) < at {
            app.store.delete(&key).await?;
        } else if doc["user"] == user {
            own.push((
                doc["issued"].as_u64().unwrap_or(0),
                doc["expires"].as_u64().unwrap_or(0),
                key,
            ));
        }
    }
    // Oldest first by the user's own issue counter, not the clock: codes issued in the same
    // second share an expiry, and only the counter orders them every time.
    own.sort();
    let issued = own.iter().map(|(n, _, _)| n + 1).max().unwrap_or(0);
    for (_, _, key) in own.iter().take((own.len() + 1).saturating_sub(LIVE_CODES)) {
        app.store.delete(key).await?;
    }
    let code = rules::link_code();
    let key = format!("{LINKS}{}", digest(&json!(code)));
    app.store
        .put(
            &key,
            &json!({"key":key,"user":user,"expires":at + rules::LINK_SECS,"issued":issued,"source":source,
                "generation":identity["generation"]}),
        )
        .await?;
    Ok(Json(json!({"code":code})))
}

/// Trade a link code for this device's own secret, returned once. With `preview: true` the
/// code is only looked at, not used: the extension learns which account it would link to and
/// asks the user first when that is another account than its last one (D1).
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
    if input["preview"] == true {
        if now() > pending["expires"].as_u64().unwrap_or(0) {
            return Err(invalid());
        }
        let user = pending["user"].as_str().ok_or_else(invalid)?;
        let account = app.store.account_by_id(user).await?.ok_or_else(invalid)?;
        return Ok(Json(json!({"user_id":user,"username":account["username"]})));
    }
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
            &json!({"user":user,"device":device,"source":pending["source"],
                "generation":pending["generation"]}),
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

/// Admits a device by its secret while the sign-in that linked it is live: signing out,
/// changing the password or losing the account ends the device, which is then forgotten.
async fn admit(app: &App, headers: &HeaderMap) -> Result<Device, ApiError> {
    let store = &app.store;
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
    let device = Device {
        user: admission["user"].as_str().unwrap_or("").to_owned(),
        id: admission["device"].as_str().unwrap_or("").to_owned(),
        secret_key,
    };
    let live = match admission["source"].as_str() {
        Some(source) => {
            store
                .accounts_repository()
                .session_reference_live(source, &device.user, &admission["generation"], now())
                .await?
        }
        None => false,
    };
    if !live {
        let _guard = app.gate.lock().await;
        forget(store, &device.user, &device.id, &device.secret_key).await?;
        return Err(unauthorized(
            "This browser's sign-in ended; link this browser again",
        ));
    }
    Ok(device)
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
    let device = admit(&app, &headers).await?;
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
    let device = admit(&app, &headers).await?;
    let deadline = now() + rules::POLL_SECS;
    let device_key = format!("{DEVICES}{}:{}", device.user, device.id);
    let version: Option<String> = headers
        .get(VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| v.chars().take(20).collect());
    let minimum = rules::limits::Limits::live().min_extension_version;
    let supported = rules::extension_supported(version.as_deref(), minimum);
    loop {
        {
            let _guard = app.gate.lock().await;
            if let Some(mut record) = app.store.get(&device_key).await? {
                record["last_seen"] = json!(now());
                record["version"] = json!(version);
                app.store.put(&device_key, &record).await?;
            }
            // An outdated extension is seen (so the agent can say "update it") but gets no work.
            if !supported {
                let (major, minor, patch) = minimum;
                return Err(ApiError(
                    StatusCode::UPGRADE_REQUIRED,
                    format!("This WebAgent browser extension is out of date; version {major}.{minor}.{patch} or newer is needed. Download it again from {} and reload it in chrome://extensions.", install_url()),
                ));
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
                return Ok(Json(json!({"call":offer(&call)})));
            }
        }
        if now() >= deadline {
            return Ok(Json(json!({"call":null})));
        }
        tokio::time::sleep(TICK).await;
    }
}

/// What the extension receives for one queued call: the action, the agent's site list and
/// the hosts it must never act on. A call queued before `blocked_hosts` existed gets the app's
/// host now; the extension adds the host it is linked to in any case.
fn offer(call: &Value) -> Value {
    let blocked = match &call["blocked_hosts"] {
        Value::Array(hosts) if !hosts.is_empty() => Value::Array(hosts.clone()),
        _ => json!(blocked_hosts()),
    };
    json!({"id":call["id"],"args":call["args"],"sites":call["sites"],
        "blocked_hosts":blocked,"consent":call["consent"],"session":call["session"]})
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
    let device = admit(&app, &headers).await?;
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
    let device = admit(&app, &headers).await?;
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
    let minimum = rules::limits::Limits::live().min_extension_version;
    Ok(store
        .list(&format!("{DEVICES}{user}:"))
        .await?
        .into_iter()
        .map(|d| {
            let online = rules::recently_seen(d["last_seen"].as_u64(), at);
            let outdated = !rules::extension_supported(d["version"].as_str(), minimum);
            json!({"id":d["id"],"name":d["name"],"created":d["created"],"last_seen":d["last_seen"],
                "online":online,"version":d["version"],"outdated":outdated})
        })
        .collect())
}

fn consent_key(agent: &str, site: &str) -> String {
    format!("{CONSENTS}{}", digest(&json!([agent, site])))
}

/// Whether `user` must agree to each site before `agent` acts in their browser (D3): yes
/// unless they own or operate the agent. Unreadable access fails the call.
async fn needs_consent(store: &Store, user: &str, agent: &str) -> Result<bool> {
    let access = store.live_access(user, agent).await?;
    Ok(access.agent_owner_id != user && !access.agent_administrator)
}

/// The entries of `sites` that `store`'s user agreed `agent` may use.
async fn agreed_sites(store: &Store, agent: &str, sites: &[String]) -> Result<Vec<String>> {
    let mut agreed = Vec::new();
    for site in sites {
        if store
            .get(&consent_key(agent, site))
            .await?
            .is_some_and(|c| c["state"] == "allowed")
        {
            agreed.push(site.clone());
        }
    }
    Ok(agreed)
}

/// Record that `agent` asked to use `site`, for the user to allow on the Linked browsers list.
async fn ask(store: &Store, agent: &str, site: &str) -> Result<()> {
    let key = consent_key(agent, site);
    if store.get(&key).await?.is_some() {
        return Ok(());
    }
    let mut asked: Vec<(u64, String)> = store
        .list(CONSENTS)
        .await?
        .into_iter()
        .filter(|c| c["state"] == "asked")
        .map(|c| {
            let id = c["id"].as_str().unwrap_or_default();
            (c["at"].as_u64().unwrap_or(0), format!("{CONSENTS}{id}"))
        })
        .collect();
    asked.sort();
    for (_, old) in asked
        .iter()
        .take((asked.len() + 1).saturating_sub(LIVE_ASKS))
    {
        store.delete(old).await?;
    }
    store
        .put(
            &key,
            &json!({"id":digest(&json!([agent, site])),"agent":agent,"host":site,"state":"asked","at":now()}),
        )
        .await
}

/// The caller's site agreements for shared agents, allowed and asked (D3).
pub(crate) async fn consents(Extension(app): Extension<App>) -> Result<Json<Value>, ApiError> {
    let mut rows: Vec<Value> = app
        .store
        .list(CONSENTS)
        .await?
        .into_iter()
        .map(|c| json!({"id":c["id"],"agent_id":c["agent"],"host":c["host"],"state":c["state"],"at":c["at"]}))
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row["at"].as_u64().unwrap_or(0)));
    Ok(Json(json!({"consents":rows})))
}

/// The caller's agreement `id`, after checking the sign-in may change settings.
async fn own_consent(app: &App, id: &str) -> Result<(String, Value), ApiError> {
    // The request's identity, set by the sign-in middleware; none means refused.
    if !app
        .identity
        .as_deref()
        .is_some_and(crate::access::identity::Identity::may_change_settings)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Change site agreements from a WebAgent sign-in".into(),
        ));
    }
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Agreement id required".into(),
        ));
    }
    let key = format!("{CONSENTS}{id}");
    let record = app
        .store
        .get(&key)
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Site agreement not found".into()))?;
    Ok((key, record))
}

/// Allow a site a shared agent asked for; it may then act there in the caller's browser.
pub(crate) async fn allow_consent(
    Extension(app): Extension<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (key, mut record) = own_consent(&app, &id).await?;
    record["state"] = json!("allowed");
    record["at"] = json!(now());
    app.store.put(&key, &record).await?;
    Ok(Json(json!({"allowed":true})))
}

/// Take back an agreement, or decline a request: the agent's next action there is refused.
pub(crate) async fn remove_consent(
    Extension(app): Extension<App>,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    let (key, _) = own_consent(&app, &id).await?;
    app.store.delete(&key).await?;
    Ok(Json(json!({"removed":true})))
}

pub(crate) fn owns(name: &str) -> bool {
    rules::TOOLS.contains(&name)
}

/// The hosts the extension never acts on: this app's own host (BROWSER-5).
fn blocked_hosts() -> Vec<String> {
    rules::blocked_hosts(&webagent_webapp::pages::public_origin())
}

/// The agent's allowlisted sites for session `sid`, and the entries refused whatever the
/// setting says (the app itself, loopback, private and Tailscale hosts, IP literals).
async fn agent_sites(
    host: &tools::native_host::Host<'_>,
    sid: &str,
) -> Result<(Vec<String>, Vec<String>)> {
    use tools::native::NativeHost;
    let (_, visibility, _) = host.ability_controls(sid).await?;
    let setting = visibility["settings"][rules::ABILITY]["sites"]
        .as_str()
        .unwrap_or("");
    Ok(rules::extension_sites(setting, &blocked_hosts()))
}

fn install_url() -> String {
    format!(
        "{}/ui/shared/browser-extension/install.html",
        webagent_webapp::pages::public_origin()
    )
}

/// The longest lease one Browser Control call gets; the worker accepts at most ten minutes.
const SERVER_LEASE_SECS: u64 = 600;

/// The record of this session's signed-in browser: its sign-in cookie and session end.
fn signed_key(sid: &str) -> String {
    format!("administrator_browser:{sid}")
}

/// The record of this session's test-account browser (Diagnostics `test_login`): the test
/// account, its sign-in cookie, the run it belongs to and its end. The cookie goes only to
/// the worker's browser; it is never returned to the model, logged or put in a run record.
fn persona_key(sid: &str) -> String {
    format!("test_persona_browser:{sid}")
}

/// Ends this session's test-account sign-in, if any.
async fn end_persona(app: &App, sid: &str) -> Result<()> {
    if let Some(old) = app.store.get(&persona_key(sid)).await? {
        if let Some(token) = old["cookie"].as_str() {
            app.store.accounts_repository().logout(token).await?;
        }
        app.store.delete(&persona_key(sid)).await?;
    }
    Ok(())
}

/// Diagnostics `test_login`: signs this session's Browser Control browser in as `tier`'s
/// test account for run `run_id` of administrator `actor`. The caller (the Diagnostics
/// request handler) has checked administrator authority. The anonymous tier gets a fresh
/// test visitor each time. Returns what the agent may see: never the cookie.
pub(crate) async fn test_persona_sign_in(
    app: &App,
    actor: &str,
    sid: &str,
    run_id: &str,
    tier: &str,
) -> Result<Value> {
    use crate::access::Role;
    let policy = app.store.read_access_policy()?;
    if policy["tiers"].get(tier).is_none() {
        let names: Vec<&String> = policy["tiers"]
            .as_object()
            .map(|tiers| tiers.keys().collect())
            .unwrap_or_default();
        anyhow::bail!("Unknown account tier {tier:?}; tiers: {names:?}");
    }
    let role = Role::for_tier(tier);
    if role == Role::Administrator {
        anyhow::bail!("Test accounts never use the administrator tier");
    }
    // The sign-in ends with this run, so the run must be this session's, running, in the
    // caller's own store (an administrator's own chat).
    let store = app.store.for_user(actor).await?;
    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM run_index WHERE id=? AND session_id=?")
            .bind(run_id)
            .bind(sid)
            .fetch_optional(&store.pool)
            .await?;
    if !status.is_some_and(|status| !webagent_engine::runtime::failure::terminal(&status)) {
        anyhow::bail!("test_login works only during a turn of your own chat");
    }
    let accounts = app.store.accounts_repository();
    let account = if role == Role::Guest {
        accounts
            .create_test_guest_account(crate::access::initial_guest_access())
            .await?
    } else {
        accounts
            .ensure_test_account(tier, "user")
            .await?
            .ok_or_else(|| {
                anyhow!(
                "The reserved name {} belongs to another account; no test account for this tier",
                webagent_accounts::test_username(tier)
            )
            })?
    };
    let user = account["user_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Test account unavailable"))?
        .to_owned();
    let limits = webagent_accounts::TestAccountLimits::live();
    let _guard = app.gate.lock().await;
    end_persona(app, sid).await?;
    let session = accounts
        .issue_test_persona_session(
            &user,
            now(),
            limits.sign_in_minutes * 60,
            webagent_accounts::TestPersonaBinding {
                creator_user_id: actor,
                creator_session_id: sid,
                creator_run_id: run_id,
            },
        )
        .await?;
    app.store
        .put(
            &persona_key(sid),
            &json!({"user_id":user,"cookie":session.token,"expires_at":session.expires,
                "generation":account["generation"],"run_id":run_id,"tier":tier}),
        )
        .await?;
    Ok(json!({
        "signed_in_as": account["username"],
        "user_id": user,
        "tier": tier,
        "expires_at": session.expires,
        "browser": "Browser Control navigations to the app now open it as this test account until this turn ends, the browser leaves the app, or the sign-in expires. The sign-in itself is never shown."
    }))
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
            webagent_accounts::AgentBrowserBinding {
                agent_id: agent,
                allowed_session_ids: &[sid.to_owned()],
            },
        )
        .await?;
    let authority = json!({"session_id":sid,"user_id":user,"cookie":session.token,
        "expires_at":session.expires,"generation":generation,
        "agent_id":agent,"allowed_session_ids":[sid]});
    app.store.put(&signed_key(sid), &authority).await?;
    Ok(authority)
}

/// The authority for this session's private browser. A navigation to this app signs in as the
/// caller when `browser-pages.json` and the agent's setting both allow it; other calls keep a
/// valid signed-in session, renewed near its end. Turning sign-in off signs the next call out.
/// A navigation anywhere else signs out, so the next browser carries the agent's site lists.
async fn server_authority(
    host: &tools::native_host::Host<'_>,
    app: &App,
    sid: &str,
    run_id: &str,
    args: &Value,
) -> Result<Value> {
    use tools::native::NativeHost;
    if let Some(authority) = persona_authority(host, app, sid, run_id, args).await? {
        return Ok(authority);
    }
    let user = host.actor().to_owned();
    let account = app
        .store
        .account_by_id(&user)
        .await?
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let (_, visibility, _) = host.ability_controls(sid).await?;
    let settings = &visibility["settings"][rules::SERVER_ABILITY];
    let origin = webagent_webapp::pages::public_origin();
    // A script signs in, or out, as its first navigate step would on its own.
    let first_navigate = (args["action"] == "script")
        .then(|| {
            args["steps"]
                .as_array()?
                .iter()
                .find(|step| step["action"] == "navigate")
                .cloned()
        })
        .flatten();
    let args = first_navigate.as_ref().unwrap_or(args);
    let navigating = args["action"] == "navigate";
    let agent = host.session_agent_id(sid).await?;
    // Signing in needs the app-wide switch too; the signed-in browser carries its page rules.
    let pages = rules::pages::Pages::live();
    // One sign-in at a time, so parallel calls share a session instead of revoking each other's.
    let _guard = app.gate.lock().await;
    let saved = app.store.get(&signed_key(sid)).await?.filter(|signed| {
        signed["user_id"] == user.as_str()
            && signed["generation"] == account["generation"]
            && signed["expires_at"].as_u64().unwrap_or(0) > now() + 5
    });
    if pages.sign_in_app
        && (rules::signs_in(args, &origin, &settings[rules::SIGN_IN_SETTING])
            || (!navigating && saved.is_some()))
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
        authority["pages"] = pages.authority();
        return Ok(authority);
    }
    if navigating || !pages.sign_in_app {
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

/// The test-account browser's authority while its sign-in belongs to this run, is unexpired
/// and the call stays on the app; otherwise the sign-in ends here and `None` falls through
/// to the ordinary signed-in or signed-out authority.
async fn persona_authority(
    host: &tools::native_host::Host<'_>,
    app: &App,
    sid: &str,
    run_id: &str,
    args: &Value,
) -> Result<Option<Value>> {
    use tools::native::NativeHost;
    let Some(persona) = app.store.get(&persona_key(sid)).await? else {
        return Ok(None);
    };
    let origin = webagent_webapp::pages::public_origin();
    let first_navigate = (args["action"] == "script")
        .then(|| {
            args["steps"]
                .as_array()?
                .iter()
                .find(|step| step["action"] == "navigate")
                .cloned()
        })
        .flatten();
    let args = first_navigate.as_ref().unwrap_or(args);
    let stays = args["action"] != "navigate" || rules::signs_in(args, &origin, &json!(true));
    let live = persona["run_id"] == run_id
        && persona["expires_at"].as_u64().unwrap_or(0) > now() + 5
        && stays;
    if !live {
        let _guard = app.gate.lock().await;
        end_persona(app, sid).await?;
        return Ok(None);
    }
    let end = persona["expires_at"].as_u64().unwrap_or(0);
    // The chat's own caller owns the browser folder; the cookie signs in as the test account.
    Ok(Some(json!({
        "session_id": sid,
        "user_id": host.actor(),
        "signed_in_as": persona["user_id"],
        "agent_id": host.session_agent_id(sid).await?,
        "cookie": persona["cookie"],
        "generation": persona["generation"],
        "expires_at": end.min(now() + SERVER_LEASE_SECS),
        "allowed_session_ids": [sid],
        "pages": rules::pages::Pages::test_persona().authority(),
    })))
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
    label: Option<&str>,
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
    let name = label.map_or_else(
        || format!("screenshot-{}.png", id.get(..8).unwrap_or(id)),
        |label| format!("{label}.png"),
    );
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
    let authority = server_authority(&host, app, call.sid, call.run_id, args).await?;
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
    // A full browser pool queues the call; it waits here rather than costing model steps.
    let max_wait = rules::limits::Limits::live().max_wait_seconds;
    let mut output = rules::queue::wait(
        max_wait,
        || {
            tools::execute_trusted(
                name,
                args,
                call.sid,
                &app.tool_socket,
                Some(trusted.clone()),
                cancel.clone(),
            )
        },
        |seconds| {
            let cancel = cancel.clone();
            async move {
                tokio::select! {
                    () = cancel.cancelled() => false,
                    () = tokio::time::sleep(Duration::from_secs(seconds)) => true,
                }
            }
        },
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
    let entry = |shot: &Value| {
        shot["artifact_id"]
            .as_str()
            .map(|id| (id.to_owned(), shot["label"].as_str().map(str::to_owned)))
    };
    // A failed script still attaches the screenshot of its failing step.
    let ids: Vec<(String, Option<String>)> = if name == "browser_read"
        && args["action"] == "screenshot"
        && output["status"] == "success"
    {
        entry(&output).into_iter().collect()
    } else if (name == "browser_record"
        && args["action"] == "stop"
        && output["status"] == "success")
        || args["action"] == "script"
    {
        output["artifacts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(entry)
            .collect()
    } else {
        Vec::new()
    };
    for (index, (id, label)) in ids.iter().enumerate() {
        match attach_screenshot(app, call, &authority, id, label.as_deref(), cancel.clone()).await {
            Ok(file) if name == "browser_read" && args["action"] == "screenshot" => {
                output["attachment"] = file;
            }
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
    let (sites, refused) = agent_sites(&host, call.sid).await?;
    let blocked = blocked_hosts();
    let devices = device_list(&app.store, &user).await?;
    let online = devices
        .iter()
        .any(|d| d["online"] == true && d["outdated"] != true);
    let outdated = !online && devices.iter().any(|d| d["online"] == true);
    if name == "browser_extension_status" {
        let status = match (devices.is_empty(), online, outdated) {
            (true, _, _) => "not_linked",
            (false, true, _) => "online",
            (false, false, true) => "outdated",
            (false, false, false) => "offline",
        };
        let mut output = json!({"status":status,"devices":devices,"sites":sites});
        if !refused.is_empty() {
            output["refused_sites"] = json!(refused);
            output["refused_reason"] = json!("The extension never acts on this app's own address, loopback, private-network or Tailscale hosts, or IP addresses, even when an operator lists them.");
        }
        if status == "not_linked" {
            output["install_url"] = json!(install_url());
            output["next_step"] = json!("Give the user install_url: it explains installing the WebAgent browser extension in Chrome. Once installed it links to their account by itself while they are signed in here.");
        } else if status == "outdated" {
            output["install_url"] = json!(install_url());
            output["next_step"] = json!("The user's extension is too old to take actions. Ask them to download it again from install_url and reload it in chrome://extensions.");
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
    if let Err(error) = rules::check_action(args, &sites, &blocked) {
        return Ok(json!({"status":"denied","output":error.to_string()}));
    }
    // A member of someone else's agent agrees to each listed site first (D3); the extension
    // checks the same agreed entries against the tab at every action.
    let agent = host.session_agent_id(call.sid).await?;
    let consent = if needs_consent(&app.store, &user, &agent).await? {
        let store = app.store.for_user(&user).await?;
        let agreed = agreed_sites(&store, &agent, &sites).await?;
        if let Err(entry) = rules::consent_refusal(args, &sites, &agreed) {
            let place = format!(
                "Manage account → Linked browsers ({}/app?tab=account)",
                webagent_webapp::pages::public_origin()
            );
            let output = if entry.is_empty() {
                format!("The user has not agreed to any site for this agent in their own browser yet. Use open on a listed site first; then ask them to allow it under {place}.")
            } else {
                ask(&store, &agent, entry).await?;
                format!("The user has not agreed to let this agent use {entry} in their own browser. This agent belongs to someone else, so they agree once per site: ask them to allow {entry} under {place}, then try again.")
            };
            return Ok(json!({"status":"denied","output":output}));
        }
        Some(agreed)
    } else {
        None
    };
    // One document per tool call: a retried call finds its earlier action instead of
    // queuing it again.
    let id = digest(&json!([
        "browser_extension_action",
        call.sid,
        call.pending_id
    ]));
    let key = format!("{CALLS}{user}:{id}");
    if app.store.get(&key).await?.is_none() {
        if outdated {
            return Ok(
                json!({"status":"failed","output":format!("The user's browser extension is out of date and refuses actions; ask them to download it again from {} and reload it in chrome://extensions", install_url())}),
            );
        }
        if !online {
            return Ok(
                json!({"status":"failed","output":"The user's browser extension is not connected; call browser_extension_status for setup steps"}),
            );
        }
        app.store
            .put(
                &key,
                &json!({"id":id,"session":call.sid,"args":args,"sites":sites,
                    "blocked_hosts":blocked,"consent":consent,"state":"pending","created":now()}),
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
            // Page text, titles and addresses come from outside: labelled like Browser
            // Control's results.
            Some("replied") => {
                return Ok(json!({"status":"success","output":doc["output"],
                    "source_kind":"untrusted_browser_observation"}))
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A browser sign-in for the fixture administrator and the identity its requests carry.
    async fn sign_in(app: &App) -> (String, Value) {
        let key = format!("auth:{}", digest(&json!(Uuid::new_v4().to_string())));
        app.store
            .put(
                &key,
                &json!({"generation":"fixture-generation","expires":now() + 3600}),
            )
            .await
            .unwrap();
        let identity =
            json!({"_user_id":"admin","generation":"fixture-generation","_auth_key":key});
        (key, identity)
    }

    async fn linked_device(app: &App, identity: &Value) -> HeaderMap {
        let code = link_code(
            State(app.clone()),
            Extension(identity.clone()),
            Json(json!({})),
        )
        .await
        .unwrap()
        .0;
        let linked = link(State(app.clone()), Json(json!({"code":code["code"]})))
            .await
            .unwrap()
            .0;
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            format!("Bearer {}", linked["secret"].as_str().unwrap())
                .parse()
                .unwrap(),
        );
        headers.insert(VERSION_HEADER, "0.3.0".parse().unwrap());
        headers
    }

    /// One queued read for `admin`, so `next` answers at once instead of long-polling.
    async fn queue_read(app: &App, n: u64) {
        let id = format!("{n:064x}");
        app.store
            .put(
                &format!("{CALLS}admin:{id}"),
                &json!({"id":id,"session":"s","args":{"action":"read"},"sites":["example.com"],
                    "state":"pending","created":now()}),
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn outdated_extensions_are_seen_but_get_no_work() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (_, identity) = sign_in(&app).await;
        let mut device = linked_device(&app, &identity).await;
        queue_read(&app, 1).await;
        // A 0.1.0 install sends no version; one below the minimum is refused the same way.
        for old in [None, Some("0.1.0")] {
            match old {
                None => device.remove(VERSION_HEADER),
                Some(v) => device.insert(VERSION_HEADER, v.parse().unwrap()),
            };
            let refused = next(State(app.clone()), device.clone())
                .await
                .err()
                .unwrap();
            assert_eq!(refused.0, StatusCode::UPGRADE_REQUIRED);
            assert!(refused.1.contains("install.html"), "{}", refused.1);
        }
        let listed = device_list(&app.store, "admin").await.unwrap();
        assert_eq!(listed[0]["online"], true);
        assert_eq!(listed[0]["outdated"], true);
        // The call stayed queued, and an up-to-date extension gets it.
        device.insert(VERSION_HEADER, "0.3.0".parse().unwrap());
        let offered = next(State(app.clone()), device).await.unwrap().0;
        assert_eq!(offered["call"]["args"]["action"], "read");
        assert_eq!(
            device_list(&app.store, "admin").await.unwrap()[0]["outdated"],
            false
        );
    }

    #[tokio::test]
    async fn a_link_preview_names_the_account_and_leaves_the_code_usable() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (_, identity) = sign_in(&app).await;
        let code = link_code(State(app.clone()), Extension(identity), Json(json!({})))
            .await
            .unwrap()
            .0["code"]
            .clone();
        let preview = link(
            State(app.clone()),
            Json(json!({"code":code,"preview":true})),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(preview["user_id"], "admin");
        assert!(preview["username"].is_string());
        assert!(preview.get("secret").is_none());
        // Looking did not use the code: it still links once, then never again.
        assert!(link(State(app.clone()), Json(json!({"code":code})))
            .await
            .is_ok());
        let used = link(
            State(app.clone()),
            Json(json!({"code":code,"preview":true})),
        )
        .await
        .err()
        .unwrap();
        assert_eq!(used.0, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn members_agree_to_each_site_and_only_they_can_change_it() {
        use axum::{body::Body, http::Request};
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let account = app
            .store
            .create_user_account("ext_member", "fixture-only-password-hash")
            .await
            .unwrap();
        let member = account["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(&member, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                &member,
                "shared_default",
                crate::access::Relationship::Member,
                0,
            )
            .await
            .unwrap();
        // The owner acting on their own agent needs no agreement; a member does.
        assert!(!needs_consent(&app.store, "admin", "shared_default")
            .await
            .unwrap());
        assert!(needs_consent(&app.store, &member, "shared_default")
            .await
            .unwrap());
        let sites = vec!["example.com".to_owned(), "crm.test".to_owned()];
        let store = app.store.for_user(&member).await.unwrap();
        ask(&store, "shared_default", "example.com").await.unwrap();
        assert!(agreed_sites(&store, "shared_default", &sites)
            .await
            .unwrap()
            .is_empty());
        let mut tokens = Vec::new();
        for (user, csrf) in [("admin", "a"), (member.as_str(), "m")] {
            let token = Uuid::new_v4().to_string();
            let generation =
                app.store.account_by_id(user).await.unwrap().unwrap()["generation"].clone();
            app.store
                .put(
                    &format!("auth:{}", digest(&json!(token))),
                    &json!({"user_id":user,"generation":generation,"csrf":csrf,"expires":now()+3600}),
                )
                .await
                .unwrap();
            tokens.push((token, csrf));
        }
        let call = |who: usize, method: &str, path: String, csrf: bool| {
            let (token, token_csrf) = &tokens[who];
            let mut request = Request::builder()
                .method(method)
                .uri(path)
                .header("cookie", format!("v3_session={token}"));
            if csrf {
                request = request.header("x-csrf-token", *token_csrf);
            }
            let app = app.clone();
            let request = request.body(Body::empty()).unwrap();
            async move {
                let response = crate::api::router(app).oneshot(request).await.unwrap();
                let status = response.status();
                let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
                )
            }
        };
        let (status, listed) = call(1, "GET", "/api/v3/browser/consents".into(), false).await;
        assert_eq!(status, StatusCode::OK);
        let row = &listed["consents"][0];
        assert_eq!(
            (row["host"].as_str(), row["state"].as_str()),
            (Some("example.com"), Some("asked"))
        );
        let id = row["id"].as_str().unwrap().to_owned();
        // Another account sees none of it and cannot allow it.
        let (_, theirs) = call(0, "GET", "/api/v3/browser/consents".into(), false).await;
        assert_eq!(theirs["consents"], json!([]));
        let path = format!("/api/v3/browser/consents/{id}");
        assert_eq!(
            call(0, "POST", path.clone(), true).await.0,
            StatusCode::NOT_FOUND
        );
        // Without the CSRF header the member's own change is refused; with it, it counts.
        assert_eq!(
            call(1, "POST", path.clone(), false).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(call(1, "POST", path.clone(), true).await.0, StatusCode::OK);
        assert_eq!(
            agreed_sites(&store, "shared_default", &sites)
                .await
                .unwrap(),
            ["example.com"]
        );
        // Taking it back refuses the agent's next action there again.
        assert_eq!(
            call(1, "DELETE", path.clone(), true).await.0,
            StatusCode::OK
        );
        assert!(agreed_sites(&store, "shared_default", &sites)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(call(1, "DELETE", path, true).await.0, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn unanswered_site_requests_stay_few() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for n in 0..LIVE_ASKS + 5 {
            ask(&app.store, "shared_default", &format!("site{n}.test"))
                .await
                .unwrap();
        }
        let kept = app.store.list(CONSENTS).await.unwrap();
        assert_eq!(kept.len(), LIVE_ASKS);
    }

    #[tokio::test]
    async fn a_device_lasts_only_as_long_as_the_sign_in_that_linked_it() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (key, identity) = sign_in(&app).await;
        let device = linked_device(&app, &identity).await;
        assert!(admit(&app, &device).await.is_ok());
        assert_eq!(device_list(&app.store, "admin").await.unwrap().len(), 1);
        // Signing out deletes the sign-in: the device is refused and forgotten.
        app.store.delete(&key).await.unwrap();
        let refused = admit(&app, &device).await.err().unwrap();
        assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
        assert!(device_list(&app.store, "admin").await.unwrap().is_empty());
        // A password change moves the account to a new generation.
        let (key, identity) = sign_in(&app).await;
        let device = linked_device(&app, &identity).await;
        assert!(admit(&app, &device).await.is_ok());
        app.store
            .put(
                &key,
                &json!({"generation":"before-password-change","expires":now() + 3600}),
            )
            .await
            .unwrap();
        assert!(admit(&app, &device).await.is_err());
    }

    #[tokio::test]
    async fn queued_calls_tell_the_extension_which_hosts_it_never_acts_on() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (_, identity) = sign_in(&app).await;
        let device = linked_device(&app, &identity).await;
        let app_host = blocked_hosts();
        assert_eq!(app_host.len(), 1);
        // One call queued with its blocked hosts, one queued before they existed.
        for (n, blocked) in [(1u64, json!(["app.example"])), (2, Value::Null)] {
            let id = format!("{n:064x}");
            app.store
                .put(
                    &format!("{CALLS}admin:{id}"),
                    &json!({"id":id,"session":"s","args":{"action":"read"},"sites":["example.com"],
                        "blocked_hosts":blocked,"state":"pending","created":now() + n}),
                )
                .await
                .unwrap();
            let offered = next(State(app.clone()), device.clone()).await.unwrap().0;
            let call = &offered["call"];
            assert_eq!(call["sites"], json!(["example.com"]));
            let expected = if blocked.is_null() {
                json!(&app_host)
            } else {
                blocked
            };
            assert_eq!(call["blocked_hosts"], expected, "{call}");
        }
    }

    #[tokio::test]
    async fn only_a_browser_sign_in_gets_link_codes_and_they_stay_few() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        // A chat widget's credential is not a browser sign-in.
        let widget = json!({"_user_id":"admin","_auth_key":"embed:credential:fixture"});
        let refused = link_code(State(app.clone()), Extension(widget), Json(json!({})))
            .await
            .err()
            .unwrap();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        app.store
            .put(
                &format!("{LINKS}expired"),
                &json!({"key":format!("{LINKS}expired"),"user":"someone","expires":1}),
            )
            .await
            .unwrap();
        let (_, identity) = sign_in(&app).await;
        let mut codes = Vec::new();
        for _ in 0..5 {
            let issued = link_code(
                State(app.clone()),
                Extension(identity.clone()),
                Json(json!({})),
            )
            .await
            .unwrap()
            .0;
            codes.push(issued["code"].as_str().unwrap().to_owned());
        }
        let live = app.store.list(LINKS).await.unwrap();
        assert_eq!(live.len(), LIVE_CODES);
        assert!(live.iter().all(|doc| doc["user"] == "admin"));
        // The newest code still links; the oldest was dropped.
        assert!(link(State(app.clone()), Json(json!({"code":codes[4]})))
            .await
            .is_ok());
        assert!(link(State(app.clone()), Json(json!({"code":codes[0]})))
            .await
            .is_err());
    }
}
