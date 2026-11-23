use axum::{
    extract::{Path as RoutePath, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::Value;
use std::path::Path;
use tower::ServiceExt;
use webagent_accounts_host::AccountsStore;

/// Whether this browser request carries a signed-in administrator's own sign-in. Bearer callers
/// (chat widgets) and sessions the diagnostics (agent) browser signed in with never count.
#[cfg(feature = "administrator")]
pub(crate) async fn administrator_browser(app: &crate::App, headers: &HeaderMap) -> bool {
    if headers.contains_key(header::AUTHORIZATION) {
        return false;
    }
    let Ok(Some(candidate)) = app
        .store
        .accounts_repository()
        .inspect_session(
            webagent_accounts::session_token(headers),
            webagent_host_core::now(),
        )
        .await
    else {
        return false;
    };
    crate::access::Principal::from_account(candidate.account_state())
        .is_ok_and(|principal| principal.active() && principal.administrator())
        && candidate.finish().is_some_and(|session| {
            session.document["user_agent"] != webagent_accounts::DIAGNOSTICS_BROWSER_AGENT
                && !crate::access::identity::CredentialKind::of_sign_in(&session.document)
                    .restricted()
        })
}
/// Builds without the administrator feature serve no administrator pages to anyone.
#[cfg(not(feature = "administrator"))]
pub(crate) async fn administrator_browser(_app: &crate::App, _headers: &HeaderMap) -> bool {
    false
}

pub(super) async fn live_ui(
    State(app): State<crate::App>,
    request: Request,
    next: Next,
) -> Response {
    // Files in `ui/main-panel/Administrator/` get the missing-file 404 unless the caller is a
    // signed-in administrator; checked first, so draft previews and the live root are gated too.
    if webagent_webapp::pages::administrator_file(request.uri().path())
        && !administrator_browser(&app, request.headers()).await
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let path = request.uri().path();
    if (path == "/app" || path.starts_with("/ui/"))
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
    if matches!(path, "/" | "/app" | "/setup.html") || path == "/ui" || path.starts_with("/ui/") {
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
pub async fn catalog(
    State(app): State<crate::App>,
    headers: HeaderMap,
) -> Result<Json<Value>, (StatusCode, String)> {
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
    catalog_at(&app, &headers, &ui).await
}
/// The main page catalog under `ui`; administrator pages only for a signed-in administrator.
async fn catalog_at(
    app: &crate::App,
    headers: &HeaderMap,
    ui: &Path,
) -> Result<Json<Value>, (StatusCode, String)> {
    let administrator = administrator_browser(app, headers).await;
    let packages = crate::modules::package_ids()
        .into_iter()
        .filter(|id| crate::modules::available(id))
        .filter_map(|id| crate::modules::package_root(&id).map(|root| (id.to_owned(), root)));
    webagent_webapp::pages::catalog(ui, administrator, packages)
        .map(Json)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))
}
pub(crate) async fn module_asset(
    axum::Extension(app): axum::Extension<crate::App>,
    RoutePath((id, name)): RoutePath<(String, String)>,
) -> Response {
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
    use axum::{http::HeaderValue, response::Redirect};
    use serde_json::json;
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{LazyLock, Mutex},
    };

    pub(super) const COOKIE: &str = "v3_ui_preview";
    /// The browser's longest cookie life (400 days): a preview lasts until the off link.
    const COOKIE_MAX_AGE: u64 = 400 * 24 * 3600;

    #[derive(Clone)]
    struct Grant {
        user_id: String,
        session_id: String,
        variant: String,
    }

    /// Grants by random cookie token, one per user; `off` or a restart ends a preview.
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
            path => path.strip_prefix("/ui/")?,
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
        let grant = GRANTS.lock().ok()?.get(token)?.clone();
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
        headers: HeaderMap,
    ) -> Response {
        if let Err(error) = crate::phase3_api::administrator(&app, &identity).await {
            return error.into_response();
        }
        let clear = format!("{COOKIE}=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Lax");
        if variant == "off" {
            if let (Some(token), Ok(mut grants)) = (cookie_value(&headers, COOKIE), GRANTS.lock()) {
                grants.remove(token);
            }
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
        };
        if grant.user_id.is_empty() {
            return (StatusCode::UNAUTHORIZED, "Sign in required").into_response();
        }
        if let Ok(mut grants) = GRANTS.lock() {
            // A new preview replaces the user's previous one, so grants stay bounded.
            grants.retain(|_, old| old.user_id != grant.user_id);
            grants.insert(token.clone(), grant);
        }
        let cookie = format!(
            "{COOKIE}={token}; Path=/; Max-Age={COOKIE_MAX_AGE}; HttpOnly; Secure; SameSite=Lax"
        );
        ([(header::SET_COOKIE, cookie)], Redirect::to("/app")).into_response()
    }

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    pub(crate) struct Choice {
        /// The variant to preview, or null to stop previewing.
        variant: Option<String>,
        /// The session whose workspace holds the variant: this chat (the default) or one of
        /// its sub-agent sessions, which keep their variants in their own workspaces.
        #[serde(default)]
        source_session: Option<String>,
    }

    /// The conversation tree's root of session `sid`, from its spawn record (itself when it
    /// was not spawned). The caller's own store finds only the caller's records.
    async fn tree_root(app: &crate::App, sid: &str) -> Option<String> {
        let origin = app
            .store
            .get(&format!("core_spawn_origin:{sid}"))
            .await
            .ok()
            .flatten();
        Some(
            origin
                .as_ref()
                .and_then(|origin| origin["root_session_id"].as_str())
                .unwrap_or(sid)
                .to_owned(),
        )
    }

    /// `POST /api/v3/sessions/{sid}/ui-preview` with `{"variant": name | null}` and an
    /// optional `source_session` (a sub-agent of this chat): what the start and off links do,
    /// for a page that then reloads itself in the same tab (a UI element's preview switch)
    /// rather than following a redirect.
    pub(crate) async fn set(
        axum::Extension(app): axum::Extension<crate::App>,
        axum::Extension(identity): axum::Extension<Value>,
        RoutePath(sid): RoutePath<String>,
        headers: HeaderMap,
        axum::Json(choice): axum::Json<Choice>,
    ) -> Response {
        let variant = choice.variant.unwrap_or_else(|| "off".into());
        if variant != "off" && !variant_ok(&variant) {
            return (StatusCode::BAD_REQUEST, "Invalid variant").into_response();
        }
        // A card may preview a variant from a sub-agent of this chat, never another tree.
        let source = choice.source_session.unwrap_or_else(|| sid.clone());
        if source != sid
            && (!session_ok(&source)
                || tree_root(&app, &source).await != tree_root(&app, &sid).await)
        {
            return (StatusCode::FORBIDDEN, "Not a session of this conversation").into_response();
        }
        let sid = source;
        let mut response = start(
            axum::Extension(app),
            axum::Extension(identity),
            RoutePath((sid.clone(), variant.clone())),
            headers,
        )
        .await;
        if response.status().is_redirection() {
            *response.status_mut() = StatusCode::OK;
            response.headers_mut().remove(header::LOCATION);
            let active = (variant != "off").then_some(variant);
            *response.body_mut() =
                axum::body::Body::from(json!({"session_id":sid,"variant":active}).to_string());
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            );
        }
        response
    }

    /// `GET /api/v3/ui-preview`: the preview this browser shows, `{"session_id","variant"}`,
    /// or nulls when none (the preview cookie itself is hidden from pages).
    pub(crate) async fn status(
        axum::Extension(app): axum::Extension<crate::App>,
        headers: HeaderMap,
    ) -> Response {
        let grant = cookie_value(&headers, COOKIE)
            .and_then(|token| GRANTS.lock().ok()?.get(token).cloned());
        let live = match &grant {
            Some(_) => root_for(&app, &headers).await.is_some(),
            None => false,
        };
        let body = match grant.filter(|_| live) {
            Some(grant) => json!({"session_id":grant.session_id,"variant":grant.variant}),
            None => json!({"session_id":null,"variant":null}),
        };
        axum::Json(body).into_response()
    }

    /// Serve the variant's copy of a UI file, never cached.
    pub(super) async fn serve(file: PathBuf, mut request: Request) -> Response {
        // The browser revalidates its cached live copy; answering that from the variant's
        // date could return 304 and keep the live file, so a preview is always sent whole.
        for name in [
            header::IF_MODIFIED_SINCE,
            header::IF_NONE_MATCH,
            header::IF_UNMODIFIED_SINCE,
            header::IF_MATCH,
            header::IF_RANGE,
        ] {
            request.headers_mut().remove(name);
        }
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

        #[tokio::test]
        async fn a_preview_file_is_sent_whole_despite_a_newer_cached_copy() {
            let dir = tempfile::tempdir().expect("dir");
            let file = dir.path().join("a.css");
            std::fs::write(&file, "/* variant */").expect("file");
            // The browser's cached live copy is newer than the variant file.
            let request = Request::builder()
                .uri("/ui/a.css")
                .header(header::IF_MODIFIED_SINCE, "Fri, 01 Jan 2100 00:00:00 GMT")
                .body(axum::body::Body::empty())
                .expect("request");
            let response = serve(file, request).await;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }

        #[test]
        fn variant_files_stay_inside_their_folder() {
            let dir = tempfile::tempdir().expect("dir");
            std::fs::create_dir_all(dir.path().join("chat")).expect("folder");
            std::fs::write(dir.path().join("index.html"), "x").expect("file");
            std::fs::write(dir.path().join("chat/a.js"), "x").expect("file");
            assert!(variant_file(dir.path(), "/app").is_some());
            assert!(variant_file(dir.path(), "/ui/chat/a.js").is_some());
            assert!(variant_file(dir.path(), "/v3-ui/chat/a.js").is_none());
            assert!(variant_file(dir.path(), "/ui/chat/missing.js").is_none());
            assert!(variant_file(dir.path(), "/ui/../index.html").is_none());
            assert!(variant_file(dir.path(), "/ui/shared/x.css").is_none());
            std::os::unix::fs::symlink("/etc/hostname", dir.path().join("link")).expect("link");
            assert!(variant_file(dir.path(), "/ui/link").is_none());
            assert!(variant_ok("option-a") && !variant_ok("A") && !variant_ok("../x"));
        }
    }
}
pub(crate) use preview::set as ui_preview_set;
pub(crate) use preview::start as ui_preview_start;
pub(crate) use preview::status as ui_preview_status;

