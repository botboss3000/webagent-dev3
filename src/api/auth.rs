//! Authentication middleware composes Accounts identity with live Access authorization.
use super::*;
pub(crate) use crate::account_authentication::{
    change_password, list_auth_sessions, login, logout, me, revoke_auth_session, session_policy,
    setup_admin, setup_status, update_profile, update_session_policy,
};
pub(crate) use webagent_accounts::message_api_token_hash;
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
    let authorized = app
        .store
        .account_by_id("admin")
        .await
        .ok()
        .flatten()
        .and_then(|account| {
            access::Principal::from_account(&account)
                .ok()
                .map(|principal| {
                    principal.active()
                        && principal.administrator()
                        && account["is_approved"] == true
                })
        })
        .unwrap_or(false);
    if !authorized {
        return ApiError(StatusCode::UNAUTHORIZED, "Messaging API unavailable".into())
            .into_response();
    }
    let mut caller = app.clone();
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
        return match super::embed::bearer(&app, &mut request).await {
            Ok(()) => next.run(request).await,
            Err(error) => error.into_response(),
        };
    }

    let token = webagent_accounts::session_token(&headers);
    let candidate = match app
        .store
        .accounts_repository()
        .inspect_session(token, now())
        .await
    {
        Ok(Some(candidate)) => candidate,
        _ => return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response(),
    };
    let principal = match access::Principal::from_account(candidate.account_state()) {
        Ok(principal) if principal.active() => principal,
        _ => return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response(),
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
    let Some(session) = candidate.finish() else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response();
    };
    let user_id = session.identity.user_id;
    let mut doc = session.document;
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
    doc["_auth_key"] = json!(session.key);
    doc["_user_id"] = json!(user_id);
    doc["_role"] = json!(principal.role);
    doc["_tier_id"] = json!(principal.tier_id);
    doc["_access_revision"] = json!(principal.access_revision);
    let mut caller = app.clone();
    caller.store = match app.store.for_user(&user_id).await {
        Ok(store) => store,
        Err(_) => {
            return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response()
        }
    };
    request.extensions_mut().insert(caller);
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
pub(crate) async fn admin_auth(State(app): State<App>, request: Request, next: Next) -> Response {
    let Some(identity) = request.extensions().get::<Value>() else {
        return ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()).into_response();
    };
    if let Err(error) = phase3_api::administrator(&app, identity).await {
        return error.into_response();
    }
    next.run(request).await
}
pub(crate) async fn chat_ui_config(State(app): State<App>) -> Api {
    webagent_webapp::configuration::ui_config(&app.chat_ui_config_path, &app.store.root)
        .await
        .map(Json)
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
    async fn ui_config_adapter_delegates_and_preserves_error_status() {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        app.chat_ui_config_path = root.path().join("adapter-chat-ui.json");
        std::fs::write(&app.chat_ui_config_path, br#"{"chat_common":{}}"#).unwrap();
        // Domain schema/default/privacy assertions live in Webapp; this verifies host wiring.
        let expected =
            webagent_webapp::configuration::ui_config(&app.chat_ui_config_path, &app.store.root)
                .await
                .unwrap();
        assert_eq!(
            chat_ui_config(State(app.clone())).await.unwrap().0,
            expected
        );
        std::fs::write(&app.chat_ui_config_path, "{}").unwrap();
        assert_eq!(
            chat_ui_config(State(app.clone())).await.err().unwrap().0,
            StatusCode::INTERNAL_SERVER_ERROR
        );
        std::fs::remove_file(&app.chat_ui_config_path).unwrap();
        assert_eq!(
            chat_ui_config(State(app)).await.err().unwrap().0,
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
