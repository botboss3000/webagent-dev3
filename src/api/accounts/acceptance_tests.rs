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
async fn retired_browser_login_route_is_gone_and_its_variables_are_ignored() {
    let (_root, app) = fixture().await;
    let bearer = "d".repeat(32);
    let post = |cookie: Option<String>| {
        let mut builder = Request::builder()
            .method("POST")
            .uri("/api/v3/diagnostics/browser-login")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {bearer}"))
            .header("x-csrf-token", "fixture-csrf");
        if let Some(cookie) = cookie {
            builder = builder.header("cookie", format!("v3_session={cookie}"));
        }
        builder
            .body(Body::from(json!({"caller_id":"admin"}).to_string()))
            .unwrap()
    };
    // Without a sign-in the bearer no longer signs anyone in.
    let anonymous = router(app.clone()).oneshot(post(None)).await.unwrap();
    assert!(anonymous.headers().get("set-cookie").is_none());
    assert!(matches!(
        anonymous.status(),
        StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED
    ));
    // A signed-in administrator reaches the router's end: the route does not exist.
    let admin = sign_in_as(&app, "admin").await;
    let signed_in = router(app.clone())
        .oneshot(post(Some(admin)))
        .await
        .unwrap();
    assert_eq!(signed_in.status(), StatusCode::NOT_FOUND);
    assert!(signed_in.headers().get("set-cookie").is_none());
    // Present variables are named in one warning and otherwise ignored; absent ones are silent.
    assert_eq!(
        crate::warn_retired_environment(|_| true),
        ["V3_BROWSER_LOGIN_TOKEN", "V3_BROWSER_LOGIN_CALLER_ID"]
    );
    assert!(crate::warn_retired_environment(|_| false).is_empty());
}

/// A sign-in record for `user` at the account's current generation; returns the cookie value.
async fn sign_in_as(app: &App, user: &str) -> String {
    let token = uuid();
    let account = app.store.account_by_id(user).await.unwrap().unwrap();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"id":uuid(),"user_id":user,"csrf":"fixture-csrf",
                "generation":account["generation"],"expires":now()+3600}),
        )
        .await
        .unwrap();
    token
}
async fn call(app: &App, method: &str, uri: &str, token: &str, body: Value) -> StatusCode {
    router(app.clone())
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("cookie", format!("v3_session={token}"))
                .header("x-csrf-token", "fixture-csrf")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}