#[cfg(test)]
mod administrator_pages_tests {
    use super::*;
    use axum::body::Body;
    use serde_json::json;

    /// Stores a browser session for `user` and returns its cookie header.
    async fn session(app: &crate::App, user: &str, user_agent: &str) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        let generation =
            app.store.account_by_id(user).await.unwrap().unwrap()["generation"].clone();
        app.store
            .put(
                &format!("auth:{}", webagent_configuration::digest(&json!(token))),
                &json!({"user_id":user,"csrf":"fixture","expires":webagent_host_core::now()+3600,
                    "generation":generation,"user_agent":user_agent}),
            )
            .await
            .unwrap();
        format!("v3_session={token}")
    }

    /// One UI tree with a public page and an administrator page (script and stylesheet).
    fn ui_tree() -> tempfile::TempDir {
        let ui = tempfile::tempdir().unwrap();
        for (folder, id) in [
            ("main-panel/wiki", "wiki"),
            ("main-panel/Administrator/builds", "builds"),
        ] {
            let folder = ui.path().join(folder);
            std::fs::create_dir_all(&folder).unwrap();
            std::fs::write(folder.join("page.js"), "export function start() {}").unwrap();
            std::fs::write(folder.join("page.css"), "main{}").unwrap();
            std::fs::write(
                folder.join("page.json"),
                format!(r#"{{"id":"{id}","label":"{id}","css":["page.css"],"entry":"page.js"}}"#),
            )
            .unwrap();
        }
        ui
    }

    /// The status, headers and body a UI file request gets through the administrator gate.
    async fn fetch(
        app: &crate::App,
        ui: &Path,
        path: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, String, Vec<u8>) {
        let mut request = Request::builder().uri(path);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = webagent_webapp::pages::routes::<crate::App>(ui)
            .layer(axum::middleware::from_fn_with_state(app.clone(), live_ui))
            .with_state(app.clone())
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let kind = format!("{:?}", response.headers().get(header::CONTENT_TYPE));
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap()
            .to_vec();
        (status, kind, body)
    }

    async fn catalog_ids(app: &crate::App, ui: &Path, headers: &[(&str, &str)]) -> Vec<String> {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        let Json(catalog) = catalog_at(app, &map, ui).await.unwrap();
        catalog["main"]
            .as_array()
            .unwrap()
            .iter()
            .map(|page| page["id"].as_str().unwrap().to_owned())
            .collect()
    }

    /// Signed out, an ordinary member, a widget bearer and the diagnostics browser get the
    /// missing-file 404 for administrator files and a catalog without administrator pages.
    #[tokio::test]
    async fn administrator_pages_are_refused_to_everyone_else() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let ui = ui_tree();
        let member = app
            .store
            .create_user_account("pages_member", "fixture-only-password-hash")
            .await
            .unwrap();
        let member = member["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(&member, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let member_cookie = session(&app, &member, "Fixture browser").await;
        let admin_cookie = session(&app, "admin", "Fixture browser").await;
        let diagnostics_cookie =
            session(&app, "admin", webagent_accounts::DIAGNOSTICS_BROWSER_AGENT).await;
        let missing = fetch(&app, ui.path(), "/ui/main-panel/wiki/missing.js", &[]).await;
        assert_eq!(missing.0, StatusCode::NOT_FOUND);
        let callers: [&[(&str, &str)]; 5] = [
            &[],
            &[("cookie", &member_cookie)],
            &[("authorization", "Bearer widget-fixture-token")],
            &[
                ("cookie", &admin_cookie),
                ("authorization", "Bearer widget-fixture-token"),
            ],
            &[("cookie", &diagnostics_cookie)],
        ];
        for caller in callers {
            for path in [
                "/ui/main-panel/Administrator/builds/page.js",
                "/ui/main-panel/Administrator/builds/page.css",
                "/ui/main-panel/%41dministrator/builds/page.js",
            ] {
                assert_eq!(
                    fetch(&app, ui.path(), path, caller).await,
                    missing,
                    "{path} {caller:?}"
                );
            }
            assert_eq!(
                fetch(&app, ui.path(), "/ui/main-panel/wiki/page.js", caller)
                    .await
                    .0,
                StatusCode::OK
            );
            let ids = catalog_ids(&app, ui.path(), caller).await;
            assert!(
                ids.contains(&"wiki".into()) && !ids.contains(&"builds".into()),
                "{caller:?}"
            );
        }
    }

    /// A signed-in administrator gets the administrator script, stylesheet and catalog entry.
    #[cfg(feature = "administrator")]
    #[tokio::test]
    async fn administrator_pages_are_served_to_a_signed_in_administrator() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let ui = ui_tree();
        let cookie = session(&app, "admin", "Fixture browser").await;
        let caller = [("cookie", cookie.as_str())];
        for (path, body) in [
            (
                "/ui/main-panel/Administrator/builds/page.js",
                "export function start() {}",
            ),
            ("/ui/main-panel/Administrator/builds/page.css", "main{}"),
        ] {
            let (status, _, bytes) = fetch(&app, ui.path(), path, &caller).await;
            assert_eq!(
                (status, bytes.as_slice()),
                (StatusCode::OK, body.as_bytes()),
                "{path}"
            );
        }
        let ids = catalog_ids(&app, ui.path(), &caller).await;
        assert!(ids.contains(&"wiki".into()) && ids.contains(&"builds".into()));
    }
}
