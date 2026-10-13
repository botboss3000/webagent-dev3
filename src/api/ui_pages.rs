use crate::State;
use axum::{extract::{Request, Path as RoutePath}, http::StatusCode, middleware::Next,
    response::Response, Json, Router};
use tower::ServiceExt;
use tower_http::services::{ServeDir, ServeFile};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs, path::Path};

#[derive(Deserialize)]
struct Descriptor {
    id: String,
    label: String,
    #[serde(default)]
    order: i64,
    html: Option<String>,
    css: Option<Vec<String>>,
    entry: Option<String>,
}

fn asset(folder: &Path, base: &str, name: &str) -> Result<String, String> {
    if name.is_empty() || name == "." || name == ".."
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || !folder.join(name).is_file() || folder.join(name).is_symlink()
    {
        return Err(format!("Invalid page asset: {base}/{name}"));
    }
    Ok(format!("{base}/{name}"))
}

fn discover(ui: &Path) -> Result<Value, String> {
    let root = ui.join("main-panel");
    let mut pages = Vec::new();
    for item in fs::read_dir(&root).map_err(|e| format!("Cannot read main page directory: {e}"))? {
        let folder = item.map_err(|e| e.to_string())?.path();
        if !folder.is_dir() || folder.is_symlink() || folder.file_name().unwrap().to_string_lossy().starts_with('_') {
            continue;
        }
        let descriptor = folder.join("page.json");
        if !descriptor.is_file() { continue; }
        let page: Descriptor = serde_json::from_str(
            &fs::read_to_string(&descriptor).map_err(|e| e.to_string())?
        ).map_err(|e| format!("Invalid {}: {e}", descriptor.display()))?;
        let folder_name = folder.file_name().unwrap().to_string_lossy();
        if page.id != folder_name || page.label.trim().is_empty()
            || !page.id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')) {
            return Err(format!("Invalid page identity: {}", descriptor.display()));
        }
        let base = format!("/v3-ui/main-panel/{}", page.id);
        let html = page.html.as_deref().map(|name| asset(&folder, &base, name)).transpose()?;
        let css = page.css.unwrap_or_default().iter()
            .map(|name| asset(&folder, &base, name)).collect::<Result<Vec<_>, _>>()?;
        let entry = page.entry.as_deref().map(|name| asset(&folder, &base, name)).transpose()?;
        pages.push(json!({"id":page.id,"label":page.label,"order":page.order,
            "html":html,"css":css,"entry":entry}));
    }
    pages.sort_by(|a,b| a["order"].as_i64().cmp(&b["order"].as_i64())
        .then_with(|| a["label"].as_str().cmp(&b["label"].as_str())));
    Ok(json!({"main":pages}))
}

fn live_root(state: &Path, ui: &Path) -> Option<std::path::PathBuf> {
    (crate::store::read_app_settings(state)
        .is_ok_and(|settings| settings["development"]["live_ui_editing"].as_bool() == Some(true)) && ui.is_dir() && !ui.is_symlink())
        .then(|| ui.to_path_buf())
}

fn live_routes(ui: &Path) -> Router {
    Router::new()
        .route_service("/app", ServeFile::new(ui.join("index.html")))
        .route_service("/", ServeFile::new(ui.join("index.html")))
        .route_service("/setup.html", ServeFile::new(ui.join("setup.html")))
        .nest_service("/v3-ui", ServeDir::new(ui))
}

/// Only the fixed development checkout UI is eligible. API admission, backend
/// code and shared reference assets continue through their existing routes.
pub(super) async fn live_ui(
    State(app): State<crate::App>, request: Request, next: Next,
) -> Response {
    let path = request.uri().path();
    let ui_request = matches!(path, "/" | "/app" | "/setup.html")
        || path == "/v3-ui" || path.starts_with("/v3-ui/");
    if ui_request {
        if let Some(ui) = live_root(&app.store.root, Path::new("/live-checkout/ui")) {
            return live_routes(&ui).oneshot(request).await.expect("infallible UI router");
        }
    }
    next.run(request).await
}

pub async fn catalog(State(app): State<crate::App>) -> Result<Json<Value>, (StatusCode, String)> {
    let ui = live_root(&app.store.root, Path::new("/live-checkout/ui"))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| frozen_root());
    let mut catalog=discover(Path::new(&ui)).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR,e))?;
    // Package-owned assets disappear with their package; the shell has no module engine imports.
    for id in crate::modules::package_ids() {
        if !crate::modules::available(&id) {continue;}
        let Some(folder)=crate::modules::package_root(&id) else {continue;};
        let descriptor=folder.join("page.json");
        let Ok(meta)=fs::symlink_metadata(&descriptor) else {continue;};
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>4096 {continue;}
        let Ok(bytes)=fs::read(descriptor) else {continue;};
        let Ok(page)=serde_json::from_slice::<Descriptor>(&bytes) else {continue;};
        if page.id!=id || page.label.trim().is_empty() {continue;}
        let base=format!("/api/v3/modules/{id}/assets");
        let html=page.html.as_deref().map(|name|asset(&folder,&base,name)).transpose();
        let css=page.css.unwrap_or_default().iter().map(|name|asset(&folder,&base,name)).collect::<Result<Vec<_>,_>>();
        let entry=page.entry.as_deref().map(|name|asset(&folder,&base,name)).transpose();
        if let (Ok(html),Ok(css),Ok(entry))=(html,css,entry) {
            catalog["main"].as_array_mut().unwrap().push(json!({"id":page.id,"label":page.label,"order":page.order,"html":html,"css":css,"entry":entry}));
        }
    }
    catalog["main"].as_array_mut().unwrap().sort_by(|a,b|a["order"].as_i64().cmp(&b["order"].as_i64()).then_with(||a["label"].as_str().cmp(&b["label"].as_str())));
    Ok(Json(catalog))
}

