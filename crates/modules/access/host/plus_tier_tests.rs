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
pub(crate) async fn add_tier(app: &App, id: &str) {
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
                owner_limits: Default::default(),
            },
            0,
        )
        .await
        .unwrap();
}

/// An active account on `tier`.
pub(crate) async fn member(app: &App, name: &str, tier: &str) -> String {
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
pub(crate) async fn owned_agent(app: &App, owner: &str) -> String {
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
pub(crate) async fn sign_in_as(app: &App, user: &str) -> String {
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
pub(crate) async fn call(
    app: &App,
    method: &str,
    uri: &str,
    token: &str,
    body: Value,
) -> (StatusCode, Value) {
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
pub(crate) async fn put_tier(
    app: &App,
    admin: &str,
    id: &str,
    tier: &str,
    until: Option<u64>,
) -> StatusCode {
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
    // Its own agent's models stay unchangeable from the agent browser.
    let agent = owned_agent(&app, &user).await;
    let uri: &'static str =
        Box::leak(format!("/api/v3/models/catalog?agent_id={agent}").into_boxed_str());
    assert_eq!(browser("PUT", uri).await, StatusCode::FORBIDDEN);
}

/// Free allows only `model-one` of the fixture catalog (both entries the administrator's).
async fn free_allows_model_one(app: &App) {
    let mut free = app.store.entitlement_tier("free").await.unwrap().unwrap();
    let expected = free.revision;
    free.allowed_models = vec!["model-one".into()];
    app.store
        .save_entitlement_tier(free, expected)
        .await
        .unwrap();
}

/// The agent's Models page as `token` sees it.
async fn models_page(app: &App, token: &str, agent: &str) -> (StatusCode, Value) {
    call(
        app,
        "GET",
        &format!("/api/v3/models/catalog?agent_id={agent}"),
        token,
        Value::Null,
    )
    .await
}

/// Save `page`'s catalog with `entry` as the agent's only (Standard) model.
async fn choose_model(
    app: &App,
    token: &str,
    agent: &str,
    owner: &str,
    page: &Value,
    entry: &str,
) -> StatusCode {
    let mut catalog = page["catalog"].clone();
    catalog["agent_models"][agent] = json!({"agent_id":agent,"owner_id":owner,"revision":1,
        "entry_ids":[entry],"roles":{"standard":{"entry_id":entry}}});
    call(
        app,
        "PUT",
        &format!("/api/v3/models/catalog?agent_id={agent}"),
        token,
        catalog,
    )
    .await
    .0
}

#[tokio::test]
async fn the_owners_tier_decides_which_models_run_on_its_agent_for_every_caller() {
    let (_root, app) = fixture().await;
    crate::runtime_tests::selection_fixture(&app).await;
    add_tier(&app, "plus").await;
    free_allows_model_one(&app).await;
    let plus_owner = member(&app, "plus-owner", "plus").await;
    let free_owner = member(&app, "free-owner", "free").await;
    let plus_agent = owned_agent(&app, &plus_owner).await;
    let free_agent = owned_agent(&app, &free_owner).await;
    let free_caller = member(&app, "free-caller", "free").await;
    let plus_caller = member(&app, "plus-caller", "plus").await;
    for (caller, agent) in [(&free_caller, &plus_agent), (&plus_caller, &free_agent)] {
        app.store
            .set_agent_membership(caller, agent, crate::access::Relationship::Member, 0)
            .await
            .unwrap();
    }
    // A Free caller on a Plus owner's agent runs every catalog model: the owner pays.
    let live = app
        .store
        .live_access(&free_caller, &plus_agent)
        .await
        .unwrap();
    assert!(live.model_allowed("model-two", "admin"));
    // A Plus caller on a Free owner's agent gets only what Free allows from the catalog ...
    let live = app
        .store
        .live_access(&plus_caller, &free_agent)
        .await
        .unwrap();
    assert!(live.model_allowed("model-one", "admin"));
    assert!(!live.model_allowed("model-two", "admin"));
    // ... and any model the owner connected itself, but not someone else's.
    assert!(live.model_allowed("any-own-model", &free_owner));
    assert!(!live.model_allowed("any-own-model", &plus_caller));

    // A downgrade from the administrator's sign-in stops the catalog model for every caller
    // at the next check; an upgrade brings it back. The owner's epoch moves each time.
    let admin = sign_in_as(&app, "admin").await;
    let before = epoch(&app, &plus_owner).await;
    assert_eq!(
        put_tier(&app, &admin, &plus_owner, "free", None).await,
        StatusCode::OK
    );
    assert!(epoch(&app, &plus_owner).await > before);
    let live = app
        .store
        .live_access(&free_caller, &plus_agent)
        .await
        .unwrap();
    assert!(!live.model_allowed("model-two", "admin"));
    assert!(live.model_allowed("model-one", "admin"));
    assert_eq!(
        put_tier(&app, &admin, &plus_owner, "plus", None).await,
        StatusCode::OK
    );
    let live = app
        .store
        .live_access(&free_caller, &plus_agent)
        .await
        .unwrap();
    assert!(live.model_allowed("model-two", "admin"));
}

#[tokio::test]
async fn an_owner_chooses_catalog_models_its_tier_allows_and_nobody_else_can() {
    let (_root, app) = fixture().await;
    crate::runtime_tests::selection_fixture(&app).await;
    add_tier(&app, "plus").await;
    free_allows_model_one(&app).await;
    let plus_owner = member(&app, "plus-owner", "plus").await;
    let free_owner = member(&app, "free-owner", "free").await;
    let plus_agent = owned_agent(&app, &plus_owner).await;
    let free_agent = owned_agent(&app, &free_owner).await;
    let visitor = member(&app, "agent-member", "plus").await;
    app.store
        .set_agent_membership(
            &visitor,
            &plus_agent,
            crate::access::Relationship::Member,
            0,
        )
        .await
        .unwrap();
    let plus_token = sign_in_as(&app, &plus_owner).await;
    let free_token = sign_in_as(&app, &free_owner).await;
    let member_token = sign_in_as(&app, &visitor).await;
    let available = |page: &Value, entry: &str| {
        page["available_entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == entry)
    };

    // Allowed: the Plus owner is offered both catalog models and keeps the one it chooses.
    let (status, page) = models_page(&app, &plus_token, &plus_agent).await;
    assert_eq!(status, StatusCode::OK);
    assert!(available(&page, "one") && available(&page, "two"));
    assert_eq!(
        choose_model(&app, &plus_token, &plus_agent, &plus_owner, &page, "two").await,
        StatusCode::OK
    );
    let stored = app.store.model_catalog().await.unwrap();
    assert!(stored.entries["two"].allowed_agents.contains(&plus_agent));
    assert!(stored.connections["two"]
        .allowed_agents
        .contains(&plus_agent));
    // The offered model it did not choose is not stored for its agent.
    assert!(!stored.entries["one"].allowed_agents.contains(&plus_agent));
    assert!(!stored.connections["one"]
        .allowed_agents
        .contains(&plus_agent));

    // Refused: a Free owner is offered only what Free allows and cannot choose the other.
    let (status, page) = models_page(&app, &free_token, &free_agent).await;
    assert_eq!(status, StatusCode::OK);
    assert!(available(&page, "one") && !available(&page, "two"));
    assert_ne!(
        choose_model(&app, &free_token, &free_agent, &free_owner, &page, "two").await,
        StatusCode::OK
    );
    assert_eq!(
        choose_model(&app, &free_token, &free_agent, &free_owner, &page, "one").await,
        StatusCode::OK
    );
    let after = app.store.model_catalog().await.unwrap();
    assert!(!after.entries["two"].allowed_agents.contains(&free_agent));
    assert!(after.entries["one"].allowed_agents.contains(&free_agent));

    // Refused: a member of the agent, and an owner on another owner's agent.
    assert_eq!(
        models_page(&app, &member_token, &plus_agent).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        choose_model(&app, &member_token, &plus_agent, &plus_owner, &page, "two").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        models_page(&app, &free_token, &plus_agent).await.0,
        StatusCode::FORBIDDEN
    );

    // A downgrade keeps the chosen model stored but no longer available or offered.
    let admin = sign_in_as(&app, "admin").await;
    assert_eq!(
        put_tier(&app, &admin, &plus_owner, "free", None).await,
        StatusCode::OK
    );
    let (_, page) = models_page(&app, &plus_token, &plus_agent).await;
    assert!(available(&page, "one") && !available(&page, "two"));
    let kept = app.store.model_catalog().await.unwrap();
    assert!(kept.entries["two"].allowed_agents.contains(&plus_agent));
    assert_eq!(kept.agent_models[&plus_agent].entry_ids, ["two".to_owned()]);
}

#[tokio::test]
async fn a_lower_tier_lets_runs_finish_and_disabling_the_account_stops_them() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let owner = member(&app, "plus-runner", "plus").await;
    let agent = owned_agent(&app, &owner).await;
    let owner_store = app.store.for_user(&owner).await.unwrap();
    let sid = crate::uuid();
    owner_store
        .put(
            &crate::key("session", &sid),
            &json!({"id":sid,"agent_id":agent}),
        )
        .await
        .unwrap();
    let rid = crate::uuid();
    let mut run = json!({"id":rid,"session_id":sid,"agent_id":agent,"status":"queued",
        "requester_user_id":owner,"run_revision":0,"lease_epoch":0,"generation":"fixture",
        "input":[]});
    owner_store
        .save_run(&mut run, "run_status", &json!({}))
        .await
        .unwrap();
    let status = |store: crate::Store, rid: String| async move {
        store.get(&format!("run:{rid}")).await.unwrap().unwrap()["status"].clone()
    };
    let admin = sign_in_as(&app, "admin").await;
    // Plus to Free: the queued run keeps going.
    assert_eq!(
        put_tier(&app, &admin, &owner, "free", None).await,
        StatusCode::OK
    );
    assert_eq!(status(owner_store.clone(), rid.clone()).await, "queued");
    // Disabled: stopped now.
    let current = account(&app, &owner).await;
    assert_eq!(
        call(
            &app,
            "PUT",
            &format!("/api/v3/admin/access/accounts/{owner}"),
            &admin,
            json!({"expected_revision":revision(&current),"status":"disabled","tier_id":"free"}),
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(status(owner_store, rid).await, "stopped");
}
