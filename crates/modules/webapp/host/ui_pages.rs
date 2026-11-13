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
use webagent_accounts_host::AccountsStore;

pub(super) async fn live_ui(
    State(app): State<crate::App>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if (path == "/app" || path.starts_with("/v3-ui/"))
        && request
            .headers()
            .get("cookie")
            .is_some_and(|c| c.to_str().is_ok_and(|c| c.contains(preview::COOKIE)))
    {
        if let Some(root) = preview::root_for(&app, request.headers()).await {
            if let Some(file) = preview::variant_file(&root, path) {
                return preview::serve(file, request).await;
            }
        }
    }
    let path = request.uri().path();
    if matches!(path, "/" | "/app" | "/setup.html")
        || path == "/v3-ui"
        || path.starts_with("/v3-ui/")
    {
        if let Some(ui) = webagent_webapp::configuration::live_root(
            Path::new(webagent_webapp::configuration::LIVE_SETTINGS),
            &webagent_configuration::paths::checkout().join("ui"),
        ) {
            return webagent_webapp::pages::routes(&ui)
                .oneshot(request)
                .await
                .expect("infallible UI router");
        }
    }
    next.run(request).await
}
pub async fn catalog(State(_app): State<crate::App>) -> Result<Json<Value>, (StatusCode, String)> {
    let ui = webagent_webapp::configuration::live_root(
        Path::new(webagent_webapp::configuration::LIVE_SETTINGS),
        &webagent_configuration::paths::checkout().join("ui"),
    )
    .unwrap_or_else(|| {
        std::path::PathBuf::from(std::env::var("V3_UI_ROOT").unwrap_or_else(|_| {
            webagent_configuration::paths::app()
                .join("ui")
                .to_string_lossy()
                .into_owned()
        }))
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

/// Draft UI previews: an administrator's browser sees a session workspace's variant files
/// over the live UI, so screenshots show the draft against the real API and sign-in.
mod preview {
    use super::*;
    use axum::{
        http::{header, HeaderMap, HeaderValue},
        response::{IntoResponse, Redirect},
    };
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{LazyLock, Mutex},
    };

    pub(super) const COOKIE: &str = "v3_ui_preview";
    const LIFETIME: u64 = 3600;

    #[derive(Clone)]
    struct Grant {
        user_id: String,
        session_id: String,
        variant: String,
        expires: u64,
    }

    /// Grants by random cookie token; a restart clears them, ending every preview.
    static GRANTS: LazyLock<Mutex<HashMap<String, Grant>>> = LazyLock::new(Default::default);

    pub(super) fn variant_ok(variant: &str) -> bool {
        (1..=32).contains(&variant.len())
            && variant
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
    }

    fn session_ok(sid: &str) -> bool {
        uuid::Uuid::parse_str(sid).is_ok_and(|id| id.hyphenated().to_string() == sid)
    }

    /// The file a UI request names under a variant's `ui/` folder, or `None` when the request
    /// is not a UI file or would leave the folder. Symlinks are never followed.
    pub(super) fn variant_file(root: &Path, request_path: &str) -> Option<PathBuf> {
        let relative = match request_path {
            "/app" => "index.html",
            path => path.strip_prefix("/v3-ui/")?,
        };
        let mut file = root.to_path_buf();
        for part in relative.split('/') {
            if part.is_empty() || part == "." || part == ".." || part.contains('\\') {
                return None;
            }
            file.push(part);
            if std::fs::symlink_metadata(&file)
                .ok()?
                .file_type()
                .is_symlink()
            {
                return None;
            }
        }
        file.is_file().then_some(file)
    }

    fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
        headers
            .get(header::COOKIE)?
            .to_str()
            .ok()?
            .split(';')
            .find_map(|pair| pair.trim().strip_prefix(name)?.strip_prefix('='))
    }

    /// The variant folder this request may see: a live grant whose user is the signed-in user.
    pub(super) async fn root_for(app: &crate::App, headers: &HeaderMap) -> Option<PathBuf> {
        let token = cookie_value(headers, COOKIE)?;
        let grant = {
            let mut grants = GRANTS.lock().ok()?;
            let now = webagent_host_core::now();
            grants.retain(|_, grant| grant.expires > now);
            grants.get(token)?.clone()
        };
        let candidate = app
            .store
            .accounts_repository()
            .inspect_session(
                webagent_accounts::session_token(headers),
                webagent_host_core::now(),
            )
            .await
            .ok()??;
        if candidate.finish()?.identity.user_id != grant.user_id {
            return None;
        }
        let workspaces = webagent_configuration::paths::workspaces();
        Some(
            Path::new(&workspaces)
                .join(&grant.session_id)
                .join("previews")
                .join(&grant.variant)
                .join("ui"),
        )
    }

    /// `GET /api/v3/sessions/{sid}/ui-preview/{variant}`: start previewing a variant of the
    /// caller's own session in this browser, or `off` to stop.
    pub(crate) async fn start(
        axum::Extension(app): axum::Extension<crate::App>,
        axum::Extension(identity): axum::Extension<Value>,
        RoutePath((sid, variant)): RoutePath<(String, String)>,
    ) -> Response {
        if let Err(error) = crate::phase3_api::administrator(&app, &identity).await {
            return error.into_response();
        }
        let clear = format!("{COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax");
        if variant == "off" {
            // Drop any draft copies the browser kept so the live UI shows at once.
            return (
                [
                    (header::SET_COOKIE, clear),
                    (
                        header::HeaderName::from_static("clear-site-data"),
                        "\"cache\"".into(),
                    ),
                ],
                Redirect::to("/app"),
            )
                .into_response();
        }
        if !session_ok(&sid) || !variant_ok(&variant) {
            return (StatusCode::BAD_REQUEST, "Invalid session or variant").into_response();
        }
        // The caller's own store finds only sessions the caller owns.
        match app
            .store
            .get(&webagent_host_core::key("session", &sid))
            .await
        {
            Ok(Some(_)) => {}
            _ => return (StatusCode::NOT_FOUND, "Session not found").into_response(),
        }
        let token = uuid::Uuid::new_v4().to_string();
        let grant = Grant {
            user_id: identity["_user_id"].as_str().unwrap_or_default().to_owned(),
            session_id: sid,
            variant,
            expires: webagent_host_core::now() + LIFETIME,
        };
        if grant.user_id.is_empty() {
            return (StatusCode::UNAUTHORIZED, "Sign in required").into_response();
        }
        if let Ok(mut grants) = GRANTS.lock() {
            grants.insert(token.clone(), grant);
        }
        let cookie =
            format!("{COOKIE}={token}; Path=/; Max-Age={LIFETIME}; HttpOnly; Secure; SameSite=Lax");
        ([(header::SET_COOKIE, cookie)], Redirect::to("/app")).into_response()
    }

    /// Serve the variant's copy of a UI file, never cached.
    pub(super) async fn serve(file: PathBuf, request: Request) -> Response {
        let mut response = tower_http::services::ServeFile::new(file)
            .oneshot(request)
            .await
            .map(IntoResponse::into_response)
            .unwrap_or_else(|_| StatusCode::NOT_FOUND.into_response());
        // Without validators a stored draft can never be revalidated as the (older) live file.
        let headers = response.headers_mut();
        headers.remove(header::ETAG);
        headers.remove(header::LAST_MODIFIED);
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn variant_files_stay_inside_their_folder() {
            let dir = tempfile::tempdir().expect("dir");
            std::fs::create_dir_all(dir.path().join("chat")).expect("folder");
            std::fs::write(dir.path().join("index.html"), "x").expect("file");
            std::fs::write(dir.path().join("chat/a.js"), "x").expect("file");
            assert!(variant_file(dir.path(), "/app").is_some());
            assert!(variant_file(dir.path(), "/v3-ui/chat/a.js").is_some());
            assert!(variant_file(dir.path(), "/v3-ui/chat/missing.js").is_none());
            assert!(variant_file(dir.path(), "/v3-ui/../index.html").is_none());
            assert!(variant_file(dir.path(), "/ui/shared/x.css").is_none());
            std::os::unix::fs::symlink("/etc/hostname", dir.path().join("link")).expect("link");
            assert!(variant_file(dir.path(), "/v3-ui/link").is_none());
            assert!(variant_ok("option-a") && !variant_ok("A") && !variant_ok("../x"));
        }
    }
}
pub(crate) use preview::start as ui_preview_start;
