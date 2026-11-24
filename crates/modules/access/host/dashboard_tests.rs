//! The Dashboard and Plan routes: who may read them (owner and administrators; never members,
//! other owners or agent browsers) and what they read (plain counters, alerts, plan end date).
use super::*;
use crate::phase3_api::plus_tier_tests::{
    add_tier, call, member, owned_agent, put_tier, sign_in_as,
};
use crate::runtime_tests::fixture;
use axum::body::Body;
use axum::http::Request;
use tower::ServiceExt;
use webagent_accounts_host::AccountsStore;

#[tokio::test]
async fn the_dashboard_is_for_the_agents_owner_and_administrators_only() {
    let (_root, app) = fixture().await;
    add_tier(&app, "plus").await;
    let owner = member(&app, "dash-owner", "free").await;
    let other = member(&app, "dash-other", "free").await;
    let agent = owned_agent(&app, &owner).await;
    let uri = format!("/api/v3/agents/{agent}/dashboard");
    let mine = sign_in_as(&app, &owner).await;
    let theirs = sign_in_as(&app, &other).await;
    let admin = sign_in_as(&app, "admin").await;

    // Something to show: this agent's usage, another agent's, and today's alerts.
    let day = ledger::day(crate::now());
    sqlx::query("INSERT INTO usage_agent(agent_id,period,number,model,calls,input_tokens,output_tokens,cost_micros) VALUES(?,'day',?,'m1',3,120,40,NULL),('elsewhere','day',?,'m1',9,9,9,9)")
        .bind(&agent)
        .bind(day)
        .bind(day)
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    for (kind, level, limit) in [
        (
            ledger::agent_alert(ledger::ALERT_AGENT_USERS, &agent),
            80,
            20,
        ),
        (
            ledger::agent_alert(ledger::ALERT_AGENT_USERS, "elsewhere"),
            100,
            20,
        ),
        (ledger::ALERT_COMPUTE.to_owned(), 100, 900),
    ] {
        ledger::raise_owner_alert(&app.store.app_pool, &owner, &kind, day, level, limit)
            .await
            .unwrap();
    }

    // The owner's own storage state, in their own database (the Dashboard must not read
    // the server's base store or anyone else's).
    app.store
        .for_user(&owner)
        .await
        .unwrap()
        .put(
            crate::store::storage_limits::STATE_KEY,
            &json!({"used_bytes":12_345,"checked_at":7}),
        )
        .await
        .unwrap();

    // The administrator's own database (the router state's store) holds a different value:
    // nobody else's Dashboard may show it.
    app.store
        .put(
            crate::store::storage_limits::STATE_KEY,
            &json!({"used_bytes":999,"checked_at":7}),
        )
        .await
        .unwrap();

    // Allowed: the owner.
    let (status, page) = call(&app, "GET", &uri, &mine, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["viewer_is_owner"], true);
    assert_eq!(page["plan"]["tier"], "free");
    assert_eq!(page["plan"]["until"], Value::Null);
    assert_eq!(page["shared"]["agents_owned"]["used"], 1);
    assert_eq!(page["shared"]["runs_at_once"]["used"], 0);
    assert_eq!(page["shared"]["storage_bytes"]["used"], 12_345);
    let today = page["usage"]["today"].as_array().unwrap();
    assert_eq!(today.len(), 1);
    assert_eq!(
        (
            today[0]["model"].clone(),
            today[0]["calls"].clone(),
            today[0]["cost_micros"].clone()
        ),
        (json!("m1"), json!(3), Value::Null)
    );
    assert_eq!(page["users"]["last_30_days"]["used"], 0);
    assert!(page["users"]["levels"]
        .as_array()
        .unwrap()
        .iter()
        .any(|level| level["id"] == "member"));
    // Only this agent's alerts and the owner-wide ones, named.
    let notices: Vec<String> = page["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| notice["message"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert!(notices
        .iter()
        .any(|text| text.starts_with("Your agent “Plus test agent” has had 80%")));
    assert!(notices.iter().all(|text| !text.contains("guaranteed")));

    // A second owner sees only their own storage (none recorded), never the first owner's or
    // the administrator's; their billing summary is read from their own database too.
    let second = owned_agent(&app, &other).await;
    let (status, page) = call(
        &app,
        "GET",
        &format!("/api/v3/agents/{second}/dashboard"),
        &theirs,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["shared"]["storage_bytes"]["used"], Value::Null);
    #[cfg(all(unix, feature = "server-bins"))]
    {
        let (status, billing) = call(&app, "GET", "/api/v3/billing", &theirs, Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{billing}");
        assert_eq!(billing["summary"], Value::Null);
    }

    // The roster marks the owner's own agent, so the shell shows its Dashboard tab.
    let (_, roster) = call(&app, "GET", "/api/v3/models/agents", &mine, Value::Null).await;
    let listed = |roster: &Value| {
        roster["agents"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["id"] == agent.as_str())
            .map(|entry| entry["is_owner"].clone())
    };
    assert_eq!(listed(&roster), Some(json!(true)));
    let (_, roster) = call(&app, "GET", "/api/v3/models/agents", &admin, Value::Null).await;
    assert_ne!(listed(&roster), Some(json!(true)));

    // Refused: another person (not the owner, not an administrator), and the owner's agent
    // browser (agent browsers read neither route).
    let (status, _) = call(&app, "GET", &uri, &theirs, Value::Null).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let generation = app.store.account_by_id(&owner).await.unwrap().unwrap()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let browser = app
        .store
        .accounts_repository()
        .issue_trusted_browser_session(
            &owner,
            &generation,
            crate::now(),
            "Browser Control signed-in browser",
            600,
            webagent_accounts::AgentBrowserBinding {
                agent_id: &agent,
                allowed_session_ids: &[],
            },
        )
        .await
        .unwrap();
    for path in [uri.as_str(), "/api/v3/plan"] {
        let status = crate::router(app.clone())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("cookie", format!("v3_session={}", browser.token))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status();
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    // Refused: an unknown agent.
    let (status, _) = call(
        &app,
        "GET",
        "/api/v3/agents/agent-missing/dashboard",
        &mine,
        Value::Null,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Allowed: an administrator, who does not see the owner's own storage. A tier change from
    // the administrator's sign-in shows on the owner's next read, with its end date.
    let until = crate::now() + 7 * 86_400;
    assert_eq!(
        put_tier(&app, &admin, &owner, "plus", Some(until)).await,
        StatusCode::OK
    );
    let (status, page) = call(&app, "GET", &uri, &admin, Value::Null).await;
    assert_eq!(status, StatusCode::OK, "{page}");
    assert_eq!(page["viewer_is_owner"], false);
    assert_eq!(page["shared"]["storage_bytes"], Value::Null);
    assert_eq!(
        (page["plan"]["tier"].clone(), page["plan"]["until"].clone()),
        (json!("plus"), json!(until))
    );
    assert!(page["plan"]["name"].is_string());
    let (status, plan) = call(&app, "GET", "/api/v3/plan", &mine, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (plan["tier"].clone(), plan["until"].clone()),
        (json!("plus"), json!(until))
    );
    assert_eq!(plan["source"], "grant");
    // The other person's own plan is theirs alone.
    let (_, plan) = call(&app, "GET", "/api/v3/plan", &theirs, Value::Null).await;
    assert_eq!(
        (plan["tier"].clone(), plan["until"].clone()),
        (json!("free"), Value::Null)
    );
}
