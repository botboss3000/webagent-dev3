//! Installation catalog lifecycle. Browser authentication and CSRF are inherited from api::router.
use crate::State;
use crate::{Api, App};
use axum::{
    extract::Path,
    routing::{get, post},
    Extension, Json, Router,
};
use serde_json::Value;
pub use webagent_models::api::platform_catalog::{Draft, Fence, KeyUpdate, Rollback};

async fn read(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::platform_catalog::read(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}
async fn local_models(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::platform_catalog::local_models(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}
async fn history(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::platform_catalog::history(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}

async fn draft(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Draft>,
) -> Api {
    webagent_models::api::platform_catalog::draft(&app.models_runtime(), identity, input)
        .await
        .map_err(Into::into)
}
async fn publish(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Fence>,
) -> Api {
    webagent_models::api::platform_catalog::publish(&app.models_runtime(), identity, input)
        .await
        .map_err(Into::into)
}
async fn rollback(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Rollback>,
) -> Api {
    webagent_models::api::platform_catalog::rollback(&app.models_runtime(), identity, input)
        .await
        .map_err(Into::into)
}

async fn credential(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(entry_id): Path<String>,
    Json(input): Json<KeyUpdate>,
) -> Api {
    webagent_models::api::platform_catalog::credential(
        &app.models_runtime(),
        identity,
        entry_id,
        input,
    )
    .await
    .map_err(Into::into)
}
async fn validate(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::platform_catalog::validate(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}
/// Preview compares metadata only; it neither publishes nor validates provider acceptance.
async fn preview(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::platform_catalog::preview(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}
pub fn router() -> Router<App> {
    Router::new()
        .route("/", get(read).put(draft))
        .route("/history", get(history))
        .route("/local-models", get(local_models))
        .route("/validate", post(validate))
        .route("/preview", post(preview))
        .route(
            "/entries/{entry_id}/credential",
            axum::routing::put(credential),
        )
        .route("/publish", post(publish))
        .route("/rollback", post(rollback))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform_catalog::{Entry, Mutation};
    use serde_json::json;
    use std::collections::BTreeMap;
    #[tokio::test]
    async fn preview_is_metadata_only_and_does_not_publish() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let admin = json!({"_user_id":"admin"});
        let entry = Entry {
            id: "model".into(),
            display_name: "First".into(),
            upstream_model: "deepseek-chat".into(),
            transport: crate::model_selection::Transport::Deepseek,
            capabilities: std::collections::BTreeSet::from(["text".into()]),
        };
        let entries = BTreeMap::from([("model".into(), entry.clone())]);
        let _ = mutate(&app, &admin, 0, Mutation::Draft { entries })
            .await
            .unwrap();
        let Json(initial) = preview(State(app.clone()), Extension(admin.clone()))
            .await
            .unwrap();
        assert_eq!(initial["added_entry_ids"], json!(["model"]));
        assert!(initial["published_revision"].is_null());
        let _ = mutate(&app, &admin, 1, Mutation::Publish { rollback: None })
            .await
            .unwrap();
        let mut changed = entry;
        changed.display_name = "Changed".into();
        let _ = mutate(
            &app,
            &admin,
            2,
            Mutation::Draft {
                entries: BTreeMap::from([("model".into(), changed)]),
            },
        )
        .await
        .unwrap();
        let Json(result) = preview(State(app.clone()), Extension(admin)).await.unwrap();
        assert_eq!(result["changed_entry_ids"], json!(["model"]));
        assert_eq!(result["published_revision"], 2);
        assert_eq!(
            app.store.platform_roster().await.unwrap().unwrap().revision,
            3
        );
        assert!(!result.to_string().contains("credential_ref"));
        assert!(
            preview(State(app), Extension(json!({"_user_id":"unknown"})))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn lifecycle_requires_live_admin_and_fences_history() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let admin = json!({"_user_id":"admin"});
        let Json(saved) = mutate(
            &app,
            &admin,
            0,
            Mutation::Draft {
                entries: BTreeMap::new(),
            },
        )
        .await
        .unwrap();
        assert_eq!(saved["roster"]["revision"], 1);
        assert!(mutate(
            &app,
            &admin,
            0,
            Mutation::Draft {
                entries: BTreeMap::new()
            }
        )
        .await
        .is_err());
        let _ = mutate(&app, &admin, 1, Mutation::Publish { rollback: None })
            .await
            .unwrap();
        assert!(
            read(State(app.clone()), Extension(json!({"_user_id":"unknown"})))
                .await
                .is_err()
        );
        let Json(loaded) = read(State(app), Extension(admin)).await.unwrap();
        assert_eq!(loaded["roster"]["published_revision"], 2);
        assert!(serde_json::from_value::<Draft>(
            json!({"expected_revision":2,"entries":{},"history":[]})
        )
        .is_err());
    }
}

#[cfg(test)]
async fn mutate(
    app: &App,
    identity: &Value,
    expected: u64,
    operation: crate::platform_catalog::Mutation,
) -> Api {
    webagent_models::api::platform_catalog::mutate(
        &app.models_runtime(),
        identity,
        expected,
        operation,
    )
    .await
    .map_err(Into::into)
}
