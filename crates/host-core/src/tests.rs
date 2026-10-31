// The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
#![allow(clippy::unwrap_used)]

use super::*;
use crate::test_support::store_core as core;
use axum::http::StatusCode;
use serde_json::json;

#[tokio::test]
async fn primitives_keep_the_handle_identity_and_codec() {
    let root = tempfile::tempdir().expect("fixture root");
    let store = core(root.path(), "user-1").await;
    assert_eq!(store.current_user_id(), "user-1");
    assert_eq!(store.current_agent_id(), "shared_default");
    assert_eq!(store.context_cache_key("s1"), "user-1:s1");
    let value = json!({"a": 1});
    assert_eq!(store.unseal(&store.seal(&value).unwrap()).unwrap(), value);
    // Non-administrator user records are always sealed.
    let encoded = store.encode_user_record(&value).unwrap();
    assert_eq!(store.unseal(&encoded).unwrap(), value);
    assert!(std::ptr::eq(store.pool_for("session:s1"), &store.pool));
    assert!(store.validate_agent_doc_key("agent:other:model").is_err());
    store.configure_cache(1, 1);
}

#[tokio::test]
async fn agent_records_need_the_bound_agent_key() {
    let root = tempfile::tempdir().expect("fixture root");
    let mut store = core(root.path(), "user-1").await;
    let value = json!({"secret": true});
    assert!(store.seal_agent(&value).is_err());
    store.initialize_agent_encryption().await.unwrap();
    let sealed = store.seal_agent(&value).unwrap();
    // unseal recognizes agent envelopes; the agent pool encodes with the agent key.
    assert_eq!(store.unseal(&sealed).unwrap(), value);
    let pooled = store.encode_for_pool(&store.agent_pool, &value).unwrap();
    assert_eq!(store.unseal_agent(&pooled).unwrap(), value);
    assert!(store.unseal_agent_for("other", &sealed).is_err());
    // A second view shares the registered key.
    let mut view = store.clone();
    view.agent_cipher = None;
    view.initialize_agent_encryption().await.unwrap();
    assert_eq!(view.unseal_agent(&sealed).unwrap(), value);
}

#[test]
fn api_errors_hide_internal_causes() {
    let archived = ApiError::from(anyhow::Error::new(webagent_chat::session::ArchivedSession));
    assert_eq!(archived.0, StatusCode::NOT_FOUND);
    let internal = ApiError::from(anyhow::anyhow!("private path"));
    assert_eq!(internal.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(!internal.1.contains("private"));
    assert_eq!(bad("x").0, StatusCode::BAD_REQUEST);
    assert_eq!(conflict("x").0, StatusCode::CONFLICT);
    assert_eq!(key("run", "1"), "run:1");
    assert_ne!(uuid(), uuid());
    assert!(now() > 0);
}

#[tokio::test]
async fn documents_round_trip_and_run_writes_need_the_run_index() {
    let root = tempfile::tempdir().expect("fixture root");
    let store = core(root.path(), "user-1").await;
    let value = json!({"enabled": true});
    store.put("preference:a", &value).await.unwrap();
    store
        .put("preference:b", &json!({"enabled": false}))
        .await
        .unwrap();
    assert_eq!(store.get("preference:a").await.unwrap(), Some(value));
    assert_eq!(store.list("preference:").await.unwrap().len(), 2);
    store.delete("preference:a").await.unwrap();
    assert_eq!(store.get("preference:a").await.unwrap(), None);
    assert_eq!(store.list("preference:").await.unwrap().len(), 1);
    // Stored rows are encoded with the handle's codec, never plain JSON.
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='preference:b'")
        .fetch_one(store.pool_for("preference:b"))
        .await
        .unwrap();
    assert!(serde_json::from_slice::<serde_json::Value>(&raw).is_err());
    let run = json!({"id": "r1", "session_id": "s1", "status": "completed"});
    assert!(store.put("run:r1", &run).await.is_err());
    assert_eq!(store.get("run:r1").await.unwrap(), None);
}

#[test]
fn storage_causes_are_logged_without_other_error_text() {
    let busy = std::io::Error::from_raw_os_error(11);
    let error = anyhow::Error::from(sqlx::Error::Io(busy)).context("Save run");
    assert_eq!(
        crate::error::storage_cause(&error),
        "error communicating with database: Resource temporarily unavailable (os error 11)"
    );
    let io = anyhow::Error::from(std::io::Error::from_raw_os_error(11));
    assert_eq!(
        crate::error::storage_cause(&io),
        "I/O WouldBlock (os error Some(11))"
    );
    let private = anyhow::anyhow!("Message text: my secret plan");
    assert_eq!(
        crate::error::storage_cause(&private),
        "not a database or I/O error"
    );
}
