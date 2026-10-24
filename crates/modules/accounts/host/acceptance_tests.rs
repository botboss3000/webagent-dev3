//! Real HTTP/host wiring for Accounts; domain primitives and response contracts live in native tests.
use crate::runtime_tests::{fixture, fixture_uninitialized};
use crate::*;
use axum::body::Body;
use tower::ServiceExt;
#[tokio::test]
async fn first_run_setup_is_single_claim_and_revokes_old_sessions() {
    let (_root, app) = fixture_uninitialized().await;
    let old_token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(old_token))),
            &json!({"user":"admin","csrf":"old","generation":null,"expires":now()+3600}),
        )
        .await
        .unwrap();
    app.store.delete("admin").await.unwrap();
    assert_eq!(
        setup_status(State(app.clone())).await.unwrap().0["initialized"],
        false
    );
    let response = setup_admin(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"new-secret"})),
    )
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("set-cookie").is_some());
    assert_eq!(
        setup_status(State(app.clone())).await.unwrap().0["initialized"],
        true
    );
    let repeated = setup_admin(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"other","password":"another-secret"})),
    )
    .await
    .unwrap_err();
    assert_eq!(repeated.0, StatusCode::CONFLICT);
    let old = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/status")
                .header("cookie", format!("v3_session={old_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    // The account table remains authoritative after the rollback-only document is gone.
    app.store.delete("admin").await.unwrap();
    assert_eq!(
        setup_status(State(app.clone())).await.unwrap().0["initialized"],
        true
    );
    let wrong = login(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"admin","password":"new-secret"})),
    )
    .await
    .unwrap_err();
    assert_eq!(wrong.0, StatusCode::UNAUTHORIZED);
    let valid = login(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"new-secret"})),
    )
    .await
    .unwrap();
    assert_eq!(valid.status(), StatusCode::OK);
    let profile = app.store.user_profile("admin").await.unwrap().unwrap();
    assert_eq!(profile["display_name"], "operator");
    assert!(profile["last_login_at"].as_u64().unwrap() > 0);
    let cookie = valid
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let me_response = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/auth/me")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(me_response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(me_response.into_body(), 4096)
        .await
        .unwrap();
    let me: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(me["user_id"], "admin");
    assert_eq!(me["display_name"], "operator");
    assert_eq!(me["is_admin"], true);
}

#[tokio::test]
async fn account_panel_updates_profile_and_rotates_password_generation() {
    let (_root, app) = fixture_uninitialized().await;
    app.store.delete("admin").await.unwrap();
    setup_admin(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"first-secret"})),
    )
    .await
    .unwrap();
    let signed_in = login(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"first-secret"})),
    )
    .await
    .unwrap();
    let cookie = signed_in
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let old_generation =
        app.store.account_by_id("admin").await.unwrap().unwrap()["generation"].clone();
    let doc = json!({"_user_id":"admin","user":"operator"});
    assert_eq!(
        update_profile(
            State(app.clone()),
            axum::Extension(doc.clone()),
            Json(json!({"username":"operator2","display_name":"Operator One"})),
        )
        .await
        .unwrap()
        .0["display_name"],
        "Operator One"
    );
    assert_eq!(
        change_password(
            State(app.clone()),
            axum::Extension(doc.clone()),
            Json(json!({"current_password":"wrong","new_password":"second-secret"})),
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::FORBIDDEN
    );
    let _ = change_password(
        State(app.clone()),
        axum::Extension(doc),
        Json(json!({"current_password":"first-secret","new_password":"second-secret"})),
    )
    .await
    .unwrap();
    let new_account = app.store.account_by_id("admin").await.unwrap().unwrap();
    assert_ne!(new_account["generation"], old_generation);
    assert_eq!(
        app.store.get("admin").await.unwrap().unwrap()["generation"],
        new_account["generation"]
    );
    let revoked = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/auth/me")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        login(
            State(app.clone()),
            None,
            HeaderMap::new(),
            Json(json!({"username":"operator","password":"first-secret"}))
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        login(
            State(app),
            None,
            HeaderMap::new(),
            Json(json!({"username":"operator2","password":"second-secret"}))
        )
        .await
        .unwrap()
        .status(),
        StatusCode::OK
    );
}
#[tokio::test]
async fn sign_in_duration_and_session_revocation_use_the_app_store() {
    let (_root, app) = fixture_uninitialized().await;
    app.store.delete("admin").await.unwrap();
    setup_admin(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"first-secret"})),
    )
    .await
    .unwrap();
    let doc = json!({"_user_id":"admin"});
    assert_eq!(
        update_session_policy(
            State(app.clone()),
            axum::Extension(doc.clone()),
            Json(json!({"days":2})),
        )
        .await
        .unwrap_err()
        .0,
        StatusCode::BAD_REQUEST
    );
    let _ = update_session_policy(
        State(app.clone()),
        axum::Extension(doc),
        Json(json!({"days":7})),
    )
    .await
    .unwrap();
    let response = login(
        State(app.clone()),
        None,
        HeaderMap::new(),
        Json(json!({"username":"operator","password":"first-secret"})),
    )
    .await
    .unwrap();
    let cookie_header = response
        .headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(cookie_header.contains("Max-Age=604800"));
    let cookie = cookie_header.split(';').next().unwrap().to_owned();
    let body = axum::body::to_bytes(response.into_body(), 4096)
        .await
        .unwrap();
    let login_body: Value = serde_json::from_slice(&body).unwrap();
    let listing = router(app.clone())
        .oneshot(
            Request::builder()
                .uri("/api/v3/auth/sessions")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(listing.status(), StatusCode::OK);
    let body = axum::body::to_bytes(listing.into_body(), 8192)
        .await
        .unwrap();
    let sessions: Value = serde_json::from_slice(&body).unwrap();
    let current = sessions["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["current"] == true)
        .unwrap();
    let id = current["id"].as_str().unwrap();
    let revoked = router(app.clone())
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/v3/auth/sessions/{id}"))
                .header("cookie", &cookie)
                .header("x-csrf-token", login_body["csrf_token"].as_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let denied = router(app)
        .oneshot(
            Request::builder()
                .uri("/api/v3/auth/me")
                .header("cookie", &cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
}
#[tokio::test]
async fn diagnostics_browser_route_uses_host_state_and_accounts_cookie() {
    let (_root, mut app) = fixture().await;
    let credential = "d".repeat(32);
    app.browser_login_bridge =
        browser_login::Bridge::configure(&credential, "fixture-diagnostics-owner").unwrap();
    let request = |origin: bool| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v3/diagnostics/browser-login")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {credential}"));
        if origin {
            builder = builder.header("origin", "https://fixture.invalid");
        }
        builder
            .body(Body::from(
                json!({"caller_id":"fixture-diagnostics-owner"}).to_string(),
            ))
            .unwrap()
    };
    assert_eq!(
        router(app.clone())
            .oneshot(request(true))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let response = router(app.clone()).oneshot(request(false)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let cookie = response.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    let response = router(app)
        .oneshot(
            Request::builder()
                .uri("/api/v3/auth/me")
                .header("cookie", cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}
