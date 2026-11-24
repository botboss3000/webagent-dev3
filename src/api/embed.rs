//! Embed credentials authorize only the selected agent's caller-owned chat.
//! Host-page configuration never changes agent, account, or tool authority.
use super::{
    access, admin_agent_view, bad, conflict, digest, json, key, now, registration, required,
    ui_config_value, uuid, Api, ApiError, App, Deserialize, HashMap, HeaderMap, HeaderValue, Json,
    Mutex, Path, PathBuf, Query, Request, Response, Sha256, State, StatusCode, Value,
};
use axum::response::IntoResponse;
use axum::Extension;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Serialize;
use sha2::Digest;
use std::sync::Arc;
use webagent_access::widget_trial::{self, WidgetState, Widgets};
use webagent_accounts_host::AccountsStore;
#[derive(Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct Config {
    enabled: bool,
    allowed_origins: Vec<String>,
    #[serde(default)]
    guest_access: bool,
}
fn denied() -> ApiError {
    ApiError(StatusCode::FORBIDDEN, "Embedded chat unavailable".into())
}
/// A route that is not on the widget allowlist (`credential_routes.rs`).
fn route_refused() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Not available in the embedded chat".into(),
    )
}
fn unauthorized() -> ApiError {
    ApiError(
        StatusCode::UNAUTHORIZED,
        "Embedded chat authentication required".into(),
    )
}
/// What a visitor sees while the agent owner's widgets are paused (trial over, or the plan has
/// no widgets): settings and chats are kept, so it is "unavailable", not "denied".
fn paused() -> ApiError {
    ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "This chat is paused and not available right now. Please check back later.".into(),
    )
}
/// The `account_due` kind whose time is an owner's next widget trial notice.
const WIDGET_TRIAL_DUE: &str = "widget_trial";
/// How often the API looks for widget trial notices that are due.
const WIDGET_TRIAL_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a notice that could not be written waits before it is tried again (seconds).
const WIDGET_TRIAL_RETRY: u64 = 3600;
/// The owner's widget state from their effective `tier` (`None`: unreadable, never serves).
/// Only a trial tier reads the trial start: one plain indexed read, no decryption.
async fn widget_state(
    app: &App,
    owner: &str,
    tier: Option<&access::Tier>,
) -> anyhow::Result<WidgetState> {
    let Some(tier) = tier else {
        return Ok(WidgetState::NotIncluded);
    };
    let started = if tier.owner_limits.widgets() == Widgets::Trial {
        app.store
            .accounts_repository()
            .widget_trial_started(owner)
            .await?
    } else {
        None
    };
    Ok(WidgetState::of(&tier.owner_limits, started, now()))
}
/// The agent's owner and their effective tier, for widget routes without a signed-in caller.
/// An unknown or unreadable tier gives `None`, which never serves (fail closed).
async fn agent_owner_tier(
    app: &App,
    agent: &str,
) -> anyhow::Result<(String, Option<access::Tier>)> {
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
        .bind(agent)
        .fetch_one(&app.store.app_pool)
        .await?;
    let tier = match app.store.account_by_id(&owner).await? {
        Some(account) => {
            let tier_id = access::Principal::from_account(&account)?.tier_id;
            app.store.entitlement_tier(&tier_id).await.ok().flatten()
        }
        None => None,
    };
    Ok((owner, tier))
}
/// Refuses a public widget route while the agent owner's widgets are paused.
async fn widget_serves(app: &App, agent: &str) -> std::result::Result<(), ApiError> {
    let (owner, tier) = agent_owner_tier(app, agent).await?;
    if widget_state(app, &owner, tier.as_ref()).await?.serves() {
        Ok(())
    } else {
        Err(paused())
    }
}
/// Whether the widgets of `access`'s agent owner serve now, from the owner tier that access
/// check already read.
async fn widget_serves_for(app: &App, access: &access::authority::LiveAccess) -> bool {
    widget_state(app, &access.agent_owner_id, access.owner_tier.as_ref())
        .await
        .is_ok_and(WidgetState::serves)
}
fn origin_valid(origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    matches!(url.scheme(), "https" | "http")
        && url.host_str().is_some_and(|host| !host.contains('*'))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.origin().ascii_serialization() == origin
        && (url.scheme() == "https" || matches!(url.host_str(), Some("localhost" | "127.0.0.1")))
}
async fn settings(app: &App, id: &str) -> std::result::Result<Value, ApiError> {
    let scoped = admin_agent_view(app, id).await?;
    Ok(scoped
        .store
        .get(&format!("agent:{id}:embed"))
        .await?
        .unwrap_or_else(|| json!({"revision":0,"config":{"enabled":false,"guest_access":false,"allowed_origins":[]}})))
}
async fn allowed(app: &App, id: &str, origin: &str) -> std::result::Result<Value, ApiError> {
    if !origin_valid(origin) {
        return Err(denied());
    }
    let value = settings(app, id).await?;
    if value["config"]["enabled"] != true
        || !value["config"]["allowed_origins"]
            .as_array()
            .is_some_and(|items| items.iter().any(|o| o == origin))
    {
        return Err(denied());
    }
    Ok(value)
}
async fn authority(app: &App, identity: &Value, id: &str) -> std::result::Result<(), ApiError> {
    let user = identity["_user_id"].as_str().ok_or_else(denied)?;
    if app.store.agent_authority(user, id).await?["can_manage"] != true {
        return Err(denied());
    }
    Ok(())
}
pub(super) async fn config_get(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    authority(&app, &identity, &id).await?;
    let mut value = settings(&app, &id).await?;
    let caller = identity["_user_id"].as_str().unwrap_or("");
    value["widget_plan"] = owner_widget_state(&app, caller, &id).await?.2.view();
    Ok(Json(value))
}
/// The agent's owner, their tier's limits and their widget state, read through `caller`'s
/// access to the agent.
async fn owner_widget_state(
    app: &App,
    caller: &str,
    agent: &str,
) -> std::result::Result<(String, access::OwnerLimits, WidgetState), ApiError> {
    let access = app.store.live_access(caller, agent).await?;
    let state = widget_state(app, &access.agent_owner_id, access.owner_tier.as_ref()).await?;
    let limits = access
        .owner_tier
        .map(|tier| tier.owner_limits)
        .unwrap_or_default();
    Ok((access.agent_owner_id, limits, state))
}
/// Starts the owner's one widget trial now, unless it already started: the `account_due` row
/// for its notices first (only brought earlier), then the account's start, then the row set
/// from the stored start. A failed or racing write costs at worst an early visit. Returns the
/// stored start.
async fn start_widget_trial(
    app: &App,
    owner: &str,
    limits: &access::OwnerLimits,
) -> anyhow::Result<u64> {
    let pool = &app.store.app_pool;
    let at = now();
    if let (_, Some(first)) = widget_trial::due_visit(limits, Some(at), at) {
        webagent_engine::due::earlier(pool, owner, WIDGET_TRIAL_DUE, first).await?;
    }
    let started = app
        .store
        .accounts_repository()
        .start_widget_trial(owner, at)
        .await?;
    let next = widget_trial::due_visit(limits, Some(started), now()).1;
    webagent_engine::due::set(pool, owner, WIDGET_TRIAL_DUE, next).await?;
    Ok(started)
}
pub(super) async fn config_put(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    authority(&app, &identity, &id).await?;
    let config: Config = serde_json::from_value(body["config"].clone())
        .map_err(|_| bad("Invalid embed configuration"))?;
    if config.allowed_origins.len() > 32
        || config
            .allowed_origins
            .iter()
            .any(|o| o.len() > 256 || !origin_valid(o))
    {
        return Err(bad(
            "Use at most 32 exact HTTPS origins (localhost HTTP allowed)",
        ));
    }
    let expected = body["expected_revision"]
        .as_u64()
        .ok_or_else(|| bad("Expected revision required"))?;
    let _guard = app.gate.lock().await;
    let old = settings(&app, &id).await?;
    if old["revision"] != expected {
        return Err(conflict("Embed configuration changed"));
    }
    // Widgets are a plan value of the agent's owner: turning one on starts a trial not yet
    // started and is refused once the trial is over; a widget already on keeps its settings.
    let caller = identity["_user_id"].as_str().unwrap_or("");
    let (owner, limits, mut state) = owner_widget_state(&app, caller, &id).await?;
    if config.enabled {
        if let WidgetState::TrialNotStarted { .. } = state {
            let started = start_widget_trial(&app, &owner, &limits).await?;
            state = WidgetState::of(&limits, Some(started), now());
        } else if !state.may_turn_on() && old["config"]["enabled"] != true {
            return Err(conflict(match state {
                WidgetState::TrialEnded { .. } => {
                    "The agent owner's widget trial has ended; widgets work again on a plan that includes them"
                }
                _ => "The agent owner's plan does not include website chat widgets",
            }));
        }
    }
    let mut value = json!({"revision":expected.checked_add(1).ok_or_else(||bad("Revision exhausted"))?,"config":config});
    admin_agent_view(&app, &id)
        .await?
        .store
        .put(&format!("agent:{id}:embed"), &value)
        .await?;
    value["widget_plan"] = state.view();
    Ok(Json(value))
}
pub(super) async fn descriptor(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(args): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    let origin = args.get("origin").map(String::as_str).unwrap_or("");
    let config = allowed(&app, &id, origin).await?;
    if headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|o| o != origin)
    {
        return Err(denied());
    }
    widget_serves(&app, &id).await?;
    // The embed's agent overlay applies exactly as in the authenticated UI response.
    let mut ui = ui_config_value(&app).await?;
    crate::chat_config::apply_to_ui_config(&app.store, &id, &mut ui).await?;
    let chat = ui["chat_ui"].clone();
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM agents WHERE id=?")
        .bind(&id)
        .fetch_optional(&admin_agent_view(&app, &id).await?.store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    // Public descriptor deliberately exposes only presentation profiles.
    let mut response=Json(json!({"agent":{"id":id,"name":name},"config":config["config"],"chat_ui":{"chat_common":chat["chat_common"],"chat_widget":chat["chat_widget"]}})).into_response();
    response.headers_mut().insert(
        "access-control-allow-origin",
        HeaderValue::from_str(origin).map_err(|_| denied())?,
    );
    response
        .headers_mut()
        .insert("vary", HeaderValue::from_static("Origin"));
    Ok(response)
}
pub(super) async fn page(
    State(app): State<App>,
    Path(id): Path<String>,
    Query(args): Query<HashMap<String, String>>,
) -> std::result::Result<Response, ApiError> {
    let origin = args.get("origin").map(String::as_str).unwrap_or("");
    allowed(&app, &id, origin).await?;
    widget_serves(&app, &id).await?;
    let root = std::env::var("V3_UI_ROOT").unwrap_or_else(|_| {
        webagent_configuration::paths::app()
            .join("ui")
            .to_string_lossy()
            .into_owned()
    });
    let live = webagent_configuration::paths::checkout().join("ui");
    let root = if webagent_webapp::configuration::app_settings(std::path::Path::new(
        webagent_webapp::configuration::LIVE_SETTINGS,
    ))
    .is_ok_and(|s| s["development"]["live_ui_editing"] == true)
        && live.join("embed/embed.html").is_file()
    {
        live.to_string_lossy().into_owned()
    } else {
        root
    };
    let html = tokio::fs::read_to_string(format!("{root}/embed/embed.html"))
        .await
        .map_err(|_| bad("Embed UI unavailable"))?;
    let mut response = axum::response::Html(html).into_response();
    let csp=format!("default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' data: blob:; connect-src 'self'; frame-ancestors {origin}; base-uri 'self'; form-action 'self'");
    response.headers_mut().insert(
        "content-security-policy",
        HeaderValue::from_str(&csp).map_err(|_| denied())?,
    );
    Ok(response)
}
/// Live embed challenges or credentials one client address may hold for one agent.
const CHALLENGES_PER_CLIENT: usize = 16;
static EMBED_ATTEMPTS: std::sync::OnceLock<
    Mutex<webagent_accounts::Recent<(PathBuf, String, std::net::IpAddr, &'static str)>>,
> = std::sync::OnceLock::new();
async fn rate(
    app: &App,
    agent: &str,
    client: std::net::IpAddr,
    lane: &'static str,
) -> std::result::Result<(), ApiError> {
    // Independent agent/client/operation windows cannot consume native login admission.
    let key = (
        app.store.root.clone(),
        agent.to_owned(),
        webagent_accounts::client_group(client),
        lane,
    );
    let admitted = EMBED_ATTEMPTS
        .get_or_init(|| Mutex::new(webagent_accounts::Recent::default()))
        .lock()
        .await
        .hit(key, 30, 60, now());
    if !admitted {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again in one minute".into(),
        ));
    }
    let client = client.to_string();
    // These credentials/challenges are transient, bounded encrypted app records.
    for (prefix, limit) in [
        ("embed:challenge:", 512usize),
        ("embed:credential:", 2048usize),
    ] {
        let records = app.store.list(prefix).await?;
        let mut retained = 0;
        let mut own = 0;
        for doc in records {
            if doc["expires"].as_u64().unwrap_or(0) <= now() {
                let key = if prefix == "embed:challenge:" {
                    doc["challenge_key"].as_str()
                } else {
                    doc["credential_key"].as_str()
                };
                if let Some(key) = key {
                    app.store.delete(key).await?;
                }
            } else if doc["agent_id"].as_str() == Some(agent) {
                retained += 1;
                if doc["client"].as_str() == Some(client.as_str()) {
                    own += 1;
                }
            }
        }
        // One address may hold only a few live challenges, so a handful of senders cannot
        // fill the agent's cap and block real sign-ins.
        if own >= CHALLENGES_PER_CLIENT {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Try again in a few minutes".into(),
            ));
        }
        if retained >= limit {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many active embed connections".into(),
            ));
        }
    }
    Ok(())
}
fn first_party(app: &App, headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    if let Some(origin) = headers.get("origin").and_then(|v| v.to_str().ok()) {
        let host = headers
            .get("host")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if origin != format!("https://{host}") && (app.secure || origin != format!("http://{host}"))
        {
            return Err(denied());
        }
    }
    Ok(())
}
pub(super) async fn challenge(
    State(app): State<App>,
    Path(id): Path<String>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Api {
    first_party(&app, &headers)?;
    let origin = required(&body, "origin", 256)?;
    let config = allowed(&app, &id, origin).await?;
    widget_serves(&app, &id).await?;
    let proof = required(&body, "code_challenge", 43)?;
    if proof.len() != 43
        || !proof
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(bad("S256 challenge required"));
    }
    let client = crate::account_authentication::throttle_client(&app, peer, &headers)?;
    rate(&app, &id, client, "challenge").await?;
    let challenge = uuid();
    let expires = now() + 300;
    app.store.put(&format!("embed:challenge:{challenge}"),&json!({"challenge_key":format!("embed:challenge:{challenge}"),"agent_id":id,"client":client.to_string(),"origin":origin,"revision":config["revision"],"proof":proof,"expires":expires})).await?;
    Ok(Json(json!({"challenge_id":challenge,"expires_at":expires})))
}
pub(super) async fn challenge_info(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    let doc = app
        .store
        .get(&format!("embed:challenge:{id}"))
        .await?
        .ok_or_else(denied)?;
    if doc["expires"].as_u64().unwrap_or(0) <= now() {
        return Err(denied());
    }
    let agent = doc["agent_id"].as_str().ok_or_else(denied)?;
    let user = identity["_user_id"].as_str().ok_or_else(denied)?;
    let config = allowed(&app, agent, doc["origin"].as_str().unwrap_or("")).await?;
    if config["revision"] != doc["revision"]
        || !app.store.live_access(user, agent).await?.admitted()
    {
        return Err(denied());
    }
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM agents WHERE id=?")
        .bind(agent)
        .fetch_optional(&admin_agent_view(&app, agent).await?.store.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"agent":{"id":agent,"name":name},"origin":doc["origin"],"expires_at":doc["expires"]}),
    ))
}
pub(super) async fn authorize(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let id = required(&body, "challenge_id", 36)?;
    let _guard = app.gate.lock().await;
    let mut doc = app
        .store
        .get(&format!("embed:challenge:{id}"))
        .await?
        .ok_or_else(denied)?;
    if doc["expires"].as_u64().unwrap_or(0) <= now() || !doc["user_id"].is_null() {
        return Err(denied());
    }
    let user = identity["_user_id"].as_str().ok_or_else(denied)?;
    let agent = doc["agent_id"].as_str().ok_or_else(denied)?;
    let config = allowed(&app, agent, doc["origin"].as_str().unwrap_or("")).await?;
    if config["revision"] != doc["revision"]
        || !app.store.live_access(user, agent).await?.admitted()
    {
        return Err(denied());
    }
    doc["user_id"] = json!(user);
    doc["generation"] = identity["generation"].clone();
    doc["auth_key"] = identity["_auth_key"].clone();
    doc["auth_expires"] = identity["expires"].clone();
    app.store
        .put(&format!("embed:challenge:{id}"), &doc)
        .await?;
    Ok(Json(json!({"authorized":true})))
}
async fn issue(app: &App, doc: &Value, account: &Value) -> Api {
    let user = account["user_id"].as_str().ok_or_else(denied)?;
    let agent = doc["agent_id"].as_str().ok_or_else(denied)?;
    let config = allowed(app, agent, doc["origin"].as_str().unwrap_or("")).await?;
    let access = app.store.live_access(user, agent).await?;
    if config["revision"] != doc["revision"] || !access.admitted() {
        return Err(denied());
    }
    if !widget_serves_for(app, &access).await {
        return Err(paused());
    }
    let principal = access::Principal::from_account(account)?;
    if principal.role == access::Role::Administrator {
        return Err(administrators_refused());
    }
    if principal.role == access::Role::Guest && config["config"]["guest_access"] != true {
        return Err(denied());
    }
    if !principal.active()
        || account["is_approved"] != true
        || !app
            .store
            .read_access_policy()
            .ok()
            .is_some_and(|p| registration::page_allowed(&p, "main", "chat", &principal))
    {
        return Err(denied());
    }
    let token = format!("{}{}", uuid(), uuid());
    let id = uuid();
    let expires = (now() + 3600).min(doc["auth_expires"].as_u64().unwrap_or(u64::MAX));
    let mut value = doc.clone();
    value["id"] = json!(id);
    value["expires"] = json!(expires);
    value["user_id"] = json!(user);
    value["generation"] = account["generation"].clone();
    let credential_key = format!("embed:credential:{}", digest(&json!(token)));
    value["credential_key"] = json!(credential_key);
    app.store.put(&credential_key, &value).await?;
    Ok(Json(
        json!({"token":token,"credential_id":id,"expires_at":expires,"user":{"user_id":user,"user":account["username"],"role":account["role"],"is_guest":principal.role==access::Role::Guest}}),
    ))
}
pub(super) async fn redeem(
    State(app): State<App>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Api {
    first_party(&app, &headers)?;
    let id = required(&body, "challenge_id", 36)?;
    let verifier = required(&body, "code_verifier", 128)?;
    if verifier.len() < 43
        || !verifier
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
    {
        return Err(denied());
    }
    let _guard = app.gate.lock().await;
    let key = format!("embed:challenge:{id}");
    let doc = app.store.get(&key).await?.ok_or_else(denied)?;
    if doc["expires"].as_u64().unwrap_or(0) <= now()
        || doc["proof"] != URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    {
        return Err(denied());
    }
    let user = doc["user_id"].as_str().ok_or_else(denied)?;
    let account = app.store.account_by_id(user).await?.ok_or_else(denied)?;
    if account["generation"] != doc["generation"] || !source_valid(&app, &doc).await {
        return Err(denied());
    }
    let agent = doc["agent_id"].as_str().ok_or_else(denied)?;
    allowed(&app, agent, doc["origin"].as_str().unwrap_or("")).await?;
    let client = crate::account_authentication::throttle_client(&app, peer, &headers)?;
    rate(&app, agent, client, "redeem").await?;
    let result = issue(&app, &doc, &account).await?;
    app.store.delete(&key).await?;
    Ok(result)
}
pub(super) async fn guest(
    State(app): State<App>,
    Path(id): Path<String>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Api {
    first_party(&app, &headers)?;
    let origin = required(&body, "origin", 256)?;
    let _guard = app.gate.lock().await;
    let config = allowed(&app, &id, origin).await?;
    if config["config"]["guest_access"] != true
        || registration::mode(&app).await? != "public_registered"
        || !app.store.has_admin_account().await?
    {
        return Err(denied());
    }
    // A paused widget creates no guest account.
    widget_serves(&app, &id).await?;
    let client = crate::account_authentication::throttle_client(&app, peer, &headers)?;
    rate(&app, &id, client, "guest").await?;
    // Embedded guests share the daily new-account cap with sign-up and guest login.
    if !crate::request_limits::new_account_admitted(&app).await {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "New accounts are paused for today; try again tomorrow".into(),
        ));
    }
    let account = app.store.create_guest_account().await?;
    let user = account["user_id"].as_str().ok_or_else(denied)?;
    // Owner opt-in admits only a fresh guest as Caller. This does not change
    // account tiers, capability ceilings, tools, or agent ability permissions.
    app.store
        .set_agent_membership(user, &id, crate::access::Relationship::Caller, 0)
        .await?;
    let account = app.store.account_by_id(user).await?.ok_or_else(denied)?;
    issue(
        &app,
        &json!({"agent_id":id,"origin":origin,"revision":config["revision"]}),
        &account,
    )
    .await
}
async fn source_valid(app: &App, doc: &Value) -> bool {
    match doc["auth_key"].as_str() {
        Some(key) => app
            .store
            .accounts_repository()
            .session_reference_live(
                key,
                doc["user_id"].as_str().unwrap_or("admin"),
                &doc["generation"],
                now(),
            )
            .await
            .unwrap_or(false),
        None => true,
    }
}
pub(super) async fn revoke(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    let user = identity["_user_id"].as_str().ok_or_else(denied)?;
    for doc in app.store.list("embed:credential:").await? {
        if doc["id"] == id && doc["user_id"] == user {
            let key = doc["credential_key"].as_str().ok_or_else(denied)?;
            app.store.delete(key).await?;
            return Ok(Json(json!({"revoked":true})));
        }
    }
    Err(ApiError(
        StatusCode::NOT_FOUND,
        "Credential not found".into(),
    ))
}
/// Send owners their widget trial notices (7 days before the end, at the end), every minute
/// until the API stops. Each pass reads only the due `account_due` rows, never every account.
pub(crate) async fn widget_trial_service(app: App) {
    loop {
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(WIDGET_TRIAL_TICK) => {}
        }
        if let Err(error) = widget_trial_notices(&app, now()).await {
            tracing::warn!(%error, "Widget trial notice pass failed");
        }
    }
}
/// One pass at `now`: each owner whose row is due is read again (tier, trial start) and gets
/// the notice now due, if any; the row then moves to the next notice or goes. A notice that
/// cannot be written is tried again in an hour. Returns how many notices were sent.
pub(crate) async fn widget_trial_notices(app: &App, now: u64) -> anyhow::Result<usize> {
    let pool = &app.store.app_pool;
    let mut sent = 0;
    for owner in webagent_engine::due::passed(pool, WIDGET_TRIAL_DUE, now).await? {
        let tier = match app.store.account_by_id(&owner).await? {
            Some(account) => {
                let tier_id = access::Principal::from_account(&account)?.tier_id;
                app.store.entitlement_tier(&tier_id).await.ok().flatten()
            }
            None => None,
        };
        let limits = tier.map(|tier| tier.owner_limits).unwrap_or_default();
        let started = app
            .store
            .accounts_repository()
            .widget_trial_started(&owner)
            .await?;
        let (notice, mut next) = widget_trial::due_visit(&limits, started, now);
        if let (Some(notice), Some(started)) = (notice, started) {
            let ends = widget_trial::trial_end(started, limits.widget_trial_days.unwrap_or(0));
            match app
                .store
                .for_user(&owner)
                .await?
                .add_notice(
                    notice.kind(),
                    notice.severity(),
                    &widget_trial_message(notice, ends),
                )
                .await
            {
                Ok(()) => sent += 1,
                Err(error) => {
                    tracing::warn!(%error, "Widget trial notice not written; trying again later");
                    next = Some(now.saturating_add(WIDGET_TRIAL_RETRY));
                }
            }
        }
        webagent_engine::due::set(pool, &owner, WIDGET_TRIAL_DUE, next).await?;
    }
    Ok(sent)
}
/// The owner's notice text for `notice` of a trial that ends at `ends` (date in UTC).
fn widget_trial_message(notice: widget_trial::TrialNotice, ends: u64) -> String {
    let date = chrono::DateTime::from_timestamp(i64::try_from(ends).unwrap_or(i64::MAX), 0)
        .map(|at| at.format("%-d %B %Y").to_string())
        .unwrap_or_default();
    match notice {
        widget_trial::TrialNotice::Ending => format!(
            "Your website chat widget trial ends on {date} (UTC). After that your widgets pause; \
             their settings and visitor chats are kept, and a plan that includes widgets turns \
             them back on."
        ),
        widget_trial::TrialNotice::Ended => "Your website chat widget trial has ended and your \
             widgets are paused. Their settings and visitor chats are kept; a plan that \
             includes widgets turns them back on at once."
            .into(),
    }
}
// Also used by a long-lived stream; revocation must stop an already-open SSE.
pub(crate) async fn credential_valid(app: &App, doc: &Value) -> bool {
    let Some(key) = doc["credential_key"].as_str() else {
        return false;
    };
    if !app
        .store
        .get(key)
        .await
        .ok()
        .flatten()
        .is_some_and(|current| {
            current["id"] == doc["id"] && current["expires"].as_u64().unwrap_or(0) > now()
        })
    {
        return false;
    }
    let Some(agent) = doc["agent_id"].as_str() else {
        return false;
    };
    let Some(user) = doc["user_id"].as_str() else {
        return false;
    };
    if doc["expires"].as_u64().unwrap_or(0) <= now() || !source_valid(app, doc).await {
        return false;
    }
    let config = allowed(app, agent, doc["origin"].as_str().unwrap_or(""))
        .await
        .ok();
    if !config.is_some_and(|c| c["revision"] == doc["revision"]) {
        return false;
    }
    let Some(account) = app.store.account_by_id(user).await.ok().flatten() else {
        return false;
    };
    let Ok(principal) = access::Principal::from_account(&account) else {
        return false;
    };
    principal.active()
        && account["is_approved"] == true
        && account["generation"] == doc["generation"]
        && (principal.role != access::Role::Guest
            || registration::mode(app).await.unwrap_or_default() == "public_registered")
        && match app.store.live_access(user, agent).await {
            Ok(access) if access.admitted() => widget_serves_for(app, &access).await,
            _ => false,
        }
        && app
            .store
            .read_access_policy()
            .ok()
            .is_some_and(|p| registration::page_allowed(&p, "main", "chat", &principal))
}
/// Widget credentials never carry administrator power (AUTH-9): one refusal for the whole class.
fn administrators_refused() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Administrators use the app, not the embedded chat".into(),
    )
}
async fn bound_run_session(
    app: &App,
    actor: &str,
    agent: &str,
    id: &str,
    approval: bool,
) -> std::result::Result<Option<String>, ApiError> {
    let mut stores = vec![app.store.clone()];
    for entry in app.store.shared_sessions_for(actor).await? {
        if entry["agent_id"] != agent {
            continue;
        }
        if let Some(owner) = entry["owner_id"].as_str() {
            stores.push(app.store.for_user(owner).await?);
        }
    }
    for store in stores {
        let runs = if approval {
            store.active_runs().await?
        } else {
            store.get(&key("run", id)).await?.into_iter().collect()
        };
        for run in runs {
            if approval && run["pending"]["id"] != id {
                continue;
            }
            let Some(sid) = run["session_id"].as_str() else {
                continue;
            };
            if let Ok(access) = crate::shared_chat::resolve(app, sid, actor).await {
                if access.agent_id == agent {
                    return Ok(Some(sid.to_owned()));
                }
            }
        }
    }
    Ok(None)
}
async fn scope_request(
    app: &App,
    doc: &Value,
    request: &mut Request,
) -> std::result::Result<(), ApiError> {
    let agent = doc["agent_id"].as_str().ok_or_else(denied)?;
    let path = request
        .uri()
        .path()
        .strip_prefix("/api/v3")
        .unwrap_or(request.uri().path())
        .to_owned();
    // The route itself was checked against the widget allowlist (`credential_routes`).
    // The only agent route on it is the panel's read of the bound agent's privacy mode.
    if path.starts_with("/agents/") && path != format!("/agents/{agent}/access-summary") {
        return Err(denied());
    }
    // A widget's credential remains bound to one agent, even when the canonical
    // conversation lives in another participant's encrypted store.
    let actor = crate::shared_chat::actor(app);
    let parts: Vec<_> = path.trim_matches('/').split('/').collect();
    let session = if parts.first() == Some(&"sessions")
        && parts.len() > 1
        && !matches!(parts[1], "search" | "stats")
    {
        Some(parts[1].to_owned())
    } else if matches!(parts.first(), Some(&"runs") | Some(&"approvals")) && parts.len() > 1 {
        bound_run_session(app, actor, agent, parts[1], parts[0] == "approvals").await?
    } else {
        None
    };
    if matches!(parts.first(), Some(&"runs") | Some(&"approvals")) && session.is_none() {
        return Err(denied());
    }
    if let Some(id) = session {
        let access = crate::shared_chat::resolve(app, &id, actor)
            .await
            .map_err(|_| denied())?;
        if access.agent_id != agent {
            return Err(denied());
        }
    }
    if path == "/sessions" && request.method() == axum::http::Method::POST {
        let body = std::mem::take(request.body_mut());
        let bytes = axum::body::to_bytes(body, 2 * 1024 * 1024)
            .await
            .map_err(|_| bad("Invalid session request"))?;
        let mut value: Value =
            serde_json::from_slice(&bytes).map_err(|_| bad("Invalid session request"))?;
        if value.get("agent_id").is_some_and(|v| v != agent) {
            return Err(denied());
        }
        value["agent_id"] = json!(agent);
        *request.body_mut() =
            axum::body::Body::from(serde_json::to_vec(&value).map_err(anyhow::Error::from)?);
    }
    Ok(())
}
pub(crate) async fn bearer(app: &App, request: &mut Request) -> std::result::Result<(), ApiError> {
    first_party(app, request.headers())?;
    let token = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .filter(|t| t.len() == 72)
        .ok_or_else(unauthorized)?;
    let auth_key = format!("embed:credential:{}", digest(&json!(token)));
    let mut doc = app.store.get(&auth_key).await?.ok_or_else(unauthorized)?;
    if !credential_valid(app, &doc).await {
        // A paused widget says so instead of asking the visitor to sign in again.
        if let Some(agent) = doc["agent_id"].as_str() {
            widget_serves(app, agent).await?;
        }
        return Err(unauthorized());
    }
    // Default deny: only the routes the embedded chat calls (credential_routes.rs).
    let matched = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|matched| matched.as_str());
    if !super::credential_routes::widget_may(request.method(), matched) {
        return Err(route_refused());
    }
    let user = doc["user_id"].as_str().ok_or_else(unauthorized)?.to_owned();
    let mut caller = app.clone();
    caller.store = app.store.for_user(&user).await?;
    scope_request(&caller, &doc, request).await?;
    let account = app
        .store
        .account_by_id(&user)
        .await?
        .ok_or_else(unauthorized)?;
    // `credential_valid` checked this account active and approved during this request.
    let principal = access::Principal::from_account(&account).map_err(|_| unauthorized())?;
    if principal.role == access::Role::Administrator {
        return Err(administrators_refused());
    }
    let identity = Arc::new(access::identity::Identity {
        username: account["username"].as_str().unwrap_or_default().to_owned(),
        generation: account["generation"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        // Widget credentials are checked in full on every request.
        auth_epoch: None,
        kind: access::identity::CredentialKind::Widget,
        credential_key: auth_key.clone(),
        credential_record: Vec::new(),
        agent_id: doc["agent_id"].as_str().map(str::to_owned),
        principal,
    });
    doc["_auth_key"] = json!(auth_key);
    doc["_user_id"] = json!(user);
    doc["_role"] = json!(identity.effective_role());
    doc["_tier_id"] = account["tier_id"].clone();
    doc["_access_revision"] = account["access_revision"].clone();
    doc["_embed_agent"] = doc["agent_id"].clone();
    doc["_credential"] = json!(identity.kind.name());
    caller.identity = Some(identity.clone());
    request.extensions_mut().insert(caller);
    request.extensions_mut().insert(identity);
    request.extensions_mut().insert(doc);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::router;
    #[test]
    fn exact_origins_only() {
        assert!(origin_valid("https://example.com"));
        assert!(origin_valid("https://example.com:8443"));
        assert!(origin_valid("http://localhost:3000"));
        for value in [
            "https://example.com/",
            "https://example.com/path",
            "https://user@example.com",
            "https://example.com?x=1",
            "https://*.example.com",
            "https://example.com\n",
            "http://example.com",
            "null",
            "javascript:alert(1)",
        ] {
            assert!(!origin_valid(value), "{value}");
        }
    }
    #[tokio::test]
    async fn pkce_credentials_scope_replay_and_live_revocation() {
        use axum::body::Body;
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        // Widgets are for ordinary accounts: the agent's administrator sets the widget up and a
        // member signs in to it.
        let member = widget_member(&app, "widget-member").await;
        let generation =
            app.store.account_by_id(&member).await.unwrap().unwrap()["generation"].clone();
        let source_key = format!("auth:{}", digest(&json!(uuid())));
        let source = json!({"user_id":member,"generation":generation,"expires":now()+3600});
        app.store.put(&source_key, &source).await.unwrap();
        let identity = json!({"_user_id":member,"generation":generation,"_auth_key":source_key,"expires":now()+3600});
        let admin = json!({"_user_id":"admin"});
        let config = json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}});
        config_put(
            State(app.clone()),
            Extension(admin.clone()),
            Path("shared_default".into()),
            Json(config.clone()),
        )
        .await
        .unwrap();
        assert!(config_put(
            State(app.clone()),
            Extension(admin.clone()),
            Path("shared_default".into()),
            Json(config)
        )
        .await
        .is_err());
        assert!(allowed(&app, "shared_default", "https://evil.com")
            .await
            .is_err());
        let verifier = "fixture-verifier-abcdefghijklmnopqrstuvwxyz-0123456789";
        let challenge=challenge(State(app.clone()),Path("shared_default".into()),None, HeaderMap::new(),Json(json!({"origin":"https://example.com","code_challenge":URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))}))).await.unwrap().0;
        let id = challenge["challenge_id"].as_str().unwrap();
        assert!(redeem(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"challenge_id":id,"code_verifier":verifier}))
        )
        .await
        .is_err());
        let review = challenge_info(
            State(app.clone()),
            Extension(identity.clone()),
            Path(id.into()),
        )
        .await
        .unwrap()
        .0;
        assert_eq!(review["origin"], "https://example.com");
        assert_eq!(review["agent"]["id"], "shared_default");
        authorize(
            State(app.clone()),
            Extension(identity.clone()),
            Json(json!({"challenge_id":id})),
        )
        .await
        .unwrap();
        assert!(authorize(
            State(app.clone()),
            Extension(identity.clone()),
            Json(json!({"challenge_id":id}))
        )
        .await
        .is_err());
        assert!(redeem(State(app.clone()),None, HeaderMap::new(),Json(json!({"challenge_id":id,"code_verifier":"wrong-verifier-abcdefghijklmnopqrstuvwxyz-0123456789"}))).await.is_err());
        let credential = redeem(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"challenge_id":id,"code_verifier":verifier})),
        )
        .await
        .unwrap()
        .0;
        assert!(redeem(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"challenge_id":id,"code_verifier":verifier}))
        )
        .await
        .is_err());
        let token = credential["token"].as_str().unwrap();
        let request = |method: &str, path: &str, body: Value| {
            Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap()
        };
        let response = router(app.clone())
            .oneshot(request(
                "POST",
                "/api/v3/sessions",
                json!({"client_request_id":"embed-create","agent_id":"shared_default"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let created: Value = serde_json::from_slice(&bytes).unwrap();
        let sid = created["id"].as_str().unwrap();
        assert_eq!(
            router(app.clone())
                .oneshot(request(
                    "GET",
                    &format!("/api/v3/sessions/{sid}"),
                    Value::Null
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            router(app.clone())
                .oneshot(request(
                    "POST",
                    "/api/v3/sessions",
                    json!({"client_request_id":"embed-wrong","agent_id":"another"})
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for path in [
            "/api/v3/admin/access",
            "/api/v3/models/me/catalog",
            "/api/v3/sessions/missing",
            "/api/v3/sessions/missing/promotions",
        ] {
            assert_ne!(
                router(app.clone())
                    .oneshot(request("GET", path, Value::Null))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
        let member_store = app.store.for_user(&member).await.unwrap();
        member_store
            .put(
                "session:000-foreign",
                &json!({"id":"000-foreign","agent_id":"different","title":"Needle foreign"}),
            )
            .await
            .unwrap();
        member_store
            .put(
                "session:zzz-target",
                &json!({"id":"zzz-target","agent_id":"shared_default","title":"Needle target"}),
            )
            .await
            .unwrap();
        let response = router(app.clone())
            .oneshot(request(
                "GET",
                "/api/v3/sessions/search?q=needle&limit=1",
                Value::Null,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let found: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(found["results"].as_array().unwrap().len(), 1);
        assert_eq!(found["results"][0]["session_id"], "zzz-target");
        assert_eq!(
            router(app.clone())
                .oneshot(request("GET", "/api/v3/sessions/000-foreign", Value::Null))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let list = router(app.clone())
            .oneshot(request("GET", "/api/v3/sessions", Value::Null))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(list.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let list: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(list["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["agent_id"] == "shared_default"));
        app.store.delete(&source_key).await.unwrap();
        assert_eq!(
            router(app.clone())
                .oneshot(request(
                    "GET",
                    &format!("/api/v3/sessions/{sid}"),
                    Value::Null
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        app.store.put(&source_key, &source).await.unwrap();
        // A widget credential reaches no admin route.
        assert_eq!(
            router(app.clone())
                .oneshot(request("GET", "/api/v3/status", Value::Null))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        // It may read the conversation's settings but never change them (AUTH-9).
        for (method, path, body) in [
            (
                "PUT",
                format!("/api/v3/sessions/{sid}/mode"),
                json!({"mode":"auto"}),
            ),
            (
                "PUT",
                format!("/api/v3/sessions/{sid}/model"),
                json!({"model":"x"}),
            ),
            (
                "PUT",
                format!("/api/v3/sessions/{sid}/abilities/web"),
                json!({"enabled":true}),
            ),
            (
                "POST",
                format!("/api/v3/sessions/{sid}/build-tasks/clear"),
                json!({}),
            ),
            (
                "GET",
                format!("/api/v3/sessions/{sid}/ui-preview/off"),
                Value::Null,
            ),
        ] {
            assert_eq!(
                router(app.clone())
                    .oneshot(request(method, &path, body))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN,
                "{method} {path}"
            );
        }
        assert_eq!(
            router(app.clone())
                .oneshot(request(
                    "GET",
                    &format!("/api/v3/sessions/{sid}/mode"),
                    Value::Null
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        config_put(State(app.clone()),Extension(admin.clone()),Path("shared_default".into()),Json(json!({"expected_revision":1,"config":{"enabled":false,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        assert_eq!(
            router(app.clone())
                .oneshot(request(
                    "GET",
                    &format!("/api/v3/sessions/{sid}"),
                    Value::Null
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        // Issued credential can also be explicitly revoked by its owner.
        revoke(
            State(app.clone()),
            Extension(identity),
            Path(credential["credential_id"].as_str().unwrap().into()),
        )
        .await
        .unwrap();
    }
    /// An active ordinary account admitted to `shared_default` as a caller.
    async fn widget_member(app: &App, name: &str) -> String {
        let account = app
            .store
            .create_user_account(name, "fixture-long-password-hash")
            .await
            .unwrap();
        let id = account["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(&id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                &id,
                "shared_default",
                crate::access::Relationship::Caller,
                0,
            )
            .await
            .unwrap();
        id
    }
    #[tokio::test]
    async fn administrators_cannot_hold_widget_credentials() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let source_key = format!("auth:{}", digest(&json!(uuid())));
        app.store
            .put(
                &source_key,
                &json!({"generation":"fixture-generation","expires":now()+3600}),
            )
            .await
            .unwrap();
        let admin = json!({"_user_id":"admin","generation":"fixture-generation","_auth_key":source_key,"expires":now()+3600});
        config_put(State(app.clone()),Extension(admin.clone()),Path("shared_default".into()),Json(json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        let verifier = "fixture-verifier-abcdefghijklmnopqrstuvwxyz-0123456789";
        let challenge=challenge(State(app.clone()),Path("shared_default".into()),None, HeaderMap::new(),Json(json!({"origin":"https://example.com","code_challenge":URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))}))).await.unwrap().0;
        let id = challenge["challenge_id"].as_str().unwrap();
        // The administrator may approve the request, but no credential is ever issued.
        let _ = authorize(
            State(app.clone()),
            Extension(admin.clone()),
            Json(json!({"challenge_id":id})),
        )
        .await;
        let refused = redeem(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"challenge_id":id,"code_verifier":verifier})),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert!(app
            .store
            .list("embed:credential:")
            .await
            .unwrap()
            .is_empty());
        // A credential that became an administrator's later is refused on its next request.
        let member = widget_member(&app, "promoted-member").await;
        let generation =
            app.store.account_by_id(&member).await.unwrap().unwrap()["generation"].clone();
        let token = format!("{}{}", uuid(), uuid());
        let credential_key = format!("embed:credential:{}", digest(&json!(token)));
        let config = allowed(&app, "shared_default", "https://example.com")
            .await
            .unwrap();
        app.store
            .put(
                &credential_key,
                &json!({"id":"c","credential_key":credential_key,"agent_id":"shared_default",
                    "origin":"https://example.com","revision":config["revision"],
                    "user_id":member,"generation":generation,"expires":now()+3600}),
            )
            .await
            .unwrap();
        let me = || {
            Request::builder()
                .uri("/api/v3/user/status")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        use tower::ServiceExt;
        assert_eq!(
            router(app.clone()).oneshot(me()).await.unwrap().status(),
            StatusCode::OK
        );
        app.store
            .update_account_access(
                &member,
                2,
                crate::access::AccountStatus::Active,
                crate::access::ADMINISTRATOR_TIER,
            )
            .await
            .unwrap();
        assert_eq!(
            router(app.clone()).oneshot(me()).await.unwrap().status(),
            StatusCode::FORBIDDEN
        );
    }
    /// A Plus account's widget keeps the widget's limits: no administrator routes, no settings
    /// changes, whatever the account's tier allows elsewhere.
    #[tokio::test]
    async fn a_plus_members_widget_keeps_widget_limits() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let source_key = format!("auth:{}", digest(&json!(uuid())));
        app.store
            .put(
                &source_key,
                &json!({"generation":"fixture-generation","expires":now()+3600}),
            )
            .await
            .unwrap();
        let admin = json!({"_user_id":"admin","generation":"fixture-generation","_auth_key":source_key,"expires":now()+3600});
        config_put(State(app.clone()),Extension(admin),Path("shared_default".into()),Json(json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        app.store
            .save_entitlement_tier(
                crate::access::Tier {
                    id: "plus".into(),
                    revision: 0,
                    allowed_models: vec!["*".into()],
                    model_policy: None,
                    max_sessions: 100,
                    max_active_runs: 25,
                    daily_turns: None,
                    owner_limits: Default::default(),
                },
                0,
            )
            .await
            .unwrap();
        let member = widget_member(&app, "plus-widget").await;
        app.store
            .update_account_access(&member, 2, crate::access::AccountStatus::Active, "plus")
            .await
            .unwrap();
        let generation =
            app.store.account_by_id(&member).await.unwrap().unwrap()["generation"].clone();
        let token = format!("{}{}", uuid(), uuid());
        let credential_key = format!("embed:credential:{}", digest(&json!(token)));
        let config = allowed(&app, "shared_default", "https://example.com")
            .await
            .unwrap();
        app.store
            .put(
                &credential_key,
                &json!({"id":"c","credential_key":credential_key,"agent_id":"shared_default",
                    "origin":"https://example.com","revision":config["revision"],
                    "user_id":member,"generation":generation,"expires":now()+3600}),
            )
            .await
            .unwrap();
        let send = |method: &str, uri: String| {
            Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap()
        };
        use tower::ServiceExt;
        let status = |request: Request<axum::body::Body>| {
            let app = app.clone();
            async move { router(app).oneshot(request).await.unwrap().status() }
        };
        assert_eq!(
            status(send("GET", "/api/v3/user/status".into())).await,
            StatusCode::OK
        );
        let sid = uuid();
        for (method, uri) in [
            ("GET", "/api/v3/admin/access/accounts".to_owned()),
            ("GET", "/api/v3/status".to_owned()),
            ("PUT", format!("/api/v3/sessions/{sid}/mode")),
            ("PUT", format!("/api/v3/sessions/{sid}/model")),
            (
                "GET",
                "/api/v3/models/catalog?agent_id=shared_default".to_owned(),
            ),
            (
                "PUT",
                "/api/v3/models/catalog?agent_id=shared_default".to_owned(),
            ),
        ] {
            assert_eq!(
                status(send(method, uri.clone())).await,
                StatusCode::FORBIDDEN,
                "{method} {uri}"
            );
        }
    }
    #[tokio::test]
    async fn a_disabled_widget_guest_is_refused_on_every_route() {
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        app.store.set_registration_policy("open").unwrap();
        config_put(State(app.clone()),Extension(json!({"_user_id":"admin"})),Path("shared_default".into()),Json(json!({"expected_revision":0,"config":{"enabled":true,"guest_access":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        let issued = guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"})),
        )
        .await
        .unwrap()
        .0;
        let token = issued["token"].as_str().unwrap().to_owned();
        let user = issued["user"]["user_id"].as_str().unwrap().to_owned();
        let call = |path: &str| {
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let routes = ["/api/v3/sessions", "/api/v3/user/status"];
        for path in routes {
            assert_eq!(
                router(app.clone())
                    .oneshot(call(path))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK,
                "{path}"
            );
        }
        let revision = app.store.account_by_id(&user).await.unwrap().unwrap()["access_revision"]
            .as_u64()
            .unwrap();
        app.store
            .update_account_access(
                &user,
                revision,
                crate::access::AccountStatus::Disabled,
                crate::access::ANONYMOUS_TIER,
            )
            .await
            .unwrap();
        for path in routes {
            assert_eq!(
                router(app.clone())
                    .oneshot(call(path))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::UNAUTHORIZED,
                "{path}"
            );
        }
    }
    #[tokio::test]
    async fn unknown_credentials_return_authentication_failure() {
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let request = Request::builder()
            .uri("/api/v3/sessions")
            .header("authorization", "Bearer unknown")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            router(app).oneshot(request).await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
    }
    #[tokio::test]
    async fn one_address_cannot_fill_the_agent_challenge_cap() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for n in 0..CHALLENGES_PER_CLIENT {
            app.store
                .put(
                    &format!("embed:challenge:c{n}"),
                    &json!({"challenge_key":format!("embed:challenge:c{n}"),"agent_id":"agent-a",
                        "client":"192.0.2.7","expires":now() + 300}),
                )
                .await
                .unwrap();
        }
        let refused = rate(&app, "agent-a", "192.0.2.7".parse().unwrap(), "challenge").await;
        assert_eq!(refused.unwrap_err().0, StatusCode::TOO_MANY_REQUESTS);
        assert!(
            rate(&app, "agent-a", "192.0.2.8".parse().unwrap(), "challenge")
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn embed_throttle_preserves_password_login_budget() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        for _ in 0..30 {
            rate(&app, "agent-a", "192.0.2.1".parse().unwrap(), "challenge")
                .await
                .unwrap();
        }
        assert!(
            rate(&app, "agent-a", "192.0.2.1".parse().unwrap(), "challenge")
                .await
                .is_err()
        );
        assert!(
            rate(&app, "agent-b", "192.0.2.1".parse().unwrap(), "challenge")
                .await
                .is_ok()
        );
        assert!(
            rate(&app, "agent-a", "192.0.2.2".parse().unwrap(), "challenge")
                .await
                .is_ok()
        );
        assert!(rate(&app, "agent-a", "192.0.2.1".parse().unwrap(), "guest")
            .await
            .is_ok());
        assert!(app.login_attempts.is_empty().await);
    }
    #[tokio::test]
    async fn malformed_challenges_do_not_spend_valid_admission() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        config_put(State(app.clone()), Extension(json!({"_user_id":"admin"})), Path("shared_default".into()),
            Json(json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        for _ in 0..35 {
            assert!(challenge(
                State(app.clone()),
                Path("shared_default".into()),
                None,
                HeaderMap::new(),
                Json(json!({"origin":"https://example.com","code_challenge":"invalid"}))
            )
            .await
            .is_err());
        }
        assert!(challenge(
            State(app),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com","code_challenge":"a".repeat(43)}))
        )
        .await
        .is_ok());
    }
    #[tokio::test]
    async fn guest_and_descriptor_fail_closed() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        config_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        assert!(guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"}))
        )
        .await
        .is_err());
        app.store.set_registration_policy("open").unwrap();
        assert!(guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"}))
        )
        .await
        .is_err());
        config_put(State(app.clone()),Extension(identity),Path("shared_default".into()),Json(json!({"expected_revision":1,"config":{"enabled":true,"guest_access":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        let issued = guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"})),
        )
        .await
        .unwrap()
        .0;
        let credential_key = format!("embed:credential:{}", digest(&issued["token"]));
        let doc = app.store.get(&credential_key).await.unwrap().unwrap();
        assert!(credential_valid(&app, &doc).await);
        let membership = app
            .store
            .agent_membership(
                issued["user"]["user_id"].as_str().unwrap(),
                "shared_default",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(membership["relationship"], "caller");
        assert_ne!(membership["agent_administrator"], true);
        // Once today's new-account cap is used, an embedded guest is refused too.
        let mut used = 0;
        while crate::request_limits::new_account_admitted(&app).await {
            used += 1;
            assert!(used < 1000, "the daily new-account cap never filled");
        }
        let refused = guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"})),
        )
        .await
        .unwrap_err();
        assert_eq!(refused.0, StatusCode::TOO_MANY_REQUESTS);
        app.store.set_registration_policy("restricted").unwrap();
        assert!(!credential_valid(&app, &doc).await);
        app.store.set_registration_policy("open").unwrap();
        assert!(credential_valid(&app, &doc).await);
        revoke(
            State(app.clone()),
            Extension(json!({"_user_id":issued["user"]["user_id"]})),
            Path(issued["credential_id"].as_str().unwrap().into()),
        )
        .await
        .unwrap();
        assert!(!credential_valid(&app, &doc).await);
        let mut args = HashMap::new();
        args.insert("origin".into(), "https://evil.com".into());
        assert!(descriptor(
            State(app.clone()),
            Path("shared_default".into()),
            Query(args),
            HeaderMap::new()
        )
        .await
        .is_err());
    }
    /// One request with a sign-in cookie or a widget bearer; returns the status and JSON body.
    async fn trial_call(
        app: &App,
        method: &str,
        uri: &str,
        auth: (&str, &str),
        body: Value,
    ) -> (StatusCode, Value) {
        use tower::ServiceExt;
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        request = match auth {
            ("cookie", token) => request
                .header("cookie", format!("v3_session={token}"))
                .header("x-csrf-token", "fixture-csrf"),
            ("bearer", token) => request.header("authorization", format!("Bearer {token}")),
            _ => request,
        };
        let response = router(app.clone())
            .oneshot(
                request
                    .body(axum::body::Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn trial_sign_in(app: &App, user: &str) -> String {
        let token = uuid();
        let record = app.store.account_by_id(user).await.unwrap().unwrap();
        app.store
            .put(
                &format!("auth:{}", digest(&json!(token))),
                &json!({"id":uuid(),"user_id":user,"csrf":"fixture-csrf",
                    "generation":record["generation"],"expires":now()+3600}),
            )
            .await
            .unwrap();
        token
    }

    /// A widget credential of `visitor` on `agent` for https://example.com; returns its token.
    async fn trial_credential(app: &App, visitor: &str, agent: &str) -> String {
        let generation =
            app.store.account_by_id(visitor).await.unwrap().unwrap()["generation"].clone();
        let config = settings(app, agent).await.unwrap();
        let token = format!("{}{}", uuid(), uuid());
        let credential_key = format!("embed:credential:{}", digest(&json!(token)));
        app.store
            .put(
                &credential_key,
                &json!({"id":uuid(),"credential_key":credential_key,"agent_id":agent,
                    "origin":"https://example.com","revision":config["revision"],
                    "user_id":visitor,"generation":generation,"expires":now()+3600}),
            )
            .await
            .unwrap();
        token
    }

    /// A fresh widget credential of `visitor` on `agent` asks `/user/status` (the embedded
    /// chat's own refresh): status and body.
    async fn trial_me(app: &App, visitor: &str, agent: &str) -> (StatusCode, Value) {
        let token = trial_credential(app, visitor, agent).await;
        trial_call(
            app,
            "GET",
            "/api/v3/user/status",
            ("bearer", &token),
            Value::Null,
        )
        .await
    }

    /// Issues `visitor` a widget credential on `agent` at the current configuration revision.
    async fn trial_issue(app: &App, visitor: &str, agent: &str) -> Api {
        let config = settings(app, agent).await.unwrap();
        let account = app.store.account_by_id(visitor).await.unwrap().unwrap();
        let doc =
            json!({"agent_id":agent,"origin":"https://example.com","revision":config["revision"]});
        issue(app, &doc, &account).await
    }

    /// Moves the owner's recorded trial start (test only: the app writes it once).
    async fn set_trial_start(app: &App, owner: &str, at: u64) {
        sqlx::query("UPDATE user_accounts SET widget_trial_started=? WHERE user_id=?")
            .bind(i64::try_from(at).unwrap())
            .bind(owner)
            .execute(&app.store.app_pool)
            .await
            .unwrap();
    }

    async fn trial_due(app: &App, owner: &str) -> Option<u64> {
        webagent_engine::due::all(&app.store.app_pool)
            .await
            .unwrap()
            .into_iter()
            .find(|(user, kind, _)| user == owner && kind == WIDGET_TRIAL_DUE)
            .map(|(_, _, at)| at)
    }

    /// Free owners get one 30-day widget trial covering all their agents, starting at the first
    /// widget turned on; after it widgets pause (settings and chats kept) until the owner's
    /// plan includes widgets, which a second sign-in changes for the widget's next request.
    #[tokio::test]
    async fn the_widget_trial_runs_once_then_pauses_until_the_owner_has_plus() {
        const DAY: u64 = 86_400;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let mut free = app.store.entitlement_tier("free").await.unwrap().unwrap();
        let free_revision = free.revision;
        free.owner_limits.widgets = Some(Widgets::Trial);
        free.owner_limits.widget_trial_days = Some(30);
        free.owner_limits.widget_trial_notice_days = Some(7);
        app.store
            .save_entitlement_tier(free, free_revision)
            .await
            .unwrap();
        app.store
            .save_entitlement_tier(
                crate::access::Tier {
                    id: "plus".into(),
                    revision: 0,
                    allowed_models: vec!["*".into()],
                    model_policy: None,
                    max_sessions: 100,
                    max_active_runs: 25,
                    daily_turns: None,
                    owner_limits: crate::access::OwnerLimits {
                        widgets: Some(Widgets::Yes),
                        ..Default::default()
                    },
                },
                0,
            )
            .await
            .unwrap();
        let owner = widget_member(&app, "trial-owner").await;
        let created = |name: &'static str| {
            let app = app.clone();
            let owner = owner.clone();
            async move {
                let Json(created) = crate::agents_api::create_agent(
                    State(app),
                    Extension(json!({"_user_id":owner})),
                    Json(crate::agents_api::NewAgent {
                        client_request_id: format!("{name}-request"),
                        name: name.into(),
                        description: None,
                    }),
                )
                .await
                .unwrap();
                created["id"].as_str().unwrap().to_owned()
            }
        };
        let agent = created("Trial agent").await;
        let second = created("Second trial agent").await;
        let visitor = widget_member(&app, "trial-visitor").await;
        app.store
            .set_agent_membership(&visitor, &agent, crate::access::Relationship::Caller, 0)
            .await
            .unwrap();
        let owner_cookie = trial_sign_in(&app, &owner).await;
        let admin_cookie = trial_sign_in(&app, "admin").await;
        let embed_uri = format!("/api/v3/agents/{agent}/embed");
        let descriptor_uri =
            format!("/api/v3/embed/{agent}/descriptor?origin=https%3A%2F%2Fexample.com");
        let config = |revision: u64, enabled: bool| {
            json!({"expected_revision":revision,"config":{"enabled":enabled,
                "allowed_origins":["https://example.com"]}})
        };
        let turn = |revision: u64, enabled: bool| {
            let app = app.clone();
            let (uri, cookie, body) = (
                embed_uri.clone(),
                owner_cookie.clone(),
                config(revision, enabled),
            );
            async move { trial_call(&app, "PUT", &uri, ("cookie", &cookie), body).await }
        };
        // Whether a public widget route answered "paused" (the test fixture has no chat UI
        // configuration, so an allowed descriptor may still fail later for that reason).
        let public = |uri: String| {
            let app = app.clone();
            async move {
                let (status, body) = trial_call(&app, "GET", &uri, ("none", ""), Value::Null).await;
                status == StatusCode::SERVICE_UNAVAILABLE
                    && body["error"].as_str().is_some_and(|e| e.contains("paused"))
            }
        };

        // Not started: the page offers the trial; nothing serves yet.
        let (status, page) = trial_call(
            &app,
            "GET",
            &embed_uri,
            ("cookie", &owner_cookie),
            Value::Null,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            page["widget_plan"],
            json!({"plan":"trial","state":"not_started","days":30,"serves":false})
        );
        assert_eq!(
            app.store
                .accounts_repository()
                .widget_trial_started(&owner)
                .await
                .unwrap(),
            None
        );

        // Turning the first widget on starts the trial: account start, then a due row 7 days
        // before its end; the widget serves.
        let before = now();
        let (status, saved) = turn(0, true).await;
        assert_eq!(status, StatusCode::OK, "{saved}");
        let started = app
            .store
            .accounts_repository()
            .widget_trial_started(&owner)
            .await
            .unwrap()
            .unwrap();
        assert!(started >= before && started <= now());
        assert_eq!(saved["widget_plan"]["state"], "running");
        assert_eq!(saved["widget_plan"]["ends_at"], started + 30 * DAY);
        assert_eq!(trial_due(&app, &owner).await, Some(started + 23 * DAY));
        assert!(!public(descriptor_uri.clone()).await);
        let me = trial_me(&app, &visitor, &agent).await;
        assert_eq!(me.0, StatusCode::OK, "{}", me.1);
        assert!(trial_issue(&app, &visitor, &agent).await.is_ok());

        // It never restarts: off and on again, or another agent turned on, keeps the start.
        assert_eq!(turn(1, false).await.0, StatusCode::OK);
        assert_eq!(turn(2, true).await.0, StatusCode::OK);
        let (status, other) = trial_call(
            &app,
            "PUT",
            &format!("/api/v3/agents/{second}/embed"),
            ("cookie", &owner_cookie),
            config(0, true),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(other["widget_plan"]["ends_at"], started + 30 * DAY);
        assert_eq!(
            app.store
                .accounts_repository()
                .widget_trial_started(&owner)
                .await
                .unwrap(),
            Some(started)
        );

        // A session of the visitor, to show chats are kept through the pause.
        app.store
            .for_user(&visitor)
            .await
            .unwrap()
            .put(
                "session:trial-chat",
                &json!({"id":"trial-chat","agent_id":agent}),
            )
            .await
            .unwrap();

        // The notices: 7 days before the end, then at the end; the row then goes.
        let ends = started + 30 * DAY;
        assert_eq!(
            widget_trial_notices(&app, started + 23 * DAY - 1)
                .await
                .unwrap(),
            0
        );
        assert_eq!(
            widget_trial_notices(&app, started + 23 * DAY)
                .await
                .unwrap(),
            1
        );
        assert_eq!(trial_due(&app, &owner).await, Some(ends));
        assert_eq!(
            widget_trial_notices(&app, started + 23 * DAY)
                .await
                .unwrap(),
            0
        );
        assert_eq!(widget_trial_notices(&app, ends).await.unwrap(), 1);
        assert_eq!(trial_due(&app, &owner).await, None);
        let notices = app
            .store
            .for_user(&owner)
            .await
            .unwrap()
            .notices()
            .await
            .unwrap();
        let kinds: Vec<_> = notices["notices"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["kind"].clone())
            .collect();
        assert_eq!(
            kinds,
            [json!("widget_trial_ended"), json!("widget_trial_ending")]
        );

        // After the end (31 days ago it started): everything a visitor reaches pauses.
        set_trial_start(&app, &owner, now() - 31 * DAY).await;
        assert!(public(descriptor_uri.clone()).await);
        let (status, body) = trial_me(&app, &visitor, &agent).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            body["error"]
                .as_str()
                .unwrap_or_default()
                .contains("paused"),
            "{body}"
        );
        assert_eq!(
            trial_issue(&app, &visitor, &agent).await.unwrap_err().0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        let challenge = trial_call(
            &app,
            "POST",
            &format!("/api/v3/embed/{agent}/challenge"),
            ("none", ""),
            json!({"origin":"https://example.com","code_challenge":"a".repeat(43)}),
        )
        .await;
        assert_eq!(challenge.0, StatusCode::SERVICE_UNAVAILABLE);
        // Settings stay and can still be saved; turning one back on is refused.
        let (status, page) = trial_call(
            &app,
            "GET",
            &embed_uri,
            ("cookie", &owner_cookie),
            Value::Null,
        )
        .await;
        assert_eq!(
            (status, page["config"]["enabled"].clone()),
            (StatusCode::OK, json!(true))
        );
        assert_eq!(page["widget_plan"]["state"], "ended");
        assert_eq!(turn(3, true).await.0, StatusCode::OK);
        assert_eq!(turn(4, false).await.0, StatusCode::OK);
        let (status, refused) = turn(5, true).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(refused["error"]
            .as_str()
            .unwrap_or_default()
            .contains("trial has ended"));
        assert_eq!(turn(5, true).await.0, StatusCode::CONFLICT);
        assert_eq!(
            trial_call(
                &app,
                "PUT",
                &format!("/api/v3/agents/{second}/embed"),
                ("cookie", &owner_cookie),
                config(1, true)
            )
            .await
            .0,
            StatusCode::OK
        );
        assert!(app
            .store
            .for_user(&visitor)
            .await
            .unwrap()
            .get("session:trial-chat")
            .await
            .unwrap()
            .is_some());

        // Plus from the administrator's second sign-in: the widget's next request serves, and
        // the owner may turn widgets on again.
        let set_tier = |tier: &'static str| {
            let app = app.clone();
            let (owner, admin) = (owner.clone(), admin_cookie.clone());
            async move {
                let current = app.store.account_by_id(&owner).await.unwrap().unwrap();
                trial_call(&app, "PUT", &format!("/api/v3/admin/access/accounts/{owner}"), ("cookie", &admin),
                    json!({"expected_revision":current["access_revision"],"status":"active","tier_id":tier})).await.0
            }
        };
        // The second agent's widget stayed on through the pause; nothing about it changes here.
        app.store
            .set_agent_membership(&visitor, &second, crate::access::Relationship::Caller, 0)
            .await
            .unwrap();
        assert_eq!(
            trial_me(&app, &visitor, &second).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(set_tier("plus").await, StatusCode::OK);
        assert_eq!(trial_me(&app, &visitor, &second).await.0, StatusCode::OK);
        assert!(trial_issue(&app, &visitor, &second).await.is_ok());
        assert_eq!(turn(5, true).await.0, StatusCode::OK);
        assert!(!public(descriptor_uri.clone()).await);
        assert_eq!(trial_me(&app, &visitor, &agent).await.0, StatusCode::OK);
        // A widget's owner cannot change its own plan.
        let visitor_cookie = trial_sign_in(&app, &visitor).await;
        assert_eq!(
            trial_call(
                &app,
                "PUT",
                &format!("/api/v3/admin/access/accounts/{owner}"),
                ("cookie", &visitor_cookie),
                json!({"expected_revision":1,"status":"active","tier_id":"free"})
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );

        // Plus ends: paused again at once, and no second trial.
        assert_eq!(set_tier("free").await, StatusCode::OK);
        assert_eq!(
            trial_me(&app, &visitor, &second).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            trial_me(&app, &visitor, &agent).await.0,
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(public(descriptor_uri.clone()).await);
        assert_eq!(turn(6, false).await.0, StatusCode::OK);
        assert_eq!(turn(7, true).await.0, StatusCode::CONFLICT);
        assert!(
            app.store
                .accounts_repository()
                .widget_trial_started(&owner)
                .await
                .unwrap()
                .unwrap()
                < now() - 30 * DAY
        );
        assert_eq!(trial_due(&app, &owner).await, None);

        // An administrator-owned agent's widget has no trial and serves.
        let admin = json!({"_user_id":"admin"});
        config_put(
            State(app.clone()),
            Extension(admin.clone()),
            Path("shared_default".into()),
            Json(config(0, true)),
        )
        .await
        .unwrap();
        assert!(
            !public(
                "/api/v3/embed/shared_default/descriptor?origin=https%3A%2F%2Fexample.com".into()
            )
            .await
        );
        assert_eq!(
            trial_me(&app, &visitor, "shared_default").await.0,
            StatusCode::OK
        );
        assert_eq!(
            app.store
                .accounts_repository()
                .widget_trial_started("admin")
                .await
                .unwrap(),
            None
        );
        let Json(page) = config_get(
            State(app.clone()),
            Extension(admin),
            Path("shared_default".into()),
        )
        .await
        .unwrap();
        assert_eq!(
            page["widget_plan"],
            json!({"plan":"included","serves":true})
        );
    }
}

#[cfg(test)]
mod shared_scope_tests {
    use super::*;
    #[tokio::test]
    async fn shared_widget_sessions_runs_approvals_and_invites_keep_exact_agent_scope() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let account = app
            .store
            .create_user_account("widget-participant", "fixture-long-password-hash")
            .await
            .unwrap();
        let actor = account["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(&actor, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        app.store
            .set_agent_membership(
                &actor,
                "shared_default",
                crate::access::Relationship::Caller,
                0,
            )
            .await
            .unwrap();
        app.store
            .put(
                "session:widget-shared",
                &json!({"id":"widget-shared","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        app.store
            .register_shared_session("admin", "widget-shared", "shared_default")
            .await
            .unwrap();
        app.store
            .shared_join("widget-shared", &actor)
            .await
            .unwrap();
        app.store.put("run:widget-shared-run",&json!({"id":"widget-shared-run","session_id":"widget-shared","agent_id":"shared_default","status":"waiting_approval","pending":{"id":"widget-approval"}})).await.unwrap();
        let mut caller = app.clone();
        caller.store = app.store.for_user(&actor).await.unwrap();
        let doc = json!({"agent_id":"shared_default"});
        for path in [
            "/api/v3/sessions/widget-shared",
            "/api/v3/sessions/widget-shared/events",
            "/api/v3/runs/widget-shared-run/cancel",
            "/api/v3/approvals/widget-approval",
        ] {
            let mut request = Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap();
            assert!(
                scope_request(&caller, &doc, &mut request).await.is_ok(),
                "{path}"
            );
        }
        let mut request = Request::builder()
            .uri("/api/v3/sessions/widget-shared")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(
            scope_request(&caller, &json!({"agent_id":"another-agent"}), &mut request)
                .await
                .is_err()
        );
        let other = app
            .store
            .create_user_account("widget-outsider", "fixture-long-password-hash")
            .await
            .unwrap();
        let other = other["user_id"].as_str().unwrap();
        app.store
            .update_account_access(other, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let mut outsider = app.clone();
        outsider.store = app.store.for_user(other).await.unwrap();
        let mut request = Request::builder()
            .uri("/api/v3/sessions/widget-shared")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(scope_request(&outsider, &doc, &mut request).await.is_err());
        for (path, ok) in [
            ("/api/v3/agents/shared_default/access-summary", true),
            ("/api/v3/agents/another-agent/access-summary", false),
            ("/api/v3/agents/shared_default/members", false),
        ] {
            let mut request = Request::builder()
                .uri(path)
                .body(axum::body::Body::empty())
                .unwrap();
            assert_eq!(
                scope_request(&caller, &doc, &mut request).await.is_ok(),
                ok,
                "{path}"
            );
        }
    }
    /// The widget allowlist is enforced from the matched route before any handler runs: every
    /// registered route and method off the list is refused, every one on it passes the route
    /// check, and a disabled or unapproved guest is refused on every listed route.
    #[tokio::test]
    async fn widget_credentials_reach_only_allowlisted_routes() {
        use crate::api::credential_routes::{tests::registered, widget_may, ROUTES};
        use crate::api::router;
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        app.store.set_registration_policy("open").unwrap();
        config_put(State(app.clone()),Extension(json!({"_user_id":"admin"})),Path("shared_default".into()),Json(json!({"expected_revision":0,"config":{"enabled":true,"guest_access":true,"allowed_origins":["https://example.com"]}}))).await.unwrap();
        let issued = guest(
            State(app.clone()),
            Path("shared_default".into()),
            None,
            HeaderMap::new(),
            Json(json!({"origin":"https://example.com"})),
        )
        .await
        .unwrap()
        .0;
        let token = issued["token"].as_str().unwrap().to_owned();
        let user = issued["user"]["user_id"].as_str().unwrap().to_owned();
        // `{id}` → a fixture value; a run action must name a real action.
        let concrete = |pattern: &str| {
            pattern
                .split('/')
                .map(|part| match part {
                    "{action}" => "cancel",
                    part if part.starts_with('{') => "fixture-id",
                    part => part,
                })
                .collect::<Vec<_>>()
                .join("/")
        };
        let call = |method: &str, path: &str| {
            let app = app.clone();
            let request = Request::builder()
                .method(method)
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap();
            async move { router(app).oneshot(request).await.unwrap() }
        };
        let route_refused = |response: axum::response::Response| async move {
            if response.status() != StatusCode::FORBIDDEN {
                return false;
            }
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            String::from_utf8_lossy(&body).contains("Not available in the embedded chat")
        };
        // Refused side: every signed-in route and registered method that is not on the list.
        let mut refused = 0;
        for (path, (methods, signed)) in registered() {
            for method in methods.iter().filter(|_| signed) {
                let method = axum::http::Method::from_bytes(method.as_bytes()).unwrap();
                if widget_may(&method, Some(&path)) {
                    continue;
                }
                let response = call(method.as_str(), &concrete(&path)).await;
                assert!(
                    route_refused(response).await,
                    "{method} {path} was not refused"
                );
                refused += 1;
            }
        }
        assert!(refused > 300, "only {refused} routes were tried");
        for (method, path) in [("POST", "/api/v3/feedback"), ("GET", "/api/v3/auth/me")] {
            assert!(
                route_refused(call(method, path).await).await,
                "{method} {path}"
            );
        }
        // Allowed side: every listed route and method passes the route check (the handler or
        // the agent binding may still answer for its own reasons), and the basics succeed.
        let listed: Vec<(String, String)> = ROUTES
            .iter()
            .flat_map(|route| {
                route
                    .widget_methods()
                    .map(move |method| (method.to_owned(), concrete(route.path)))
            })
            .collect();
        assert_eq!(listed.len(), 34);
        for (method, path) in &listed {
            let response = call(method, path).await;
            assert_ne!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert!(
                !route_refused(response).await,
                "{method} {path} was refused"
            );
        }
        for (method, path) in [
            ("GET", "/api/v3/user/status"),
            ("GET", "/api/v3/sessions"),
            ("GET", "/api/v3/agents/shared_default/access-summary"),
        ] {
            assert_eq!(
                call(method, path).await.status(),
                StatusCode::OK,
                "{method} {path}"
            );
        }
        // Unapproved (pending), then disabled: refused on every listed route.
        for status in [
            crate::access::AccountStatus::Pending,
            crate::access::AccountStatus::Disabled,
        ] {
            let current = app.store.account_by_id(&user).await.unwrap().unwrap()["access_revision"]
                .as_u64()
                .unwrap();
            app.store
                .update_account_access(&user, current, status, crate::access::ANONYMOUS_TIER)
                .await
                .unwrap();
            for (method, path) in &listed {
                assert_eq!(
                    call(method, path).await.status(),
                    StatusCode::UNAUTHORIZED,
                    "{status:?} {method} {path}"
                );
            }
        }
    }
}
