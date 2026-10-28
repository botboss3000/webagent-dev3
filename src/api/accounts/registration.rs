//! Thin registration adapters; Access continues to own the shared policy writer and page decisions.
use crate::*;
pub(crate) use access::configuration::{page_allowed, validate_configuration, validate_pages};
pub(crate) async fn mode(app: &App) -> Result<String> {
    Ok(
        webagent_accounts::RegistrationMode::from_policy(&app.store.read_access_policy()?)?
            .access_mode_name()
            .into(),
    )
}
pub(crate) async fn access_mode(State(app): State<App>) -> Api {
    webagent_accounts::registration_api::access_mode(&app)
        .await
        .map_err(Into::into)
}
pub(crate) async fn set_mode(
    State(app): State<App>,
    axum::Extension(identity): axum::Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    webagent_accounts::registration_api::set_mode(&app, &identity, &body)
        .await
        .map_err(Into::into)
}
pub(crate) async fn register(
    State(app): State<App>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::registration_api::register(
        &app.store.accounts_repository(),
        &app,
        &app.login_attempts,
        &headers,
        &body,
        app.secure,
        webagent_accounts::AdmissionContext {
            now: now(),
            client: crate::account_authentication::throttle_client(&app, peer, &headers)?,
        },
    )
    .await
    .map_err(Into::into)
}
pub(crate) async fn guest_login(
    State(app): State<App>,
    peer: Option<axum::Extension<axum::extract::ConnectInfo<std::net::SocketAddr>>>,
    headers: HeaderMap,
) -> std::result::Result<Response, ApiError> {
    webagent_accounts::registration_api::guest_login(
        &app.store.accounts_repository(),
        &app,
        &app.login_attempts,
        &headers,
        app.secure,
        webagent_accounts::AdmissionContext {
            now: now(),
            client: crate::account_authentication::throttle_client(&app, peer, &headers)?,
        },
    )
    .await
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn policy_file_defaults_preserves_extensions_and_rejects_invalid_values() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        assert_eq!(
            app.store.read_access_policy().unwrap()["registration"],
            "restricted"
        );
        let path = app.store.access_policy_path();
        // Extension preservation is separate from the now-validated page-policy schema.
        std::fs::write(
            &path,
            br#"{"registration":"open","extension":{"future":"restricted"}}"#,
        )
        .unwrap();
        assert_eq!(mode(&app).await.unwrap(), "public_registered");
        app.store.set_registration_policy("restricted").unwrap();
        assert_eq!(
            app.store.read_access_policy().unwrap()["extension"]["future"],
            "restricted"
        );
        assert!(app.store.set_registration_policy("invalid").is_err());
        std::fs::write(&path, br#"{"registration":"invalid"}"#).unwrap();
        assert!(mode(&app).await.is_err());
        assert!(app.store.init_access_policy().await.is_err());
    }
    #[tokio::test]
    async fn policy_file_migrates_legacy_only_when_missing() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        app.store
            .put(
                "auth:registration_policy",
                &json!({"access_mode":"public_registered"}),
            )
            .await
            .unwrap();
        std::fs::remove_file(app.store.access_policy_path()).unwrap();
        app.store.init_access_policy().await.unwrap();
        assert_eq!(
            app.store.read_access_policy().unwrap()["registration"],
            "open"
        );
        app.store.set_registration_policy("restricted").unwrap();
        app.store.init_access_policy().await.unwrap();
        assert_eq!(mode(&app).await.unwrap(), "admin_approval");
    }
    #[tokio::test]
    async fn guest_cookie_rotates_and_policy_revokes_runtime_access() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        assert!(guest_login(State(app.clone()), None, HeaderMap::new())
            .await
            .is_err());
        app.store.set_registration_policy("open").unwrap();
        let response = guest_login(State(app.clone()), None, HeaderMap::new())
            .await
            .unwrap();
        let cookie = response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned();
        let token = cookie.strip_prefix("v3_session=").unwrap();
        let old_key = format!("auth:{}", digest(&json!(token)));
        let doc = app.store.get(&old_key).await.unwrap().unwrap();
        let id = doc["user_id"].as_str().unwrap();
        let account = app.store.account_by_id(id).await.unwrap().unwrap();
        assert_eq!(account["role"], "guest");
        assert!(account["password_hash"].is_null());
        assert!(!access::Principal::from_account(&account)
            .unwrap()
            .administrator());
        assert!(!app
            .store
            .live_access(id, "shared_default")
            .await
            .unwrap()
            .admitted());
        assert!(login(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"username":id,"password":"anything"}))
        )
        .await
        .is_err());
        let mut headers = HeaderMap::new();
        headers.insert("cookie", HeaderValue::from_str(&cookie).unwrap());
        let rotated = guest_login(State(app.clone()), None, headers)
            .await
            .unwrap();
        let next = rotated.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert_ne!(next, cookie);
        assert!(app.store.get(&old_key).await.unwrap().is_none());
        let next_doc = app
            .store
            .get(&format!(
                "auth:{}",
                digest(&json!(next.strip_prefix("v3_session=").unwrap()))
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(next_doc["user_id"], id);
        app.store.set_registration_policy("restricted").unwrap();
        assert!(app.store.live_access(id, "shared_default").await.is_err());
    }
    #[tokio::test]
    async fn registration_defaults_pending_and_never_grants_admin() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let response=register(State(app.clone()), None,HeaderMap::new(),Json(json!({"username":"new@example.test","password":"fixture-password","display_name":"New person"}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(!response.headers().contains_key("set-cookie"));
        let account = app
            .store
            .account_by_username("new@example.test")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account["status"], "pending");
        assert_eq!(account["role"], "user");
        assert_eq!(account["tier_id"], "basic");
        assert!(register(
            State(app),
            None,
            HeaderMap::new(),
            Json(json!({"username":"new@example.test","password":"fixture-password"}))
        )
        .await
        .is_err());
    }
    #[tokio::test]
    async fn public_signup_uses_regular_cookie_but_has_no_agent_grant() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        app.store.set_registration_policy("open").unwrap();
        let response = register(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"username":"second","password":"fixture-password"})),
        )
        .await
        .unwrap();
        assert!(response.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("HttpOnly"));
        let account = app
            .store
            .account_by_username("second")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(account["status"], "active");
        assert!(!app
            .store
            .live_access(account["user_id"].as_str().unwrap(), "shared_default")
            .await
            .unwrap()
            .admitted());
    }
}

