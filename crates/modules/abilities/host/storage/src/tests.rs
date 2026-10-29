// The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
#![allow(clippy::unwrap_used)]

use super::*;
use serde_json::json;
use webagent_host_core::test_support::store_core;
use webagent_host_core::{digest, now, uuid};

#[tokio::test]
async fn core_vault_submission_is_nonce_revision_and_user_bound() {
    let root = tempfile::tempdir().unwrap();
    let store = store_core(root.path(), "admin").await;
    let request = digest(&json!(uuid()));
    let value = uuid();
    store
        .put(
            "session:source",
            &json!({"id":"source","status":"active","agent_id":"shared_default"}),
        )
        .await
        .unwrap();
    store.put("module_state:core_ui:source",&json!({"revision":1,"state":{"request_id":request,"destination":"credential","credential_name":"example","credential_origin":"https://example.test","actor_id":"admin","expected_revision":0}})).await.unwrap();
    assert!(store
        .core_save_credential("source", "other", &request, 0, &value)
        .await
        .is_err());
    let output = store
        .core_save_credential("source", "example", &request, 0, &value)
        .await
        .unwrap();
    assert_eq!(output["revision"], 1);
    assert!(output.get("value").is_none());
    // The spent form is cleared so its card disappears everywhere; the replayed receipt still answers retries.
    let ui = store.get("module_state:core_ui:source").await.unwrap().unwrap();
    assert_eq!(ui["state"], json!({}));
    assert_eq!(ui["revision"], 2);
    assert_eq!(
        store
            .core_save_credential("source", "example", &request, 0, &value)
            .await
            .unwrap(),
        output
    );
    assert!(store
        .core_save_credential("source", "example", &request, 0, "different")
        .await
        .is_err());
    let raw: Vec<u8> =
        sqlx::query_scalar("SELECT value FROM docs WHERE key='core_vault:example'")
            .fetch_one(&store.pool)
            .await
            .unwrap();
    assert!(!raw
        .windows(value.len())
        .any(|part| part == value.as_bytes()));
}

#[tokio::test]
async fn core_webhook_replay_revocation_and_bounded_retention() {
    let root = tempfile::tempdir().unwrap();
    let store = store_core(root.path(), "admin").await;
    let id = uuid();
    store
        .put(
            "session:source",
            &json!({"id":"source","status":"active","agent_id":"shared_default"}),
        )
        .await
        .unwrap();
    let spec = json!({"id":id,"revision":1,"session_id":"source","artifact":{"expires_at":now()+60,"token_digest":"fixture-fingerprint","max_entries":2}});
    store
        .put(&format!("core_webhook:{id}"), &spec)
        .await
        .unwrap();
    let body = json!({"message":"untrusted"});
    let first = store
        .core_webhook_ingest(&id, 1, "fixture-fingerprint", "one", &body)
        .await
        .unwrap();
    assert_eq!(first["seq"], 1);
    assert_eq!(
        store
            .core_webhook_ingest(&id, 1, "fixture-fingerprint", "one", &body)
            .await
            .unwrap()["duplicate"],
        true
    );
    assert!(store
        .core_webhook_ingest(
            &id,
            1,
            "fixture-fingerprint",
            "one",
            &json!({"changed":true})
        )
        .await
        .is_err());
    for event in ["two", "three"] {
        store
            .core_webhook_ingest(&id, 1, "fixture-fingerprint", event, &body)
            .await
            .unwrap();
    }
    let log = store
        .get(&format!("core_webhook_log:{id}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(log["entries"].as_array().unwrap().len(), 2);
    assert_eq!(log["entries"][0]["seq"], 2);
    let mut revoked = spec.clone();
    revoked["revision"] = json!(2);
    revoked["artifact"]["deleted"] = json!(true);
    store
        .put(&format!("core_webhook:{id}"), &revoked)
        .await
        .unwrap();
    assert!(store
        .core_webhook_ingest(&id, 1, "fixture-fingerprint", "four", &body)
        .await
        .is_err());
}
