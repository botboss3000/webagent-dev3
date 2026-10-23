use axum::{
    extract::{Path as RoutePath, Request, State},
    http::StatusCode,
    middleware::Next,
    response::Response,
    Json,
};
use serde_json::Value;
use std::path::Path;
use tower::ServiceExt;

pub(super) async fn live_ui(
    State(app): State<crate::App>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if matches!(path, "/" | "/app" | "/setup.html")
        || path == "/v3-ui"
        || path.starts_with("/v3-ui/")
    {
        if let Some(ui) = webagent_webapp::configuration::live_root(
            &app.store.root,
            Path::new("/live-checkout/ui"),
        ) {
            return webagent_webapp::pages::routes(&ui)
                .oneshot(request)
                .await
                .expect("infallible UI router");
        }
    }
    next.run(request).await
}
pub async fn catalog(State(app): State<crate::App>) -> Result<Json<Value>, (StatusCode, String)> {
    let ui =
        webagent_webapp::configuration::live_root(&app.store.root, Path::new("/live-checkout/ui"))
            .unwrap_or_else(|| {
                std::path::PathBuf::from(
                    std::env::var("V3_UI_ROOT").unwrap_or_else(|_| "/opt/v3/ui".into()),
                )
            });
    let packages = crate::modules::package_ids()
        .into_iter()
        .filter(|id| crate::modules::available(id))
        .filter_map(|id| crate::modules::package_root(&id).map(|root| (id.to_owned(), root)));
    webagent_webapp::pages::catalog(&ui, packages)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
pub(crate) async fn module_asset(
    axum::Extension(app): axum::Extension<crate::App>,
    RoutePath((id, name)): RoutePath<(String, String)>,
) -> Response {
    use axum::response::IntoResponse;
    if id == "outbound_notifications" {
        let allowed = app
            .store
            .account_by_id(crate::shared_chat::actor(&app))
            .await
            .ok()
            .flatten()
            .and_then(|account| crate::access::Principal::from_account(&account).ok())
            .is_some_and(|principal| principal.active() && principal.administrator());
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    if !crate::modules::available(&id) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(root) = crate::modules::package_root(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    webagent_webapp::pages::module_asset(&root, &name).await
}