/// Only validated package UI assets are public to the authenticated shell.
pub(crate) async fn module_asset(State(_app):State<crate::App>,RoutePath((id,name)):RoutePath<(String,String)>)->Response {
    use axum::response::IntoResponse;
    let mime=match name.as_str() {"page.js"=>"text/javascript; charset=utf-8","page.css"=>"text/css; charset=utf-8","page.html"=>"text/html; charset=utf-8",_=>return StatusCode::NOT_FOUND.into_response()};
    if !crate::modules::available(&id) {return StatusCode::NOT_FOUND.into_response();}
    let Some(root)=crate::modules::package_root(&id) else {return StatusCode::NOT_FOUND.into_response();};
    let path=root.join(name);
    let Ok(meta)=fs::symlink_metadata(&path) else {return StatusCode::NOT_FOUND.into_response();};
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len()>256*1024 {return StatusCode::NOT_FOUND.into_response();}
    match tokio::fs::read(path).await {
        Ok(bytes)=>([(axum::http::header::CONTENT_TYPE,mime),(axum::http::header::CACHE_CONTROL,"no-store")],bytes).into_response(),
        Err(_)=>StatusCode::NOT_FOUND.into_response(),
    }
}

fn frozen_root() -> String {
    std::env::var("V3_UI_ROOT").unwrap_or_else(|_| "/opt/v3/ui".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn live_ui_switch_and_saved_edits_are_visible() {
        let state = tempfile::tempdir().unwrap();
        let ui = tempfile::tempdir().unwrap();
        fs::create_dir(state.path().join("config")).unwrap();
        let config_path = state.path().join("config/app-settings.json");
        let mut config: Value = serde_json::from_str(include_str!("../../config/app-settings.json")).unwrap();
        fs::write(&config_path, config.to_string()).unwrap();
        assert!(live_root(state.path(), ui.path()).is_none());
        config["development"]["live_ui_editing"] = json!(true);
        fs::write(&config_path, config.to_string()).unwrap();
        assert_eq!(live_root(state.path(), ui.path()).unwrap(), ui.path());
        // Storage policy is preserved; switching does not require a restart.
        assert_eq!(config["admin_database_encryption"]["enabled"], false);
        for text in ["first edit", "saved edit"] {
            fs::write(ui.path().join("index.html"), text).unwrap();
            let response = live_routes(ui.path()).oneshot(
                Request::builder().uri("/app").body(axum::body::Body::empty()).unwrap()
            ).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(body.as_ref(), text.as_bytes());
        }
        config["development"]["live_ui_editing"] = json!(false);
        fs::write(&config_path, config.to_string()).unwrap();
        assert!(live_root(state.path(), ui.path()).is_none());
        // Invalid or missing settings and absent checkout mounts fail closed.
        fs::write(&config_path, "{}").unwrap();
        assert!(live_root(state.path(), ui.path()).is_none());
        fs::write(&config_path, "{broken").unwrap();
        assert!(live_root(state.path(), ui.path()).is_none());
        fs::remove_file(config_path).unwrap();
        assert!(live_root(state.path(), ui.path()).is_none());
    }

    #[test]
    fn discovers_drop_in_pages_in_order() {
        let root = tempfile::tempdir().unwrap();
        for (name, order) in [("later", 8), ("first", 1)] {
            let folder = root.path().join("main-panel").join(name);
            fs::create_dir_all(&folder).unwrap();
            fs::write(folder.join("page.js"), "export function start() {}").unwrap();
            fs::write(folder.join("page.json"),
                format!(r#"{{"id":"{name}","label":"{name}","order":{order},"entry":"page.js"}}"#)
            ).unwrap();
        }
        let pages = discover(root.path()).unwrap();
        assert_eq!(pages["main"][0]["id"], "first");
        assert_eq!(pages["main"][1]["entry"], "/v3-ui/main-panel/later/page.js");
    }
    #[test]
    fn rejects_asset_escape() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("main-panel/agents");
        fs::create_dir_all(&folder).unwrap();
        fs::write(folder.join("page.json"),
            r#"{"id":"agents","label":"Agents","entry":"../app.js"}"#).unwrap();
        assert!(discover(root.path()).is_err());
    }
}
