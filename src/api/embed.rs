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
fn unauthorized() -> ApiError {
    ApiError(
        StatusCode::UNAUTHORIZED,
        "Embedded chat authentication required".into(),
    )
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
    Ok(Json(settings(&app, &id).await?))
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
    let value = json!({"revision":expected.checked_add(1).ok_or_else(||bad("Revision exhausted"))?,"config":config});
    admin_agent_view(&app, &id)
        .await?
        .store
        .put(&format!("agent:{id}:embed"), &value)
        .await?;
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
    let root = std::env::var("V3_UI_ROOT").unwrap_or_else(|_| "/opt/v3/ui".into());
    let root = if webagent_webapp::configuration::app_settings(std::path::Path::new(
        webagent_webapp::configuration::LIVE_SETTINGS,
    ))
    .is_ok_and(|s| s["development"]["live_ui_editing"] == true)
        && std::path::Path::new("/live-checkout/ui/embed/embed.html").is_file()
    {
        "/live-checkout/ui".into()
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
static EMBED_ATTEMPTS: std::sync::OnceLock<
    Mutex<HashMap<(PathBuf, String, std::net::IpAddr, &'static str), Vec<u64>>>,
> = std::sync::OnceLock::new();
async fn rate(
    app: &App,
    agent: &str,
    client: std::net::IpAddr,
    lane: &'static str,
) -> std::result::Result<(), ApiError> {
    // Independent agent/client/operation windows cannot consume native login admission.
    let key = (app.store.root.clone(), agent.to_owned(), client, lane);
    let mut roots = EMBED_ATTEMPTS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .await;
    roots.retain(|_, attempts| {
        attempts.retain(|t| now().saturating_sub(*t) < 60);
        !attempts.is_empty()
    });
    if roots.len() >= 16384 && !roots.contains_key(&key) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again in one minute".into(),
        ));
    }
    let attempts = roots.entry(key).or_default();
    if attempts.len() >= 30 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Try again in one minute".into(),
        ));
    }
    attempts.push(now());
    drop(roots);
    // These credentials/challenges are transient, bounded encrypted app records.
    for (prefix, limit) in [
        ("embed:challenge:", 512usize),
        ("embed:credential:", 2048usize),
    ] {
        let records = app.store.list(prefix).await?;
        let mut retained = 0;
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
            }
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
    app.store.put(&format!("embed:challenge:{challenge}"),&json!({"challenge_key":format!("embed:challenge:{challenge}"),"agent_id":id,"origin":origin,"revision":config["revision"],"proof":proof,"expires":expires})).await?;
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
    if config["revision"] != doc["revision"]
        || !app.store.live_access(user, agent).await?.admitted()
    {
        return Err(denied());
    }
    let principal = access::Principal::from_account(account)?;
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
    let client = crate::account_authentication::throttle_client(&app, peer, &headers)?;
    rate(&app, &id, client, "guest").await?;
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
        && app
            .store
            .live_access(user, agent)
            .await
            .is_ok_and(|a| a.admitted())
        && app
            .store
            .read_access_policy()
            .ok()
            .is_some_and(|p| registration::page_allowed(&p, "main", "chat", &principal))
}
fn scope_path(path: &str) -> bool {
    let path = path.strip_prefix("/api/v3").unwrap_or(path);
    matches!(
        path,
        "/auth/me"
            | "/auth/ui-config"
            | "/sessions"
            | "/sessions/search"
            | "/sessions/stats"
            | "/user/status"
            | "/chat-invitations/redeem"
    ) || path.starts_with("/sessions/")
        || path.starts_with("/runs/")
        || path.starts_with("/approvals/")
        || wiki_path(path)
}
/// The Wiki reader opens articles the agent linked; the visitor's own access applies.
fn wiki_path(path: &str) -> bool {
    path == "/wiki" || path.starts_with("/wiki/")
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
    // The panel reads the bound agent's privacy mode before a widget's first message.
    let summary = path == format!("/agents/{agent}/access-summary");
    if !(scope_path(&path) || summary)
        || path.contains("/promotions")
        || path.ends_with("/source-review")
        || path.ends_with("/state-inspect")
        // Embedded chats only read the Wiki; changes go through the agent's tools.
        || (wiki_path(&path) && request.method() != axum::http::Method::GET)
    {
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
    if path == "/chat-invitations/redeem" {
        let body = std::mem::take(request.body_mut());
        let bytes = axum::body::to_bytes(body, 2 * 1024 * 1024)
            .await
            .map_err(|_| bad("Invalid invitation request"))?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| bad("Invalid invitation request"))?;
        let token = value["token"].as_str().ok_or_else(denied)?;
        let sid = app
            .store
            .shared_invite_session_for(token, actor)
            .await?
            .ok_or_else(denied)?;
        let directory = app.store.shared_directory(&sid).await?.ok_or_else(denied)?;
        if directory["agent_id"] != agent {
            return Err(denied());
        }
        *request.body_mut() = axum::body::Body::from(bytes);
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
        return Err(unauthorized());
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
    doc["_auth_key"] = json!(auth_key);
    doc["_user_id"] = json!(user);
    doc["_role"] = account["role"].clone();
    doc["_tier_id"] = account["tier_id"].clone();
    doc["_access_revision"] = account["access_revision"].clone();
    doc["_embed_agent"] = doc["agent_id"].clone();
    request.extensions_mut().insert(caller);
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
    #[test]
    fn embed_is_chat_scoped() {
        for p in [
            "/api/v3/sessions",
            "/sessions/a/events",
            "/runs/a/cancel",
            "/approvals/a/decision",
            "/auth/me",
            "/api/v3/wiki/articles/lessons",
            "/wiki/search",
        ] {
            assert!(scope_path(p));
        }
        for p in [
            "/admin/access",
            "/agents/a/model",
            "/auth/logout",
            "/models/me/catalog",
            "/messages",
            "/sessions-other",
            "/wikis",
        ] {
            assert!(!scope_path(p));
        }
    }
    #[tokio::test]
    async fn pkce_credentials_scope_replay_and_live_revocation() {
        use axum::body::Body;
        use tower::ServiceExt;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let source_key = format!("auth:{}", digest(&json!(uuid())));
        let source = json!({"generation":"fixture-generation","expires":now()+3600});
        app.store.put(&source_key, &source).await.unwrap();
        let identity = json!({"_user_id":"admin","generation":"fixture-generation","_auth_key":source_key,"expires":now()+3600});
        let config = json!({"expected_revision":0,"config":{"enabled":true,"allowed_origins":["https://example.com"]}});
        config_put(
            State(app.clone()),
            Extension(identity.clone()),
            Path("shared_default".into()),
            Json(config.clone()),
        )
        .await
        .unwrap();
        assert!(config_put(
            State(app.clone()),
            Extension(identity.clone()),
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
        app.store
            .put(
                "session:000-foreign",
                &json!({"id":"000-foreign","agent_id":"different","title":"Needle foreign"}),
            )
            .await
            .unwrap();
        app.store
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
        // Even an administrator's embed credential cannot reach admin routes.
        assert_eq!(
            router(app.clone())
                .oneshot(request("GET", "/api/v3/status", Value::Null))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        config_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(json!({"expected_revision":1,"config":{"enabled":false,"allowed_origins":["https://example.com"]}}))).await.unwrap();
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
            .update_account_access(
                &actor,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
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
            .update_account_access(
                other,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        let mut outsider = app.clone();
        outsider.store = app.store.for_user(other).await.unwrap();
        let mut request = Request::builder()
            .uri("/api/v3/sessions/widget-shared")
            .body(axum::body::Body::empty())
            .unwrap();
        assert!(scope_request(&outsider, &doc, &mut request).await.is_err());
        let invite = app
            .store
            .shared_invite_issue("widget-shared", "admin", "widget-invite", 86400)
            .await
            .unwrap();
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v3/chat-invitations/redeem")
            .body(axum::body::Body::from(
                json!({"token":invite["token"],"client_request_id":"widget-redeem"}).to_string(),
            ))
            .unwrap();
        assert!(scope_request(&caller, &doc, &mut request).await.is_ok());
        let retained = axum::body::to_bytes(request.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&retained).unwrap()["token"],
            invite["token"]
        );
        let mut request = Request::builder()
            .method("POST")
            .uri("/api/v3/chat-invitations/redeem")
            .body(axum::body::Body::from(
                json!({"token":invite["token"],"client_request_id":"widget-redeem"}).to_string(),
            ))
            .unwrap();
        assert!(
            scope_request(&caller, &json!({"agent_id":"another-agent"}), &mut request)
                .await
                .is_err()
        );
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
}
