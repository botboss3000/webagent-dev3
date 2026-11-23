//! Authentication middleware composes Accounts identity with live Access authorization.
use super::{
    access, json, now, phase3_api, registration, Api, ApiError, App, HashMap, HeaderMap,
    HeaderValue, Json, Next, Query, Request, Response, State, StatusCode, Value,
};
pub(crate) use crate::account_authentication::{
    change_password, list_auth_sessions, login, logout, me, revoke_auth_session, session_policy,
    setup_admin, setup_status, update_profile, update_session_policy,
};
use axum::response::IntoResponse;
use std::sync::Arc;
pub(crate) use webagent_accounts::message_api_token_hash;
use webagent_accounts_host::AccountsStore;
pub(crate) fn message_api_authorized(expected: Option<&[u8; 32]>, headers: &HeaderMap) -> bool {
    webagent_accounts::message_api_authorized(
        expected,
        headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
    )
}
pub(crate) async fn message_api_auth(
    State(app): State<App>,
    mut request: Request,
    next: Next,
) -> Response {
    if !message_api_authorized(app.message_api_token_hash.as_ref(), request.headers()) {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            "Messaging API authentication required".into(),
        )
        .into_response();
    }
    let account = app.store.account_by_id("admin").await.ok().flatten();
    let identity = account.as_ref().and_then(|account| {
        let principal = access::Principal::from_account(account).ok()?;
        (principal.active() && principal.administrator() && account["is_approved"] == true).then(
            || {
                Arc::new(access::identity::Identity {
                    principal,
                    username: account["username"].as_str().unwrap_or_default().to_owned(),
                    generation: account["generation"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                    auth_epoch: None,
                    kind: access::identity::CredentialKind::MessagingBearer,
                    credential_key: String::new(),
                    credential_record: Vec::new(),
                    agent_id: None,
                })
            },
        )
    });
    let Some(identity) = identity else {
        return ApiError(StatusCode::UNAUTHORIZED, "Messaging API unavailable".into())
            .into_response();
    };
    let mut caller = app.clone();
    caller.identity = Some(identity);
    caller.store = match app.store.for_user("admin").await {
        Ok(store) => store,
        Err(_) => {
            return ApiError(StatusCode::UNAUTHORIZED, "Messaging API unavailable".into())
                .into_response()
        }
    };
    request.extensions_mut().insert(caller);
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response
}
pub(crate) async fn auth(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    if let Some(identity) =
        super::notifications::bearer_identity(&app, &method, &path, &headers).await
    {
        request.extensions_mut().insert(app.clone());
        request.extensions_mut().insert(identity);
        return next.run(request).await;
    }
    if request.headers().contains_key("authorization") {
        if let Err(error) = super::embed::bearer(&app, &mut request).await {
            return error.into_response();
        }
        // Embedded chat visitors send messages too: they share the per-user caps.
        let user = request
            .extensions()
            .get::<Value>()
            .and_then(|doc| doc["_user_id"].as_str())
            .unwrap_or("")
            .to_owned();
        if let Some(refused) = crate::request_limits::signed_in(&app, &user, &method, &path).await {
            return refused;
        }
        return next.run(request).await;
    }

    let token = webagent_accounts::session_token(&headers);
    let at = now();
    let known = match webagent_accounts::session_key(token) {
        Some(key) => remembered(&app, &key, at).await,
        None => None,
    };
    // A sign-in confirmed unchanged this request, or one read and checked in full.
    let (principal, signed) = match known {
        Some(known) => (known.identity.principal.clone(), Some(known)),
        None => {
            let candidate = match app
                .store
                .accounts_repository()
                .inspect_session(token, at)
                .await
            {
                Ok(Some(candidate)) => candidate,
                _ => {
                    return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into())
                        .into_response()
                }
            };
            let principal = match access::Principal::from_account(candidate.account_state()) {
                Ok(principal) if principal.active() => principal,
                _ => {
                    return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into())
                        .into_response()
                }
            };
            let epoch = candidate.auth_epoch();
            let record = candidate.record().to_vec();
            let signed = candidate
                .finish()
                .map(|session| remember(&app, session, record, principal.clone(), epoch, at));
            (principal, signed)
        }
    };
    if principal.role == access::Role::Guest
        && registration::mode(&app).await.unwrap_or_default() != "public_registered"
    {
        return ApiError(
            StatusCode::UNAUTHORIZED,
            "Guest access is not enabled".into(),
        )
        .into_response();
    }
    if let Some(page) = access::configuration::main_page_for_request(request.uri().path()) {
        if !app.store.read_access_policy().ok().is_some_and(|policy| {
            access::configuration::page_allowed(&policy, "main", page, &principal)
        }) {
            return ApiError(StatusCode::FORBIDDEN, "Page access denied".into()).into_response();
        }
    }
    let Some(signed) = signed else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response();
    };
    let identity = signed.identity.clone();
    let user_id = identity.user_id().to_owned();
    let mut doc = signed.document.clone();
    if !webagent_accounts::valid_csrf(
        method.as_str(),
        doc["csrf"].as_str().unwrap_or(""),
        headers
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok()),
    ) {
        return ApiError(StatusCode::FORBIDDEN, "Invalid request token".into()).into_response();
    }
    if !matches!(method.as_str(), "GET" | "HEAD" | "OPTIONS")
        && !webagent_accounts::valid_origin(
            headers.get("origin").and_then(|value| value.to_str().ok()),
            headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .unwrap_or(""),
            app.secure,
        )
    {
        return ApiError(StatusCode::FORBIDDEN, "Invalid request origin".into()).into_response();
    }
    // Rules of the credential kind, before any handler runs.
    if let Some(refused) = identity.kind.refuses_method(method.as_str()) {
        return ApiError(StatusCode::FORBIDDEN, refused.into()).into_response();
    }
    // A test account only through its test sign-in, and that sign-in only while the
    // administrator run that made it is still going (checked on every request).
    if !identity.kind_matches_account()
        || (identity.kind == access::identity::CredentialKind::TestPersona
            && !test_persona_live(&app, &signed.document).await)
    {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response();
    }
    doc["_credential"] = json!(identity.kind.name());
    doc["_auth_key"] = json!(identity.credential_key);
    doc["_user_id"] = json!(user_id);
    doc["_role"] = json!(identity.effective_role());
    doc["_tier_id"] = json!(principal.tier_id);
    doc["_access_revision"] = json!(principal.access_revision);
    let mut caller = app.clone();
    // This request read the account row (or its epoch) just now, so it still exists.
    caller.store = match app.store.for_identity(&identity).await {
        Ok(store) => store,
        Err(_) => {
            return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response()
        }
    };
    caller.identity = Some(identity.clone());
    if let Some(refused) = crate::request_limits::signed_in(&app, &user_id, &method, &path).await {
        return refused;
    }
    request.extensions_mut().insert(caller);
    request.extensions_mut().insert(identity);
    request.extensions_mut().insert(doc);
    let mutation = !matches!(
        *request.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        crate::wake::notify(&app).await;
    }
    response
}
/// Whether a test-persona sign-in's creating run is still going: one indexed status read in
/// the creator's store, no decryption. Fails closed (missing creator, run or status = ended).
pub(crate) async fn test_persona_live(app: &App, record: &Value) -> bool {
    let (Some(creator), Some(run)) = (
        record["creator_user_id"].as_str(),
        record["creator_run_id"].as_str(),
    ) else {
        return false;
    };
    let Ok(store) = app.store.for_user(creator).await else {
        return false;
    };
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM run_index WHERE id=?")
        .bind(run)
        .fetch_optional(&store.pool)
        .await
        .ok()
        .flatten();
    status.is_some_and(|status| !webagent_engine::runtime::failure::terminal(&status))
}
/// A browser sign-in whose identity was built from its records, kept while they are unchanged.
struct Remembered {
    identity: Arc<access::identity::Identity>,
    document: Value,
    expires: u64,
}
type RememberedKey = (std::path::PathBuf, String);
/// Remembered sign-ins of this process, keyed by store root and sign-in record. An entry is
/// used only after this request's indexed read shows the account's `auth_epoch` unchanged and
/// the sign-in record still present with the same sealed bytes; nothing is reused on time alone. The bound only costs
/// rebuilds: a full map is emptied.
static REMEMBERED: std::sync::LazyLock<std::sync::Mutex<HashMap<RememberedKey, Arc<Remembered>>>> =
    std::sync::LazyLock::new(Default::default);
