//! Thin HTTP adapters preserve the existing host State extractor and authenticated identity.
use crate::*;
use axum::{extract::Path, Extension, Json};
pub(crate) async fn login(
    State(app): State<App>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::authentication_api::login(
        &app.store.accounts_repository(),
        &app,
        &app.login_attempts,
        &headers,
        &body,
        app.secure,
        webagent_accounts::AdmissionContext {
            now: now(),
            client: throttle_client(&app, peer, &headers)?,
        },
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn setup_status(State(app): State<App>) -> Api {
    webagent_accounts::authentication_api::setup_status(&app.store.accounts_repository())
        .await
        .map_err(Into::into)
}
pub(crate) async fn setup_admin(
    State(app): State<App>,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::authentication_api::setup_admin(
        &app.store.accounts_repository(),
        &app,
        &app.login_attempts,
        &headers,
        &body,
        app.secure,
        webagent_accounts::AdmissionContext {
            now: now(),
            client: throttle_client(&app, peer, &headers)?,
        },
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn me(State(app): State<App>, Extension(doc): Extension<Value>) -> Api {
    webagent_accounts::authentication_api::me(&app.store.accounts_repository(), &app, &doc)
        .await
        .map_err(Into::into)
}
pub(crate) async fn update_profile(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    webagent_accounts::self_service::update_profile(&app.store.accounts_repository(), &doc, &body)
        .await
        .map_err(Into::into)
}
pub(crate) async fn session_policy(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
) -> Api {
    webagent_accounts::self_service::session_policy(&app.store.accounts_repository(), &doc)
        .await
        .map_err(Into::into)
}
pub(crate) async fn update_session_policy(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    webagent_accounts::self_service::update_session_policy(
        &app.store.accounts_repository(),
        &doc,
        &body,
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn list_auth_sessions(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
) -> Api {
    webagent_accounts::self_service::list_auth_sessions(
        &app.store.accounts_repository(),
        &doc,
        now(),
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn revoke_auth_session(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    webagent_accounts::self_service::revoke_auth_session(
        &app.store.accounts_repository(),
        &doc,
        &id,
        now(),
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn change_password(
    State(app): State<App>,
    Extension(doc): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    webagent_accounts::self_service::change_password(
        &app.store.accounts_repository(),
        &app,
        &doc,
        &body,
        &app.login_attempts,
        now(),
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn logout(
    State(app): State<App>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::authentication_api::logout(&app.store.accounts_repository(), &headers)
        .await
        .map_err(Into::into)
}
pub(crate) async fn diagnostics_browser_login(
    State(app): State<App>,
    headers: HeaderMap,
    Json(body): Json<webagent_accounts::browser_login_api::LoginRequest>,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::browser_login_api::login(
        &app.store.accounts_repository(),
        &app,
        app.browser_login_bridge.as_ref(),
        &headers,
        body,
        app.secure,
        now(),
    )
    .await
    .map_err(Into::into)
}

pub(crate) fn throttle_client(
    app: &App,
    peer: Option<Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: &HeaderMap,
) -> std::result::Result<std::net::IpAddr, ApiError> {
    let settings = crate::store::read_app_settings(&app.store.root).map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication configuration unavailable".into(),
        )
    })?;
    webagent_accounts::client_ip(peer.map(|p| p.0 .0), headers, &settings).map_err(|_| {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Client identity unavailable".into(),
        )
    })
}
