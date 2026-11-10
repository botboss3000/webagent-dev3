//! Root composition of Diagnostics owner resources and authenticated routes.
use crate::{context_assembly::Prepared, context_input::Request, Store};
use axum::{extract::Query, routing::get, Extension, Json, Router};
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock};
use webagent_accounts_host::AccountsStore;
use webagent_diagnostics::{self as package, Recorder};
static RECORDER: OnceLock<Recorder<Store>> = OnceLock::new();
mod background;
#[cfg(test)]
mod tests;
mod ui_trace;
impl package::SnapshotStore for Store {
    fn current_user_id(&self) -> &str {
        crate::store::StoreCore::current_user_id(self)
    }
    fn seal(&self, value: &Value) -> anyhow::Result<Vec<u8>> {
        crate::store::StoreCore::seal(self, value)
    }
    fn unseal(&self, value: &[u8]) -> anyhow::Result<Value> {
        crate::store::StoreCore::unseal(self, value)
    }
    async fn session_exists(&self, sid: &str) -> Result<bool, package::Error> {
        self.get(&format!("session:{sid}"))
            .await
            .map(|v| v.is_some())
            .map_err(|error| {
                let error = crate::ApiError::from(error);
                package::Error(error.0, error.1)
            })
    }
}
fn api_error(error: package::Error) -> crate::ApiError {
    crate::ApiError(error.0, error.1)
}
pub(crate) async fn init(root: &Path, _store: &Store, role: &str) {
    package::init(&RECORDER, root, role).await
}
pub(crate) fn lifecycle(message: &str, fields: Value) {
    package::lifecycle(RECORDER.get(), message, fields)
}
pub(crate) fn provider_enabled(store: &Store) -> bool {
    package::provider_enabled(RECORDER.get(), store)
}
pub(crate) fn failure(store: &Store, run: &Value, error: &anyhow::Error, action: &str) {
    package::failure(RECORDER.get(), store, run, error, action)
}
pub(crate) fn committed(store: &Store, run: &Value, kind: &str, recovery: bool) {
    package::committed(
        RECORDER.get(),
        store,
        run,
        kind,
        recovery,
        crate::tools::failure::project,
    )
}
pub(crate) fn backend_error() {}
pub(crate) async fn http_errors(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    next.run(request).await
}
pub(crate) fn assembly(
    store: &Store,
    run: &Value,
    request: &Request<'_>,
    prepared: &anyhow::Result<Prepared<'_>>,
) -> Option<Value> {
    package::assembly(RECORDER.get(), store, run, request, prepared)
}
pub(crate) fn summary(
    store: &Store,
    run: &Value,
    request: &Request<'_>,
    result: &anyhow::Result<()>,
) -> Option<Value> {
    package::summary(RECORDER.get(), store, run, request, result)
}
pub(crate) fn validation(
    store: &Store,
    run: &Value,
    code: &'static str,
    stage: &'static str,
) -> Option<Value> {
    package::validation(RECORDER.get(), store, run, code, stage)
}
pub(crate) async fn context(
    store: &Store,
    sid: &str,
    rid: &str,
    attempt: &str,
) -> Result<Value, crate::ApiError> {
    package::context(RECORDER.get(), store, sid, rid, attempt)
        .await
        .map_err(api_error)
}
async fn authorize(app: &crate::App, identity: &Value) -> Result<(), crate::ApiError> {
    if !scope(&app.store) {
        return Err(crate::ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "Fixed admin account required".into(),
        ));
    }
    crate::phase3_api::administrator(app, identity).await
}
async fn read(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
    Query(filter): Query<package::Filter>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    Ok(Json(
        package::read(RECORDER.get(), filter)
            .await
            .map_err(api_error)?,
    ))
}
async fn status(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    Ok(Json(
        package::status(RECORDER.get()).await.map_err(api_error)?,
    ))
}
/// Most stored review records read for one issues view.
const MAX_ISSUE_VIEWS: usize = 2000;
#[derive(serde::Deserialize)]
struct IssueFilter {
    session_id: Option<String>,
    user_id: Option<String>,
    /// Unix seconds; runs updated earlier are left out.
    since: Option<i64>,
}
/// Administrator view of grouped run-health issues from the shared review log
/// (`diagnostics/agent-review.db`): one session tree, one account, or every account.
/// Counts, kinds and templates only; no user database is opened.
async fn issues(
    Extension(app): Extension<crate::App>,
    Extension(identity): Extension<Value>,
    Query(filter): Query<IssueFilter>,
) -> crate::Api {
    authorize(&app, &identity).await?;
    let since = filter.since.unwrap_or(0);
    let views = app
        .store
        .review_issue_views(
            filter.session_id.as_deref(),
            filter.user_id.as_deref(),
            since,
            MAX_ISSUE_VIEWS as i64,
        )
        .await
        .map_err(|error| {
            crate::ApiError(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                error.to_string(),
            )
        })?;
    let accounts = views
        .iter()
        .filter_map(|view| view["user_id"].as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    let mut view = webagent_engine::runtime::health::aggregate(&views);
    view["totals"]["accounts"] = json!(accounts);
    Ok(Json(view))
}
async fn no_store(mut response: axum::response::Response) -> axum::response::Response {
    response.headers_mut().insert(
        "cache-control",
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}
pub(crate) fn router() -> Router<crate::App> {
    Router::new()
        .route("/admin/diagnostics", get(read))
        .route("/admin/diagnostics/status", get(status))
        .route("/admin/diagnostics/issues", get(issues))
        .layer(axum::middleware::map_response(no_store))
}
pub(crate) fn background_router() -> Router<crate::App> {
    background::router().merge(ui_trace::router())
}
fn scope(store: &Store) -> bool {
    package::scope(store)
}
fn settings() -> Option<package::Settings> {
    package::settings(RECORDER.get())
}
#[cfg(test)]
use package::bridge;
#[cfg(test)]
use package::config;
#[cfg(test)]
use package::test_support::{
    assembled_instructions, event_allowed, identifier, insert_snapshot, measure, open, permits,
    prune, seed, snapshot_allowed, Capture, Config, Settings,
};
#[cfg(test)]
use sqlx::SqlitePool;
#[cfg(test)]
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
#[cfg(test)]
use tokio::sync::{mpsc, Semaphore};
#[cfg(test)]
fn transition(s: &Settings, run: &Value, kind: &str, recovery: bool) -> Option<Value> {
    package::test_support::transition(s, run, kind, recovery, crate::tools::failure::project)
}
#[cfg(test)]
async fn read_snapshot(
    pool: &SqlitePool,
    store: &Store,
    sid: &str,
    rid: &str,
    attempt: &str,
) -> Result<Value, crate::ApiError> {
    package::test_support::read_snapshot(pool, store, sid, rid, attempt)
        .await
        .map_err(api_error)
}