#[cfg(test)]
mod advanced_policy_tests {
    use super::*;
    #[tokio::test]
    async fn policy_cas_and_file_tiers_preserve_extensions() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = app.store.read_access_policy().unwrap();
        let revision = before["revision"].as_u64().unwrap();
        app.store
            .set_registration_policy_revision("open", Some(revision))
            .unwrap();
        assert!(app
            .store
            .set_registration_policy_revision("restricted", Some(revision))
            .is_err());
        assert_eq!(
            app.store
                .entitlement_tier("basic")
                .await
                .unwrap()
                .unwrap()
                .id,
            "basic"
        );
        let mut policy = app.store.read_access_policy().unwrap();
        policy["extension"] = json!({"future":true});
        app.store.write_access_policy(&policy).unwrap();
        app.store
            .save_page_policy(
                json!({"main":{"chat":"admin"}}),
                policy["revision"].as_u64().unwrap(),
            )
            .unwrap();
        let current = app.store.read_access_policy().unwrap();
        assert_eq!(current["extension"]["future"], true);
        let user = app
            .store
            .create_user_account("fixture-user", "long-hash-placeholder")
            .await
            .unwrap();
        app.store
            .update_account_access(
                user["user_id"].as_str().unwrap(),
                1,
                access::AccountStatus::Active,
                access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        let account = app
            .store
            .account_by_id(user["user_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap();
        assert!(!page_allowed(
            &current,
            "main",
            "chat",
            &access::Principal::from_account(&account).unwrap()
        ));
        assert!(validate_pages(&json!({"main":{"chat":"invalid"}})).is_err());
    }
    #[tokio::test]
    async fn file_edits_change_live_access_fingerprint() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let before = app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap();
        let mut policy = app.store.read_access_policy().unwrap();
        policy["extension"] = json!("changed");
        app.store.write_access_policy(&policy).unwrap();
        let after = app
            .store
            .live_access("admin", "shared_default")
            .await
            .unwrap();
        assert_ne!(before.revision.policy, after.revision.policy);
    }
}