const REMEMBERED_LIMIT: usize = 4096;

/// The remembered sign-in for `key` when its records are unchanged (one indexed read).
async fn remembered(app: &App, key: &str, at: u64) -> Option<Arc<Remembered>> {
    let slot = (app.store.root.clone(), key.to_owned());
    let known = REMEMBERED.lock().ok()?.get(&slot).cloned()?;
    let current = app
        .store
        .accounts_repository()
        .sign_in_epoch(
            known.identity.user_id(),
            key,
            &known.identity.credential_record,
        )
        .await
        .ok()
        .flatten();
    if known.expires > at && current.is_some() && current == known.identity.auth_epoch {
        return Some(known);
    }
    if let Ok(mut map) = REMEMBERED.lock() {
        map.remove(&slot);
    }
    None
}

/// The identity of a sign-in checked in full; remembered when its account has an epoch.
fn remember(
    app: &App,
    session: webagent_accounts::AuthenticatedSession,
    record: Vec<u8>,
    principal: access::Principal,
    auth_epoch: Option<i64>,
    at: u64,
) -> Arc<Remembered> {
    let identity = access::identity::Identity {
        principal,
        username: session.identity.username,
        generation: session.document["generation"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        auth_epoch,
        kind: access::identity::CredentialKind::of_sign_in(&session.document),
        credential_key: session.key.clone(),
        credential_record: record,
        agent_id: session.document["agent_id"].as_str().map(str::to_owned),
    };
    let entry = Arc::new(Remembered {
        expires: session.document["expires"].as_u64().unwrap_or(0),
        identity: Arc::new(identity),
        document: session.document,
    });
    // Fail closed: an account without an epoch is checked in full on every request.
    if auth_epoch.is_some() {
        if let Ok(mut map) = REMEMBERED.lock() {
            if map.len() >= REMEMBERED_LIMIT {
                map.retain(|_, known| known.expires > at);
                if map.len() >= REMEMBERED_LIMIT {
                    map.clear();
                }
            }
            map.insert((app.store.root.clone(), session.key), entry.clone());
        }
    }
    entry
}

/// The `auth_epoch` of the remembered identity for a cookie value, if one is remembered.
#[cfg(test)]
pub(crate) fn remembered_epoch(app: &App, token: &str) -> Option<i64> {
    let key = webagent_accounts::session_key(token)?;
    REMEMBERED
        .lock()
        .ok()?
        .get(&(app.store.root.clone(), key))
        .and_then(|known| known.identity.auth_epoch)
}

pub(crate) async fn admin_auth(State(app): State<App>, request: Request, next: Next) -> Response {
    let Some(identity) = request.extensions().get::<Value>() else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response();
    };
    if let Err(error) = phase3_api::administrator(&app, identity).await {
        return error.into_response();
    }
    next.run(request).await
}
/// Base UI configuration; with `agent_id`, that agent's effective chat.json and component overlays.
pub(crate) async fn chat_ui_config(
    State(app): State<App>,
    Query(args): Query<HashMap<String, String>>,
) -> Api {
    let mut value = ui_config_value(&app).await?;
    if let Some(agent) = args.get("agent_id") {
        // Agent overlays are served only to callers admitted to use that agent (session-creation authority).
        if !app
            .store
            .live_access(app.store.current_user_id(), agent)
            .await
            .is_ok_and(|access| access.admitted())
        {
            return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
        }
        crate::chat_config::apply_to_ui_config(&app.store, agent, &mut value).await?;
    }
    Ok(Json(value))
}
/// `chat_ui_config` with an ETag of its exact bytes: tabs poll it every few seconds, so an
/// unchanged document answers 304 with no body. The response is per caller (agent overlays),
/// so it is `private, no-cache` (set by the router layer for responses with an ETag).
pub(crate) async fn chat_ui_config_revalidated(
    State(app): State<App>,
    query: Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    use sha2::Digest;
    let Json(value) = chat_ui_config(State(app), query).await?;
    let body = serde_json::to_vec(&value)
        .map_err(|error| ApiError(StatusCode::INTERNAL_SERVER_ERROR, error.to_string()))?;
    let digest = sha2::Sha256::digest(&body);
    let etag = format!("\"{}\"", hex::encode(&digest[..16]));
    let matched = headers
        .get("if-none-match")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|tags| {
            tags.split(',')
                .map(|tag| tag.trim().trim_start_matches("W/"))
                .any(|tag| tag == etag || tag == "*")
        });
    let etag = HeaderValue::from_str(&etag).expect("hex ETag is a valid header value");
    if matched {
        return Ok((StatusCode::NOT_MODIFIED, [("etag", etag)]).into_response());
    }
    Ok((
        [
            ("etag", etag),
            ("content-type", HeaderValue::from_static("application/json")),
        ],
        body,
    )
        .into_response())
}
pub(crate) async fn ui_config_value(app: &App) -> std::result::Result<Value, ApiError> {
    webagent_webapp::configuration::ui_config(
        &app.chat_ui_config_path,
        std::path::Path::new(webagent_webapp::configuration::LIVE_SETTINGS),
    )
    .await
    .map_err(|error| match error {
        webagent_webapp::configuration::Error::Http(status, message) => {
            ApiError(status, message.into())
        }
        webagent_webapp::configuration::Error::Settings(error) => error.into(),
    })
}

