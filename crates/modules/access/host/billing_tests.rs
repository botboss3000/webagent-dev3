//! The billing connection against a test double of the private service on a temporary
//! socket: grants through the account writer (refused for unknown, Free and Administrator
//! tiers, applied once per reference), the two grant sources, the catalog switch, fixed
//! notices, plan summaries, usage reading, link requests and the peer and line checks.
use super::*;
use crate::access::{AccountStatus, Tier};
use crate::runtime_tests::fixture;
use std::os::unix::fs::MetadataExt;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

async fn add_plus(app: &App) {
    app.store
        .save_entitlement_tier(
            Tier {
                id: "plus".into(),
                revision: 0,
                allowed_models: vec!["*".into()],
                model_policy: None,
                max_sessions: 100,
                max_active_runs: 9,
                daily_turns: None,
                owner_limits: Default::default(),
            },
            0,
        )
        .await
        .unwrap();
}

async fn member(app: &App, name: &str) -> String {
    let account = app
        .store
        .create_user_account(name, "fixture-long-password-hash")
        .await
        .unwrap();
    let id = account["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(&id, 1, AccountStatus::Active, "free")
        .await
        .unwrap();
    id
}

async fn epoch(app: &App, user: &str) -> i64 {
    sqlx::query_scalar("SELECT auth_epoch FROM user_accounts WHERE user_id=?")
        .bind(user)
        .fetch_one(&app.store.app_pool)
        .await
        .unwrap()
}

async fn tier(app: &App, user: &str) -> String {
    app.store.account_by_id(user).await.unwrap().unwrap()["tier_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// The service end of one connection.
struct Fake {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    next: u32,
    /// Messages the app sent on its own (owner tiers, usage hints).
    seen: Vec<Value>,
}

impl Fake {
    async fn line(&mut self) -> Option<Value> {
        read_line(&mut self.reader).await.unwrap()
    }
    async fn write(&mut self, message: &Value) {
        let mut line = serde_json::to_string(message).unwrap();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }
    /// Send a request and return its reply, answering anything the app sends meanwhile.
    async fn ask(&mut self, mut message: Value) -> Value {
        self.next += 1;
        let id = format!("s{}", self.next);
        message["v"] = json!(1);
        message["id"] = json!(id);
        self.write(&message).await;
        loop {
            let reply = self.line().await.expect("app closed the connection");
            if reply["re"] == id.as_str() {
                return reply;
            }
            if let Some(theirs) = reply["id"].as_str() {
                let answer = json!({"v":1,"re":theirs,"ok":true});
                self.write(&answer).await;
            }
            self.seen.push(reply);
        }
    }
}

#[tokio::test]
async fn the_billing_service_grants_switches_and_reads_only_what_the_protocol_allows() {
    let (root, app) = fixture().await;
    add_plus(&app).await;
    let payer = member(&app, "payer").await;
    let path = root.path().join("billing.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let me = std::fs::metadata(root.path()).unwrap().uid();

    // Refused: a peer that is not the configured service account; nothing is read from it.
    let wrong = {
        let app = app.clone();
        let path = path.to_str().unwrap().to_owned();
        tokio::spawn(async move { connect_once(&app, &path, me + 1, Duration::from_secs(5)).await })
    };
    let _ignored = listener.accept().await.unwrap();
    let refused = wrong.await.unwrap().unwrap_err().to_string();
    assert!(refused.contains("billing peer refused"), "{refused}");

    // Allowed: the service's own account; the handshake carries the purchase offers.
    let session = {
        let app = app.clone();
        let path = path.to_str().unwrap().to_owned();
        tokio::spawn(async move { connect_once(&app, &path, me, Duration::from_secs(5)).await })
    };
    let (stream, _) = listener.accept().await.unwrap();
    let (read, writer) = stream.into_split();
    let mut fake = Fake {
        reader: BufReader::new(read),
        writer,
        next: 0,
        seen: Vec::new(),
    };
    let hello = fake.line().await.unwrap();
    assert_eq!(
        (hello["type"].as_str(), hello["protocol"].as_u64()),
        (Some("hello"), Some(1))
    );
    fake.write(&json!({"v":1,"re":"h1","ok":true,"protocol":1,
        "offers":[{"what":"plus_subscription","label":"Plus, monthly"},{"what":"BAD CODE","label":"x"}]}))
        .await;
    let tiers = fake.line().await.unwrap();
    assert_eq!(tiers["type"], "owner_tiers");
    fake.write(&json!({"v":1,"re":tiers["id"],"ok":true})).await;

    // Unknown request type: refused, connection kept.
    assert_eq!(
        fake.ask(json!({"type":"make_admin","user":payer})).await["error"],
        "unknown_type"
    );

    // Refused grants change nothing: Administrator, Free, unknown tiers, a past end, an
    // unknown account, a bad reference.
    let before = epoch(&app, &payer).await;
    let until = crate::now() + 30 * 86_400;
    for (tier_id, end, user, reference, code) in [
        (
            "administrator",
            until,
            payer.as_str(),
            "r-admin",
            "tier_not_grantable",
        ),
        (
            "free",
            until,
            payer.as_str(),
            "r-free",
            "tier_not_grantable",
        ),
        (
            "gold",
            until,
            payer.as_str(),
            "r-gold",
            "tier_not_grantable",
        ),
        ("plus", 1, payer.as_str(), "r-past", "bad_until"),
        ("plus", until, "nobody", "r-nobody", "unknown_user"),
        (
            "plus",
            until,
            payer.as_str(),
            "bad reference!",
            "bad_request",
        ),
    ] {
        let reply = fake
            .ask(json!({"type":"grant_tier","user":user,"tier":tier_id,"until":end,"reference":reference}))
            .await;
        assert_eq!(reply["error"], code, "{tier_id} {user} {reference}");
    }
    assert_eq!(
        (tier(&app, &payer).await, epoch(&app, &payer).await),
        ("free".into(), before)
    );

    // Allowed: a Plus grant through the account writer (auth_epoch rises), with its end row.
    let grant = json!({"type":"grant_tier","user":payer,"tier":"plus","until":until,"reference":"stripe:evt_1"});
    let reply = fake.ask(grant.clone()).await;
    assert_eq!(
        (reply["ok"].as_bool(), reply["tier"].as_str()),
        (Some(true), Some("plus"))
    );
    assert_eq!(tier(&app, &payer).await, "plus");
    let granted = epoch(&app, &payer).await;
    assert!(granted > before);
    let due: Option<i64> = sqlx::query_scalar(
        "SELECT next_due_at FROM account_due WHERE user_id=? AND kind='paid_end'",
    )
    .bind(&payer)
    .fetch_optional(&app.store.app_pool)
    .await
    .unwrap();
    assert_eq!(due, Some(i64::try_from(until).unwrap()));
    // A replay changes nothing; the same reference with other fields is refused.
    assert_eq!(fake.ask(grant.clone()).await["duplicate"], true);
    let mut forged = grant.clone();
    forged["until"] = json!(until + 1);
    assert_eq!(fake.ask(forged).await["error"], "reference_conflict");
    assert_eq!(epoch(&app, &payer).await, granted);
    // The service heard of the change.
    assert!(fake.seen.iter().any(|m| m["type"] == "owner_tier"
        && m["user"] == payer.as_str()
        && m["source"] == "paid"));

    // Two sources: the administrator's own choice is Free while the paid grant holds Plus; the
    // administrator grants Plus with an end too; ending the paid grant keeps Plus (the
    // administrator's grant), and that grant's end then brings Free.
    let revision = app.store.account_by_id(&payer).await.unwrap().unwrap()["access_revision"]
        .as_u64()
        .unwrap();
    app.store
        .update_account_tier(&payer, revision, AccountStatus::Active, "free", None)
        .await
        .unwrap();
    assert_eq!(tier(&app, &payer).await, "plus");
    let revision = app.store.account_by_id(&payer).await.unwrap().unwrap()["access_revision"]
        .as_u64()
        .unwrap();
    let admin_end = crate::now() + 86_400;
    app.store
        .update_account_tier(
            &payer,
            revision,
            AccountStatus::Active,
            "plus",
            Some(admin_end),
        )
        .await
        .unwrap();
    assert_eq!(
        fake.ask(json!({"type":"end_grant","user":payer,"reference":"stripe:refund_1","grant_reference":"other"}))
            .await["noop"],
        true
    );
    let ended = fake
        .ask(json!({"type":"end_grant","user":payer,"reference":"stripe:refund_2","grant_reference":"stripe:evt_1"}))
        .await;
    assert_eq!(ended["tier"], "plus");
    assert!(app
        .store
        .end_passed_grants(&payer, admin_end + 1)
        .await
        .unwrap()
        .is_some());
    assert_eq!(tier(&app, &payer).await, "free");

    // The catalog switch (on a Plus owner, whose tier allows every catalog model): off stops
    // catalog models on the owner's agents for every caller,
    // own models stay; on again restores them. Read from the access the run already builds.
    let Json(created) = crate::agents_api::create_agent(
        crate::State(app.clone()),
        Extension(json!({"_user_id":payer})),
        Json(crate::agents_api::NewAgent {
            client_request_id: "payer-agent".into(),
            name: "Billing test agent".into(),
            description: None,
        }),
    )
    .await
    .unwrap();
    let agent = created["id"].as_str().unwrap().to_owned();
    let revision = app.store.account_by_id(&payer).await.unwrap().unwrap()["access_revision"]
        .as_u64()
        .unwrap();
    app.store
        .update_account_tier(&payer, revision, AccountStatus::Active, "plus", None)
        .await
        .unwrap();
    let switch = |allowed: bool, reference: &str| json!({"type":"set_catalog_access","user":payer,"allowed":allowed,"reference":reference});
    let live = app.store.live_access(&payer, &agent).await.unwrap();
    assert!(live.owner_catalog_models && live.owner_tier_allows_model("deepseek-flash", "admin"));
    assert_eq!(fake.ask(switch(false, "cap:1")).await["allowed"], false);
    let live = app.store.live_access(&payer, &agent).await.unwrap();
    assert!(!live.owner_catalog_models);
    assert!(!live.owner_tier_allows_model("deepseek-flash", "admin"));
    assert!(live.owner_tier_allows_model("my-own-model", &payer));
    assert_eq!(fake.ask(switch(true, "cap:2")).await["allowed"], true);
    let live = app.store.live_access(&payer, &agent).await.unwrap();
    assert!(live.owner_tier_allows_model("deepseek-flash", "admin"));
    // Never on an administrator.
    let admin =
        json!({"type":"set_catalog_access","user":"admin","allowed":false,"reference":"cap:3"});
    assert_eq!(fake.ask(admin).await["error"], "bad_request");

    // Notices: the app's own sentence for a fixed kind, once per reference.
    let notice = json!({"type":"add_notice","user":payer,"kind":"renewal_failed","severity":"warning","reference":"n1"});
    assert_eq!(fake.ask(notice.clone()).await["ok"], true);
    assert_eq!(fake.ask(notice).await["duplicate"], true);
    let unknown = json!({"type":"add_notice","user":payer,"kind":"free_text","severity":"info","reference":"n2"});
    assert_eq!(fake.ask(unknown).await["error"], "unknown_kind");
    let notices = app
        .store
        .for_user(&payer)
        .await
        .unwrap()
        .notices()
        .await
        .unwrap();
    let list = notices["notices"].as_array().unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["kind"], "billing_renewal_failed");
    assert!(list[0]["message"]
        .as_str()
        .unwrap()
        .starts_with("A payment for your plan failed"));

    // Plan summary: numbers kept, a stale one ignored, a bad one refused.
    let summary = |as_of: u64, cents: i64| json!({"type":"set_plan_summary","user":payer,"as_of":as_of,"summary":{"allowance_cents":cents,"source":"paid"}});
    assert_eq!(fake.ask(summary(10, 2000)).await["ok"], true);
    assert_eq!(fake.ask(summary(9, 1)).await["stale"], true);
    assert_eq!(fake.ask(summary(11, -5)).await["error"], "bad_request");
    let stored = app
        .store
        .for_user(&payer)
        .await
        .unwrap()
        .get(SUMMARY_KEY)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored["summary"]["allowance_cents"], 2000);

    // Usage: rows after a cursor, which also acknowledges the rows before it.
    for seq in 1..=3 {
        sqlx::query("INSERT INTO usage_turns(seq,owner_id,agent_id,requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,compute_ms,cost_micros,finished_at) VALUES(?,'o','a','r','m',1,10,5,2,1500,NULL,100)")
            .bind(seq)
            .execute(&app.store.app_pool)
            .await
            .unwrap();
    }
    let first = fake
        .ask(json!({"type":"read_usage","after":0,"limit":2}))
        .await;
    let rows = first["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["compute_seconds"], 1.5);
    assert_eq!(rows[0]["cost_micros"], Value::Null);
    let next = fake.ask(json!({"type":"read_usage","after":2})).await;
    assert_eq!(next["rows"].as_array().unwrap().len(), 1);
    let cursor: i64 =
        sqlx::query_scalar("SELECT value FROM billing_state WHERE key='usage_cursor'")
            .fetch_one(&app.store.app_pool)
            .await
            .unwrap();
    assert_eq!(cursor, 2);

    // A link request from the app: only an offered code, answered with an https link.
    let asking = tokio::spawn(request(
        json!({"type":"payment_link","user":payer,"what":"plus_subscription"}),
    ));
    let asked = fake.line().await.unwrap();
    assert_eq!(
        (asked["type"].as_str(), asked["what"].as_str()),
        (Some("payment_link"), Some("plus_subscription"))
    );
    fake.write(&json!({"v":1,"re":asked["id"],"ok":true,"url":"https://pay.example/c/1"}))
        .await;
    assert_eq!(
        asking.await.unwrap().unwrap()["url"],
        "https://pay.example/c/1"
    );
    let offers = current().unwrap().offers.clone();
    assert_eq!(
        offers,
        json!([{"what":"plus_subscription","label":"Plus, monthly"}])
    );

    // A line past the limit ends the connection; billing is then unavailable.
    let mut huge = vec![b'x'; MAX_LINE + 10];
    huge.push(b'\n');
    fake.writer.write_all(&huge).await.unwrap();
    let ended = session.await.unwrap().unwrap_err().to_string();
    assert!(ended.contains("too long"), "{ended}");
    assert!(current().is_none());
    assert!(request(json!({"type":"billing_details_link","user":payer}))
        .await
        .is_err());
}

#[tokio::test]
async fn a_run_paused_at_a_daily_limit_resumes_through_its_due_row_and_alerts_become_notices() {
    let (_root, app) = fixture().await;
    let first = crate::runtime_tests::conversation(&app).await;
    let second = crate::runtime_tests::conversation(&app).await;
    let limited = crate::runtime_tests::queued(&app, &first).await;
    let other = crate::runtime_tests::queued(&app, &second).await;
    for (id, error) in [
        (&limited, "Daily usage limit reached: your account level allows 2 model turns a day. It resets at 00:00 UTC."),
        (&other, "This run reached its limit of 50 steps"),
    ] {
        let mut run = app.run(id).await.unwrap();
        run["status"] = json!("paused");
        run["error"] = json!(error);
        app.save_override(&mut run).await.unwrap();
    }
    let pool = &app.store.app_pool;
    let user = app.store.current_user_id().to_owned();
    let midnight = webagent_engine::usage_ledger::next_reset(crate::now());
    webagent_engine::due::set(
        pool,
        &user,
        webagent_engine::due::LIMIT_RESUME,
        Some(midnight),
    )
    .await
    .unwrap();
    // Before the day turns nothing is visited.
    assert_eq!(
        crate::phase3_api::resume_limited_runs(&app, midnight - 1)
            .await
            .unwrap(),
        0
    );
    assert_eq!(app.run(&limited).await.unwrap()["status"], "paused");
    // At the next day: only the run paused by the daily limit resumes; the row is gone.
    assert_eq!(
        crate::phase3_api::resume_limited_runs(&app, midnight)
            .await
            .unwrap(),
        1
    );
    assert_ne!(app.run(&limited).await.unwrap()["status"], "paused");
    assert_eq!(app.run(&other).await.unwrap()["status"], "paused");
    assert!(
        webagent_engine::due::passed(pool, webagent_engine::due::LIMIT_RESUME, u64::MAX / 2)
            .await
            .unwrap()
            .is_empty()
    );

    // Owner alerts become one notice each for the owner, then nothing more.
    let owner = member(&app, "alerted-owner").await;
    let day = webagent_engine::usage_ledger::day(crate::now());
    for level in [80, 100] {
        webagent_engine::usage_ledger::raise_owner_alert(
            pool,
            &owner,
            webagent_engine::usage_ledger::ALERT_COMPUTE,
            day,
            level,
            900,
        )
        .await
        .unwrap();
    }
    assert_eq!(
        crate::phase3_api::deliver_owner_alerts(&app, crate::now())
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        crate::phase3_api::deliver_owner_alerts(&app, crate::now())
            .await
            .unwrap(),
        0
    );
    let notices = app
        .store
        .for_user(&owner)
        .await
        .unwrap()
        .notices()
        .await
        .unwrap();
    let texts: Vec<&str> = notices["notices"]
        .as_array()
        .unwrap()
        .iter()
        .map(|notice| notice["message"].as_str().unwrap())
        .collect();
    assert_eq!(texts.len(), 2);
    assert!(texts
        .iter()
        .any(|text| text.contains("all of your plan's 15 minutes")));
}
