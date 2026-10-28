// The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
#![allow(clippy::unwrap_used)]

use super::*;
use webagent_host_core::test_support::store_core;

#[tokio::test]
async fn accounts_round_trip_through_the_core_handle() {
    let root = tempfile::tempdir().unwrap();
    let store = store_core(root.path(), "admin").await;
    store.accounts_repository().initialize().await.unwrap();
    assert!(!store.has_admin_account().await.unwrap());
    let created = store
        .create_user_account("first-user", "fixture-password-hash-placeholder")
        .await
        .unwrap();
    let id = created["user_id"].as_str().unwrap();
    let by_id = store.account_by_id(id).await.unwrap().unwrap();
    let by_name = store
        .account_by_username("first-user")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_id, by_name);
    assert!(store
        .all_user_ids()
        .await
        .unwrap()
        .iter()
        .any(|user| user == id));
    let guest = store.create_guest_account().await.unwrap();
    assert_ne!(guest["user_id"], created["user_id"]);
    // Records are sealed with the handle's codec, never stored as plain JSON.
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
        .bind(id)
        .fetch_one(&store.app_pool)
        .await
        .unwrap();
    assert!(serde_json::from_slice::<Value>(&raw).is_err());
    assert!(store.unseal(&raw).is_ok());
}