async fn me_status(app: &App, token: &str) -> StatusCode {
    call(app, "GET", "/api/v3/auth/me", token, Value::Null).await
}
async fn epoch(app: &App, user: &str) -> i64 {
    app.store
        .accounts_repository()
        .auth_epoch(user)
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn each_identity_change_ends_a_remembered_identity_on_the_next_request() {
    let (_root, app) = fixture().await;
    let member = app
        .store
        .create_user_account("epoch-member", "long-hash-placeholder")
        .await
        .unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tier = app.store.full_user_tier().await;
    app.store
        .update_account_access(&member, 1, crate::access::AccountStatus::Active, tier)
        .await
        .unwrap();
    let revision = |app: App, member: String| async move {
        app.store.account_by_id(&member).await.unwrap().unwrap()["access_revision"]
            .as_u64()
            .unwrap()
    };
    // `first` is the remembered sign-in; every change is made elsewhere (`second`, the
    // administrator's routes or the store), then `first` asks again.
    let first = sign_in_as(&app, &member).await;
    let second = sign_in_as(&app, &member).await;
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(
        remembered_epoch(&app, &first),
        Some(epoch(&app, &member).await)
    );
    // Unchanged records: the remembered identity is used again, not rebuilt.
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);

    // Username change from the second sign-in.
    let before = epoch(&app, &member).await;
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v3/auth/profile",
            &second,
            json!({"username":"epoch-renamed","display_name":"Renamed"}),
        )
        .await,
        StatusCode::OK
    );
    assert!(epoch(&app, &member).await > before);
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(
        remembered_epoch(&app, &first),
        Some(epoch(&app, &member).await),
        "username change"
    );

    // Tier edit: every member of the tier moves.
    let mut edited = app.store.entitlement_tier(tier).await.unwrap().unwrap();
    let tier_revision = edited.revision;
    edited.max_sessions -= 1;
    let before = epoch(&app, &member).await;
    app.store
        .save_entitlement_tier(edited, tier_revision)
        .await
        .unwrap();
    assert!(epoch(&app, &member).await > before, "tier edit");
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(
        remembered_epoch(&app, &first),
        Some(epoch(&app, &member).await)
    );

    // Role and tier change (administrator's access update).
    let current = revision(app.clone(), member.clone()).await;
    app.store
        .update_account_access(
            &member,
            current,
            crate::access::AccountStatus::Active,
            crate::access::ADMINISTRATOR_TIER,
        )
        .await
        .unwrap();
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(
        remembered_epoch(&app, &first),
        Some(epoch(&app, &member).await),
        "role change"
    );

    // Disabling the account ends both sign-ins on their next request.
    let current = revision(app.clone(), member.clone()).await;
    app.store
        .update_account_access(
            &member,
            current,
            crate::access::AccountStatus::Disabled,
            crate::access::ADMINISTRATOR_TIER,
        )
        .await
        .unwrap();
    assert_eq!(me_status(&app, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(remembered_epoch(&app, &first), None);
    let current = revision(app.clone(), member.clone()).await;
    app.store
        .update_account_access(&member, current, crate::access::AccountStatus::Active, tier)
        .await
        .unwrap();

    // Revoking one sign-in from another.
    let first = sign_in_as(&app, &member).await;
    let second = sign_in_as(&app, &member).await;
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    let first_id = app
        .store
        .get(&format!("auth:{}", digest(&json!(first))))
        .await
        .unwrap()
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        call(
            &app,
            "DELETE",
            &format!("/api/v3/auth/sessions/{first_id}"),
            &second,
            Value::Null,
        )
        .await,
        StatusCode::OK
    );
    assert_eq!(me_status(&app, &first).await, StatusCode::UNAUTHORIZED);

    // Signing out ends that sign-in only.
    let first = sign_in_as(&app, &member).await;
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    assert_eq!(me_status(&app, &second).await, StatusCode::OK);
    assert_eq!(
        call(&app, "POST", "/api/v3/auth/logout", &first, Value::Null).await,
        StatusCode::OK
    );
    assert_eq!(me_status(&app, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&app, &second).await, StatusCode::OK);

    // A password change elsewhere ends every older sign-in (new account generation).
    let first = sign_in_as(&app, &member).await;
    assert_eq!(me_status(&app, &first).await, StatusCode::OK);
    let generation = app.store.account_by_id(&member).await.unwrap().unwrap()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    app.store
        .accounts_repository()
        .replace_user_password(
            &member,
            &generation,
            "replacement-hash-placeholder",
            "rotated",
        )
        .await
        .unwrap();
    assert_eq!(me_status(&app, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&app, &second).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn each_credential_kind_has_its_refused_and_allowed_side() {
    let (_root, app) = fixture().await;
    let generation = app.store.account_by_id("admin").await.unwrap().unwrap()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    // A person: the administrator's own sign-in reaches admin routes and may change things.
    let person = sign_in_as(&app, "admin").await;
    assert_eq!(
        call(&app, "GET", "/api/v3/status", &person, Value::Null).await,
        StatusCode::OK
    );
    assert_eq!(
        call(
            &app,
            "PUT",
            "/api/v3/auth/session-policy",
            &person,
            json!({"days":7})
        )
        .await,
        StatusCode::OK
    );
    // An agent's browser signed in as the same administrator: reads only, never admin routes.
    let issued = app
        .store
        .accounts_repository()
        .issue_trusted_browser_session(
            "admin",
            &generation,
            now(),
            "Browser Control signed-in browser",
            600,
            webagent_accounts::AgentBrowserBinding {
                agent_id: "shared_default",
                allowed_session_ids: &["s1".to_owned()],
            },
        )
        .await
        .unwrap();
    let browser = |method: &'static str, uri: &'static str, body: Value| {
        let app = app.clone();
        let token = issued.token.clone();
        let csrf = issued.csrf.clone();
        async move {
            router(app)
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("cookie", format!("v3_session={token}"))
                        .header("x-csrf-token", csrf)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        browser("GET", "/api/v3/auth/me", Value::Null).await,
        StatusCode::OK
    );
    assert_eq!(
        browser("GET", "/api/v3/sessions", Value::Null).await,
        StatusCode::OK
    );
    for (method, uri) in [
        ("PUT", "/api/v3/auth/session-policy"),
        ("POST", "/api/v3/sessions"),
        ("POST", "/api/v3/approvals/a/decision"),
        ("PUT", "/api/v3/auth/profile"),
    ] {
        assert_eq!(
            browser(method, uri, json!({"days":7})).await,
            StatusCode::FORBIDDEN,
            "{method} {uri}"
        );
    }
    assert_eq!(
        browser("GET", "/api/v3/status", Value::Null).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser("GET", "/api/v3/admin/events", Value::Null).await,
        StatusCode::FORBIDDEN
    );
    // A sign-in made before records had a kind is recognised by its label.
    let legacy = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(legacy))),
            &json!({"id":uuid(),"user_id":"admin","csrf":"fixture-csrf","generation":generation,
                "user_agent":"Browser Control signed-in browser","expires":now()+3600}),
        )
        .await
        .unwrap();
    assert_eq!(
        call(&app, "GET", "/api/v3/status", &legacy, Value::Null).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "POST", "/api/v3/sessions", &legacy, json!({})).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, "GET", "/api/v3/auth/me", &legacy, Value::Null).await,
        StatusCode::OK
    );
}
