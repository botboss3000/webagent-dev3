//! Plus tier wiring: rank-inherited ability access for agent owners, Plus-only groups, tier
//! changes from another sign-in, and tier end dates driven by `account_due` rows.
use super::*;
use crate::access::capabilities::Exposure;
use crate::access::Permission;
use crate::runtime_tests::fixture;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;

/// A tier above Free (inserted just above it, as the Tiers page does), with every catalog
/// model and the Plus run limit.
async fn add_tier(app: &App, id: &str) {
    app.store
        .save_entitlement_tier(
            Tier {
                id: id.into(),
                revision: 0,
                allowed_models: vec!["*".into()],
                model_policy: None,
                max_sessions: 100,
                max_active_runs: 25,
                daily_turns: None,
                daily_cost_usd: None,
            },
            0,
        )
        .await
        .unwrap();
}

/// An active account on `tier`.
async fn member(app: &App, name: &str, tier: &str) -> String {
    let account = app
        .store
        .create_user_account(name, "fixture-long-password-hash")
        .await
        .unwrap();
    let id = account["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(&id, 1, AccountStatus::Active, tier)
        .await
        .unwrap();
    id
}

/// An agent `owner` creates. Levels allow every ability unless one is switched off; its
/// Operator level saves one setting for the Plus-only `github` (always loaded), which a
/// downgrade must keep.
async fn owned_agent(app: &App, owner: &str) -> String {
    let Json(created) = crate::agents_api::create_agent(
        State(app.clone()),
        Extension(json!({"_user_id":owner})),
        Json(crate::agents_api::NewAgent {
            client_request_id: format!("{owner}-agent"),
            name: "Plus test agent".into(),
            description: None,
        }),
    )
    .await
    .unwrap();
    let agent = created["id"].as_str().unwrap().to_owned();
    let levels = app.store.capability_levels(&agent).await.unwrap();
    let mut level = levels.levels["operator"].clone();
    level
        .ability_exposure
        .insert("github".into(), Exposure::Loaded);
    app.store
        .save_capability_level(&agent, level, levels.revision)
        .await
        .unwrap();
    agent
}

fn revision(account: &Value) -> u64 {
    account["access_revision"].as_u64().unwrap()
}

async fn account(app: &App, id: &str) -> Value {
    app.store.account_by_id(id).await.unwrap().unwrap()
}

/// A sign-in record for `user` at its current generation; returns the cookie value.
async fn sign_in_as(app: &App, user: &str) -> String {
    let token = crate::uuid();
    let record = account(app, user).await;
    app.store
        .put(
            &format!("auth:{}", crate::digest(&json!(token))),
            &json!({"id":crate::uuid(),"user_id":user,"csrf":"fixture-csrf",
                "generation":record["generation"],"expires":crate::now()+3600}),
        )
        .await
        .unwrap();
    token
}

/// One request with a sign-in cookie; returns the status and the JSON body.
async fn call(app: &App, method: &str, uri: &str, token: &str, body: Value) -> (StatusCode, Value) {
    let response = crate::router(app.clone())
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
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// The administrator's tier change for `id` through `PUT /admin/access/accounts/{id}`.
async fn put_tier(app: &App, admin: &str, id: &str, tier: &str, until: Option<u64>) -> StatusCode {
    let current = account(app, id).await;
    call(
        app,
        "PUT",
        &format!("/api/v3/admin/access/accounts/{id}"),
        admin,
        json!({"expected_revision":revision(&current),"status":"active","tier_id":tier,
            "tier_until":until}),
    )
    .await
    .0
}

async fn epoch(app: &App, id: &str) -> i64 {
    app.store
        .accounts_repository()
        .auth_epoch(id)
        .await
        .unwrap()
        .unwrap()
}

async fn due_rows(app: &App, id: &str) -> Vec<u64> {
    webagent_engine::due::all(&app.store.app_pool)
        .await
        .unwrap()
        .into_iter()
        .filter(|(user, kind, _)| user == id && kind == TIER_END_DUE)
        .map(|(_, _, at)| at)
        .collect()
}

#[tokio::test]
async fn a_plus_owner_gets_free_and_plus_abilities_and_a_downgrade_pauses_them_only() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let owner = member(&app, "plus-owner", "plus").await;
    let agent = owned_agent(&app, &owner).await;
    let sid = crate::uuid();
    let owner_store = app.store.for_user(&owner).await.unwrap();
    owner_store
        .put(
            &crate::key("session", &sid),
            &json!({"id":sid,"agent_id":agent}),
        )
        .await
        .unwrap();

    // Allowed: a Plus owner has the Free ability, inherited, and the Plus-only one.
    let live = app.store.live_access(&owner, &agent).await.unwrap();
    assert_eq!(live.owner_tiers, ["plus", "free", "anonymous"]);
    assert_ne!(live.ability_permission("web_access"), Permission::Deny);
    assert_ne!(live.ability_permission("github"), Permission::Deny);
    assert_eq!(live.tier.max_active_runs, 25);
    // Refused: Plus never reaches an Administrator ability, nor an administrator route.
    assert_eq!(live.ability_permission("codebase_admin"), Permission::Deny);
    assert!(!live.ability_offered("codebase_admin"));
    let plus_sign_in = sign_in_as(&app, &owner).await;
    for uri in ["/api/v3/admin/access/accounts", "/api/v3/status"] {
        assert_eq!(
            call(&app, "GET", uri, &plus_sign_in, Value::Null).await.0,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
    }

    // Downgrade to Free: the Plus-only ability stops, the Free one stays.
    let admin = sign_in_as(&app, "admin").await;
    assert_eq!(
        put_tier(&app, &admin, &owner, "free", None).await,
        StatusCode::OK
    );
    let live = app.store.live_access(&owner, &agent).await.unwrap();
    assert_ne!(live.ability_permission("web_access"), Permission::Deny);
    assert_eq!(live.ability_permission("github"), Permission::Deny);
    // Paused, not removed: still shown with the tier reason, and every setting and record kept.
    assert!(live.ability_offered("github"));
    let levels = app.store.capability_levels(&agent).await.unwrap();
    assert_eq!(
        levels.levels["operator"].ability_exposure.get("github"),
        Some(&Exposure::Loaded)
    );
    assert!(owner_store
        .get(&crate::key("session", &sid))
        .await
        .unwrap()
        .is_some());
    // An upgrade brings it back with the same settings.
    assert_eq!(
        put_tier(&app, &admin, &owner, "plus", None).await,
        StatusCode::OK
    );
    let live = app.store.live_access(&owner, &agent).await.unwrap();
    assert_ne!(live.ability_permission("github"), Permission::Deny);
}

#[tokio::test]
async fn an_unknown_owner_tier_gets_only_the_lowest_tier_and_never_plus() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    add_tier(&app, "gold").await;
    let owner = member(&app, "gold-owner", "gold").await;
    let agent = owned_agent(&app, &owner).await;
    let caller = member(&app, "gold-caller", "plus").await;
    app.store
        .set_agent_membership(&caller, &agent, crate::access::Relationship::Member, 0)
        .await
        .unwrap();
    // A tier added later sits just above Free: it inherits Free's abilities, not Plus's.
    let live = app.store.live_access(&caller, &agent).await.unwrap();
    assert_eq!(live.owner_tiers, ["gold", "free", "anonymous"]);
    assert_ne!(live.ability_permission("web_access"), Permission::Deny);
    assert_eq!(live.ability_permission("github"), Permission::Deny);
    // The owner's tier leaves the policy: it is unknown from the next check on.
    let mut policy = app.store.read_access_policy().unwrap();
    policy["tiers"].as_object_mut().unwrap().remove("gold");
    policy["tier_order"] = json!(["administrator", "plus", "free", "anonymous"]);
    policy["revision"] = json!(policy["revision"].as_u64().unwrap() + 1);
    app.store.write_access_policy(&policy).unwrap();
    let live = app.store.live_access(&caller, &agent).await.unwrap();
    assert_eq!(live.owner_tiers, ["anonymous"]);
    for ability in ["web_access", "github", "codebase_admin"] {
        assert_eq!(
            live.ability_permission(ability),
            Permission::Deny,
            "{ability}"
        );
    }
    // The owner on that tier is refused outright.
    assert!(app.store.live_access(&owner, &agent).await.is_err());
}

#[tokio::test]
async fn a_tier_change_from_another_sign_in_applies_on_the_next_request() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let user = member(&app, "second-sign-in", "free").await;
    let first = sign_in_as(&app, &user).await;
    let (status, me) = call(&app, "GET", "/api/v3/auth/me", &first, Value::Null).await;
    assert_eq!(
        (status, me["tier_id"].clone()),
        (StatusCode::OK, json!("free"))
    );
    let admin = sign_in_as(&app, "admin").await;
    let before = epoch(&app, &user).await;
    assert_eq!(
        put_tier(&app, &admin, &user, "plus", None).await,
        StatusCode::OK
    );
    assert!(epoch(&app, &user).await > before);
    let (status, me) = call(&app, "GET", "/api/v3/auth/me", &first, Value::Null).await;
    assert_eq!(
        (status, me["tier_id"].clone()),
        (StatusCode::OK, json!("plus"))
    );
    // Refused: the member cannot set its own tier.
    assert_eq!(
        put_tier(&app, &first, &user, "administrator", None).await,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_tier_end_date_returns_the_account_to_free_through_its_due_row() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let user = member(&app, "granted", "free").await;
    let admin = sign_in_as(&app, "admin").await;
    let now = crate::now();
    // Refused: an end in the past, and an end on a tier that cannot end.
    for (tier, until) in [
        ("plus", now - 1),
        ("free", now + 600),
        ("administrator", now + 600),
    ] {
        assert_eq!(
            put_tier(&app, &admin, &user, tier, Some(until)).await,
            StatusCode::BAD_REQUEST,
            "{tier}"
        );
    }
    assert!(due_rows(&app, &user).await.is_empty());
    // Allowed: Plus until a time, stored in the record and in one due row.
    let until = now + 600;
    assert_eq!(
        put_tier(&app, &admin, &user, "plus", Some(until)).await,
        StatusCode::OK
    );
    assert_eq!(account(&app, &user).await["tier_until"], json!(until));
    assert_eq!(due_rows(&app, &user).await, [until]);
    let (_, list) = call(
        &app,
        "GET",
        "/api/v3/admin/access/accounts",
        &admin,
        Value::Null,
    )
    .await;
    let listed = list["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["user_id"] == user.as_str())
        .unwrap()
        .clone();
    assert_eq!(listed["tier_until"], json!(until));
    // Before the end nothing changes; a due row ahead of the record only reschedules.
    assert_eq!(end_due_tiers(&app, until - 1).await.unwrap(), 0);
    webagent_engine::due::set(&app.store.app_pool, &user, TIER_END_DUE, Some(now))
        .await
        .unwrap();
    assert_eq!(end_due_tiers(&app, now + 1).await.unwrap(), 0);
    assert_eq!(due_rows(&app, &user).await, [until]);
    assert_eq!(account(&app, &user).await["tier_id"], "plus");
    // At the end: Free, through the account writer, and the row is gone.
    let before = epoch(&app, &user).await;
    assert_eq!(end_due_tiers(&app, until).await.unwrap(), 1);
    let ended = account(&app, &user).await;
    assert_eq!(ended["tier_id"], "free");
    assert!(ended.get("tier_until").is_none());
    assert!(epoch(&app, &user).await > before);
    assert!(due_rows(&app, &user).await.is_empty());
    // An end date removed before it passes leaves the tier as it is.
    assert_eq!(
        put_tier(&app, &admin, &user, "plus", Some(now + 900)).await,
        StatusCode::OK
    );
    assert_eq!(
        put_tier(&app, &admin, &user, "plus", None).await,
        StatusCode::OK
    );
    assert!(due_rows(&app, &user).await.is_empty());
    webagent_engine::due::set(&app.store.app_pool, &user, TIER_END_DUE, Some(now))
        .await
        .unwrap();
    assert_eq!(end_due_tiers(&app, now + 1000).await.unwrap(), 0);
    assert_eq!(account(&app, &user).await["tier_id"], "plus");
    assert!(due_rows(&app, &user).await.is_empty());
}

#[tokio::test]
async fn an_agent_browser_of_a_plus_account_stays_read_only() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let user = member(&app, "plus-browser", "plus").await;
    let generation = account(&app, &user).await["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let issued = app
        .store
        .accounts_repository()
        .issue_trusted_browser_session(
            &user,
            &generation,
            crate::now(),
            "Browser Control signed-in browser",
            600,
            webagent_accounts::AgentBrowserBinding {
                agent_id: "shared_default",
                allowed_session_ids: &[],
            },
        )
        .await
        .unwrap();
    let browser = |method: &'static str, uri: &'static str| {
        let app = app.clone();
        let (token, csrf) = (issued.token.clone(), issued.csrf.clone());
        async move {
            crate::router(app)
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("cookie", format!("v3_session={token}"))
                        .header("x-csrf-token", csrf)
                        .header("content-type", "application/json")
                        .body(Body::from("{}"))
                        .unwrap(),
                )
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(browser("GET", "/api/v3/auth/me").await, StatusCode::OK);
    assert_eq!(
        browser("POST", "/api/v3/sessions").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        browser("GET", "/api/v3/admin/access/accounts").await,
        StatusCode::FORBIDDEN
    );
}
