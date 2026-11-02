//! Account model preferences. Browser auth/CSRF are supplied by the owning router.
use crate::State;
use crate::{account_catalog::Preferences, Api, App};
use axum::{Extension, Json};
use serde_json::Value;
pub use webagent_models::api::account_catalog::DefaultUpdate;
pub async fn read(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    webagent_models::api::account_catalog::read(&app.models_runtime(), identity)
        .await
        .map_err(Into::into)
}
pub async fn save(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<Preferences>,
) -> Api {
    webagent_models::api::account_catalog::save(&app.models_runtime(), identity, input)
        .await
        .map_err(Into::into)
}

/// A narrow write avoids replaying aliases or selections from an unrelated editor.

pub async fn set_default(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(input): Json<DefaultUpdate>,
) -> Api {
    webagent_models::api::account_catalog::set_default(&app.models_runtime(), identity, input)
        .await
        .map_err(Into::into)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn default_write_is_fenced_and_rejects_forged_sources() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = serde_json::json!({"_user_id":"admin"});
        let Json(saved) = set_default(
            State(app.clone()),
            Extension(identity.clone()),
            Json(DefaultUpdate {
                expected_revision: 0,
                default_entry_id: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(saved["revision"], 1);
        assert!(set_default(
            State(app.clone()),
            Extension(identity.clone()),
            Json(DefaultUpdate {
                expected_revision: 0,
                default_entry_id: None
            })
        )
        .await
        .is_err());
        assert!(set_default(
            State(app.clone()),
            Extension(identity),
            Json(DefaultUpdate {
                expected_revision: 1,
                default_entry_id: Some("platform:base:forged".into())
            })
        )
        .await
        .is_err());
        assert_eq!(
            app.store
                .account_model_preferences("admin")
                .await
                .unwrap()
                .revision,
            1
        );
        assert!(serde_json::from_value::<DefaultUpdate>(serde_json::json!({
            "expected_revision":1,"default_entry_id":null,"credential_ref":"forged"}))
        .is_err());
    }
}

/// Owner-only: removes a saved model from My Models and from every agent using it.
pub async fn delete_entry(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    axum::extract::Path(entry): axum::extract::Path<String>,
) -> Api {
    webagent_models::api::account_catalog::delete_entry(&app.models_runtime(), identity, entry)
        .await
        .map_err(Into::into)
}

/// Source rehydration and persistence own the security boundary, not a client catalog replay.
pub async fn adopt(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    axum::extract::Path(agent): axum::extract::Path<String>,
    Json(input): Json<crate::account_catalog::Adoption>,
) -> Api {
    webagent_models::api::account_catalog::adopt(&app.models_runtime(), identity, agent, input)
        .await
        .map_err(Into::into)
}
