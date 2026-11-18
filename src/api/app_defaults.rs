//! App Default routes: administrators read editable ability files, keep drafts and Save
//! them through one broker task. The Administration module owns the checks and broker
//! calls; the router enforces administrator authentication and CSRF first.
use super::*;
use crate::tools::administrator::Host;
use webagent_administration::app_defaults::Draft;
use webagent_administration_host::app_defaults;

/// The App Default routes; the caller adds administrator authentication.
pub(super) fn admin_router() -> Router<App> {
    Router::new()
        .route("/admin/app-defaults/file", get(file))
        .route(
            "/admin/app-defaults/drafts",
            get(drafts).post(put).delete(discard),
        )
        .route("/admin/app-defaults/save", post(save))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileQuery {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutBody {
    path: String,
    /// The new draft, or null to revert this file.
    draft: Option<Draft>,
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RevisionBody {
    expected_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SaveBody {
    #[serde(default)]
    note: String,
    expected_revision: u64,
}

pub(crate) async fn file(Query(query): Query<FileQuery>) -> Api {
    Ok(Json(app_defaults::read(&query.path).await?))
}

pub(crate) async fn drafts(State(app): State<App>) -> Api {
    Ok(Json(app_defaults::drafts(&Host(&app)).await?))
}

pub(crate) async fn put(State(app): State<App>, Json(body): Json<PutBody>) -> Api {
    Ok(Json(
        app_defaults::put(&Host(&app), &body.path, body.draft, body.expected_revision).await?,
    ))
}

pub(crate) async fn discard(State(app): State<App>, Json(body): Json<RevisionBody>) -> Api {
    Ok(Json(
        app_defaults::discard(&Host(&app), body.expected_revision).await?,
    ))
}

pub(crate) async fn save(State(app): State<App>, Json(body): Json<SaveBody>) -> Api {
    Ok(Json(
        app_defaults::save(&Host(&app), &body.note, body.expected_revision).await?,
    ))
}