#[cfg(test)]
mod webapp_adapter_tests {
    use super::*;

    #[tokio::test]
    async fn ui_config_answers_not_modified_for_its_own_etag() {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        app.chat_ui_config_path = root.path().join("etag-chat-ui.json");
        std::fs::write(&app.chat_ui_config_path, br#"{"chat_common":{}}"#).unwrap();
        let get = |tag: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(tag) = tag {
                headers.insert("if-none-match", HeaderValue::from_str(tag).unwrap());
            }
            chat_ui_config_revalidated(State(app.clone()), Query(HashMap::new()), headers)
        };
        let first = get(None).await.unwrap();
        assert_eq!(first.status(), StatusCode::OK);
        let etag = first.headers()["etag"].to_str().unwrap().to_owned();
        assert_eq!(
            get(Some(&etag)).await.unwrap().status(),
            StatusCode::NOT_MODIFIED
        );
        assert_eq!(
            get(Some("\"other\"")).await.unwrap().status(),
            StatusCode::OK
        );
        std::fs::write(&app.chat_ui_config_path, br#"{"chat_common":{"x":1}}"#).unwrap();
        let changed = get(Some(&etag)).await.unwrap();
        assert_eq!(changed.status(), StatusCode::OK);
        assert_ne!(changed.headers()["etag"], etag.as_str());
    }

    #[tokio::test]
    async fn ui_config_adapter_delegates_and_preserves_error_status() {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        app.chat_ui_config_path = root.path().join("adapter-chat-ui.json");
        std::fs::write(&app.chat_ui_config_path, br#"{"chat_common":{}}"#).unwrap();
        // Domain schema/default/privacy assertions live in Webapp; this verifies host wiring.
        let expected = webagent_webapp::configuration::ui_config(
            &app.chat_ui_config_path,
            std::path::Path::new(webagent_webapp::configuration::LIVE_SETTINGS),
        )
        .await
        .unwrap();
        assert_eq!(
            chat_ui_config(State(app.clone()), Query(HashMap::new()))
                .await
                .unwrap()
                .0,
            expected
        );
        std::fs::write(&app.chat_ui_config_path, "{}").unwrap();
        assert_eq!(
            chat_ui_config(State(app.clone()), Query(HashMap::new()))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        // Agent selection requires use authority; overlays reach only the agent-scoped response.
        std::fs::write(&app.chat_ui_config_path, br#"{"chat_common":{"x":1}}"#).unwrap();
        let agent = |id: &str| Query(HashMap::from([("agent_id".to_owned(), id.to_owned())]));
        let view = app.store.for_agent("shared_default").await.unwrap();
        webagent_chat::agent_config::save(
            &view.agent_pool,
            &view,
            "shared_default",
            "chat",
            webagent_chat::agent_config::Change::Patch(&json!({"chat_common":{"x":2}})),
            0,
            &json!({}),
            "admin",
            1,
        )
        .await
        .unwrap();
        let scoped = chat_ui_config(State(app.clone()), agent("shared_default"))
            .await
            .unwrap()
            .0;
        assert_eq!(
            (
                scoped["chat_ui"]["chat_common"]["x"].clone(),
                scoped["components"]["session_dropdown"].clone()
            ),
            (json!(2), json!({}))
        );
        let plain = chat_ui_config(State(app.clone()), Query(HashMap::new()))
            .await
            .unwrap()
            .0;
        assert_eq!(
            (
                plain["chat_ui"]["chat_common"]["x"].clone(),
                plain.get("components")
            ),
            (json!(1), None)
        );
        assert_eq!(
            chat_ui_config(State(app.clone()), agent("missing-agent"))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::NOT_FOUND
        );
        let guest = app.store.create_guest_account().await.unwrap();
        let mut outsider = app.clone();
        outsider.store = app
            .store
            .for_user(guest["user_id"].as_str().unwrap())
            .await
            .unwrap();
        assert_eq!(
            chat_ui_config(State(outsider), agent("shared_default"))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::NOT_FOUND
        );
        std::fs::remove_file(&app.chat_ui_config_path).unwrap();
        assert_eq!(
            chat_ui_config(State(app), Query(HashMap::new()))
                .await
                .err()
                .unwrap()
                .0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
