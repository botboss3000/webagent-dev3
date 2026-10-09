use super::*;
fn enabled(sid: &str) -> Settings {
    let mut config = Config::disabled();
    config.enabled = true;
    config.full_context.enabled = true;
    config.full_context.sessions.push(config::Session {
        session_id: sid.into(),
        expires_at: "2099-01-01T00:00:00Z".into(),
    });
    Settings {
        config,
        digest: Some("fixture".into()),
        error: None,
    }
}
#[tokio::test]
async fn ownership_and_configuration_gate_capture() {
    let root = tempfile::tempdir().unwrap();
    let admin = Store::open(root.path()).await.unwrap();
    let account = admin
        .create_user_account("other-admin", "fixture-password-hash")
        .await
        .unwrap();
    let other = admin
        .for_user(account["user_id"].as_str().unwrap())
        .await
        .unwrap();
    assert!(scope(&admin));
    assert!(!scope(&other));
    let sid = "3c7e770c-82a2-484d-9e5a-8ad035f2ef4f";
    let mut s = enabled(sid);
    let event = json!({"owner":"admin","category":"context_assembly","config_digest":"fixture","session_id":sid});
    assert!(snapshot_allowed(&s, &event));
    let mut public = event.clone();
    public["owner"] = json!("public");
    assert!(!snapshot_allowed(&s, &public));
    s.config.enabled = false;
    assert!(!event_allowed(&s, &event));
    s = enabled(sid);
    s.digest = Some("changed".into());
    assert!(!snapshot_allowed(&s, &event));
    s = enabled(sid);
    s.config.full_context.sessions[0].expires_at = "1970-01-01T00:00:00Z".into();
    assert!(!snapshot_allowed(&s, &event));
    s.config.categories.context_assembly = false;
    assert!(!event_allowed(&s, &event));
}
#[test]
fn atomic_reload_and_invalid_config_disable_capture() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config/diagnostics.json");
    seed(&path).unwrap();
    let original = Settings::read(&path);
    assert!(!original.config.active());
    let config = enabled("3c7e770c-82a2-484d-9e5a-8ad035f2ef4f").config;
    let next = path.with_extension("next");
    std::fs::write(&next, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::rename(next, &path).unwrap();
    let loaded = Settings::read(&path);
    assert!(loaded.config.active());
    assert_ne!(original.digest, loaded.digest);
    std::fs::write(&path, b"{bad}").unwrap();
    let loaded = Settings::read(&path);
    assert!(!loaded.config.active());
    assert!(loaded.error.is_some());
    seed(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{bad}");
}
#[tokio::test]
async fn snapshot_encryption_identity_expiry_and_retention() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).await.unwrap();
    let pool = open(root.path()).await.unwrap();
    std::fs::create_dir_all(root.path().join("config")).unwrap();
    std::fs::write(
        root.path().join("config/app-settings.json"),
        br#"{"admin_database_encryption":{"enabled":false}}"#,
    )
    .unwrap();
    let config = Config::disabled();
    let payload =
        json!({"owner":"admin","attempt_id":"a","session_id":"s","run_id":"r","history":"PRIVATE"});
    let sealed = store.seal(&payload).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("PRIVATE"));
    insert_snapshot(&pool, &payload, &sealed, 100, &config)
        .await
        .unwrap();
    assert_eq!(
        read_snapshot(&pool, &store, "s", "r", "a").await.unwrap(),
        payload
    );
    assert!(read_snapshot(&pool, &store, "wrong", "r", "a")
        .await
        .is_err());
    let wrong = store
        .seal(&json!({"owner":"other","attempt_id":"a","session_id":"s","run_id":"r"}))
        .unwrap();
    sqlx::query("UPDATE context_snapshots SET sealed=?")
        .bind(wrong)
        .execute(&pool)
        .await
        .unwrap();
    assert!(read_snapshot(&pool, &store, "s", "r", "a").await.is_err());
    sqlx::query("UPDATE context_snapshots SET sealed=?,expires_at=0")
        .bind(&sealed)
        .execute(&pool)
        .await
        .unwrap();
    assert!(read_snapshot(&pool, &store, "s", "r", "a").await.is_err());
    prune(&pool, &config).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM context_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    for i in 0..3 {
        let p = json!({"owner":"admin","attempt_id":format!("a{i}"),"session_id":"s","run_id":"r"});
        insert_snapshot(&pool, &p, &store.seal(&p).unwrap(), 100, &config)
            .await
            .unwrap();
    }
    let mut small = config.clone();
    small.full_context.max_retained_bytes = 150;
    prune(&pool, &small).await.unwrap();
    let bytes: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(bytes),0) FROM context_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(bytes <= 150);
    persist_fixture(&pool, &json!({"owner":"admin","category":"provider"}))
        .await
        .unwrap();
    sqlx::query("UPDATE diagnostics SET ts=0")
        .execute(&pool)
        .await
        .unwrap();
    prune(&pool, &config).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM diagnostics")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 0);
    pool.close().await;
    let pool = open(root.path()).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM context_snapshots")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(count, 1);
}
#[test]
fn assembly_evidence_is_content_free_and_snapshot_is_exact() {
    let history = vec![
        json!({"role":"user","content":"PRIVATE"}),
        json!({"type":"function_call","call_id":"c","name":"file_read"}),
        json!({"type":"function_call_output","call_id":"c","output":"PRIVATE"}),
    ];
    let selection = crate::provider::ProviderSelection::parse(
        &json!({"provider":"deepseek","model":"deepseek-chat"}),
        "unused",
    )
    .unwrap();
    let compaction = crate::context_compaction::Config::configured().unwrap();
    let contributions = [];
    let schemas = [json!({"name":"file_read","description":"PRIVATE"})];
    let request = Request {
        history: &history,
        base: "PRIVATE",
        guidance: " PRIVATE",
        schemas: &schemas,
        contributions: &contributions,
        identity: crate::context_input::CallIdentity { turn: 1, call: 2 },
        selection: &selection,
        max_bytes: 119808,
        compaction: &compaction,
    };
    let measured = measure(&request);
    assert!(!measured.to_string().contains("PRIVATE"));
    assert_eq!(measured["compaction"]["pending_calls"], 0);
    let digest = Some("fixture".into());
    let report = json!({"outcome":"ready"});
    let sid = json!("s");
    let rid = json!("r");
    let capture = Capture {
        owner: "admin",
        category: "context_assembly",
        config_digest: &digest,
        attempt_id: "a",
        session_id: &sid,
        run_id: &rid,
        provider_selection: selection.value(),
        report: &report,
        assembled_instructions: assembled_instructions(&request),
        history: request.history,
        base: request.base,
        guidance: request.guidance,
        schemas: request.schemas,
        contributions: &contributions,
        compaction: &compaction,
        max_bytes: request.max_bytes,
        turn: 1,
        call: 2,
    };
    let bytes = crate::context_accounting::json_size(&capture).unwrap();
    let value = serde_json::to_value(&capture).unwrap();
    assert_eq!(bytes, serde_json::to_vec(&value).unwrap().len());
    assert_eq!(value["history"], json!(history));
    assert_eq!(value["base"], "PRIVATE");
    assert_eq!(value["schemas"], json!(schemas));
    let invalid = anyhow::anyhow!("PRIVATE secret token");
    assert_eq!(
        crate::context_assembly::failure_code(&invalid),
        ("assembly_internal", "assembly")
    );
}
#[tokio::test]
async fn queue_capacity_and_byte_budget_are_bounded() {
    let (tx, mut rx) = mpsc::channel::<usize>(8);
    for i in 0..8 {
        tx.try_send(i).unwrap();
    }
    assert!(tx.try_send(9).is_err());
    assert_eq!(rx.recv().await, Some(0));
    let budget = Arc::new(Semaphore::new(1024));
    let permit = budget.clone().try_acquire_many_owned(1024).unwrap();
    assert!(budget.clone().try_acquire_many_owned(1).is_err());
    drop(permit);
    assert_eq!(budget.available_permits(), 1024);
}
#[tokio::test]
async fn read_routes_reject_public_and_other_admin_accounts() {
    use axum::{
        body::Body,
        http::{Request as HttpRequest, StatusCode},
    };
    use tower::ServiceExt;
    let (_root, app) = crate::runtime_tests::fixture().await;
    let member = app
        .store
        .create_user_account("diagnostics-other-admin", "fixture-password-hash")
        .await
        .unwrap();
    app.store
        .update_account_access(
            member["user_id"].as_str().unwrap(),
            1,
            crate::access::AccountStatus::Active,
            crate::access::Role::Administrator,
            "basic",
        )
        .await
        .unwrap();
    let member = app
        .store
        .account_by_id(member["user_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    for (user, generation, expected) in [
        (
            "admin".to_owned(),
            "fixture-generation".to_owned(),
            StatusCode::OK,
        ),
        (
            member["user_id"].as_str().unwrap().to_owned(),
            member["generation"].as_str().unwrap().to_owned(),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let token = crate::uuid();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),&json!({"user_id":user,"expires":crate::now()+3600,"generation":generation,"csrf":"fixture"})).await.unwrap();
        for route in [
            "/api/v3/admin/diagnostics",
            "/api/v3/admin/diagnostics/status",
        ] {
            let req = HttpRequest::builder()
                .uri(route)
                .header("cookie", format!("v3_session={token}"))
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                crate::api::router(app.clone())
                    .oneshot(req)
                    .await
                    .unwrap()
                    .status(),
                expected
            );
        }
    }
    let req = HttpRequest::builder()
        .uri("/api/v3/admin/diagnostics")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        crate::api::router(app).oneshot(req).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );
}
#[tokio::test]
#[ignore]
async fn capture_accounting_measurement() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path()).await.unwrap();
    let account = store
        .create_user_account("public-fixture", "fixture-password-hash")
        .await
        .unwrap();
    let public = store
        .for_user(account["user_id"].as_str().unwrap())
        .await
        .unwrap();
    let pool = open(root.path()).await.unwrap();
    let history = (0..512)
        .map(|_| json!({"role":"user","content":"x".repeat(1024)}))
        .collect::<Vec<_>>();
    let selection = crate::provider::ProviderSelection::parse(
        &json!({"provider":"deepseek","model":"deepseek-chat"}),
        "unused",
    )
    .unwrap();
    let compaction = crate::context_compaction::Config::configured().unwrap();
    let request = Request {
        history: &history,
        base: "base",
        guidance: "",
        schemas: &[],
        contributions: &[],
        identity: crate::context_input::CallIdentity { turn: 1, call: 1 },
        selection: &selection,
        max_bytes: 1000000,
        compaction: &compaction,
    };
    let run = json!({"id":"r","session_id":"s"});
    let prepared = crate::context_assembly::prepare(request);
    let start = std::time::Instant::now();
    for _ in 0..50 {
        assert!(assembly(&public, &run, &request, &prepared).is_none());
    }
    let public_time = start.elapsed();
    let start = std::time::Instant::now();
    for _ in 0..50 {
        assert!(assembly(&store, &run, &request, &prepared).is_none());
    }
    let disabled = start.elapsed();
    let start = std::time::Instant::now();
    for _ in 0..50 {
        let report = measure(&request);
        persist_fixture(
            &pool,
            &json!({"owner":"admin","category":"context_assembly","report":report}),
        )
        .await
        .unwrap();
    }
    let metadata = start.elapsed();
    let start = std::time::Instant::now();
    let mut bytes = 0;
    for i in 0..50 {
        let report = measure(&request);
        let digest = Some("fixture".into());
        let sid = json!("s");
        let rid = json!("r");
        let attempt = format!("a{i}");
        let capture = Capture {
            owner: "admin",
            category: "context_assembly",
            config_digest: &digest,
            attempt_id: &attempt,
            session_id: &sid,
            run_id: &rid,
            provider_selection: selection.value(),
            report: &report,
            assembled_instructions: assembled_instructions(&request),
            history: &history,
            base: "base",
            guidance: "",
            schemas: &[],
            contributions: &[],
            compaction: &compaction,
            max_bytes: 1000000,
            turn: 1,
            call: 1,
        };
        let payload = serde_json::to_value(capture).unwrap();
        let size = crate::context_accounting::json_size(&payload).unwrap();
        bytes += size;
        insert_snapshot(
            &pool,
            &payload,
            &store.seal(&payload).unwrap(),
            size,
            &Config::disabled(),
        )
        .await
        .unwrap();
    }
    println!("50 fixture captures: public={public_time:?} disabled={disabled:?} metadata={metadata:?} full={:?} retained_payload_bytes={bytes}",start.elapsed());
}

#[test]
fn pause_classification_and_public_projection_do_not_leak_content() {
    let selection = crate::provider::ProviderSelection::parse(
        &json!({"provider":"deepseek","model":"deepseek-chat"}),
        "unused",
    )
    .unwrap();
    let mut compaction = crate::context_compaction::Config::configured().unwrap();
    compaction.summary_reserve_bytes = 100;
    let history = vec![
        json!({"role":"user","content":"x".repeat(1000)}),
        json!({"type":"function_call","call_id":"pending","name":"read","arguments":"PRIVATE"}),
    ];
    let request = Request {
        history: &history,
        base: "base",
        guidance: "",
        schemas: &[],
        contributions: &[],
        identity: crate::context_input::CallIdentity { turn: 1, call: 1 },
        selection: &selection,
        max_bytes: 600,
        compaction: &compaction,
    };
    let error = crate::context_assembly::prepare(request).err().unwrap();
    assert_eq!(
        crate::context_assembly::failure_code(&error),
        ("no_safe_boundary", "compaction_plan")
    );
    assert_eq!(measure(&request)["compaction"]["pending_calls"], 1);
    let error = crate::context_assembly::prepare(Request {
        max_bytes: 8,
        ..request
    })
    .err()
    .unwrap();
    assert_eq!(
        crate::context_assembly::failure_code(&error),
        ("overhead_exceeds_budget", "budget")
    );
    let error = crate::context_assembly::prepare(Request {
        max_bytes: 100,
        ..request
    })
    .err()
    .unwrap();
    assert_eq!(
        crate::context_assembly::failure_code(&error),
        ("summary_reserve_unavailable", "budget")
    );
    let long = "x".repeat(100000);
    let error = crate::context_assembly::prepare(Request {
        base: &long,
        max_bytes: 1000000,
        ..request
    })
    .err()
    .unwrap();
    assert_eq!(
        crate::context_assembly::failure_code(&error),
        ("instruction_limit", "instructions")
    );
    let event = json!({"attempt_id":crate::uuid(),"report":{"outcome":"failed","failure_code":"PRIVATE","stage":"PRIVATE","private_overlay":"PRIVATE","history_json_bytes":123,"compaction":{"pending_calls":1,"private":"PRIVATE"}}});
    let safe = crate::public_assembly_attempt(&event);
    assert!(!safe.to_string().contains("PRIVATE"));
    assert_eq!(safe["report"]["history_json_bytes"], 123);
}

// Run separately: the production recorder is process-global by design.
#[tokio::test]
#[ignore]
async fn recorder_end_to_end_live_reload_and_private_capture() {
    use axum::{body::Body, http::Request as HttpRequest};
    use tower::ServiceExt;
    let (root, app) = crate::runtime_tests::fixture().await;
    let sid = crate::runtime_tests::conversation(&app).await;
    let rid = crate::runtime_tests::queued(&app, &sid).await;
    let packages = root.path().join("plugins");
    let package = packages.join("diagnostics");
    std::env::set_var("V3_PLUGIN_TEST_ROOT", &packages);
    init(root.path(), &app.store, "api").await;
    tokio::time::sleep(Duration::from_millis(2200)).await;
    assert!(
        !package.exists(),
        "Absent package must not be seeded at startup"
    );
    assert!(!root.path().join("config/diagnostics.json").exists());
    std::fs::create_dir_all(&package).unwrap();
    let binary = std::env::var("V3_DIAGNOSTICS_TEST_BINARY")
        .expect("Standalone diagnostics acceptance needs V3_DIAGNOSTICS_TEST_BINARY");
    std::fs::copy(&binary, package.join("webagent-diagnostics")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(
        package.join("webagent-diagnostics"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    std::fs::write(
        package.join("plugin.json"),
        include_bytes!("../plugin_packages/diagnostics/plugin.json"),
    )
    .unwrap();
    std::fs::write(
        package.join("diagnostics.json"),
        serde_json::to_vec(&Config::disabled()).unwrap(),
    )
    .unwrap();
    std::env::set_var("V3_PLUGIN_TEST_ROOT", &packages);
    init(root.path(), &app.store, "api").await;
    tokio::time::timeout(Duration::from_secs(7), async {
        while !bridge::available() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(!settings().unwrap().config.active());
    let path = package.join("diagnostics.json");
    let config = enabled(&sid).config;
    let next = path.with_extension("next");
    std::fs::write(&next, serde_json::to_vec(&config).unwrap()).unwrap();
    std::fs::rename(next, &path).unwrap();
    tokio::time::timeout(Duration::from_secs(7), async {
        while !settings().unwrap().config.active() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let (loop_config, loop_digest) = bridge::configuration().await.unwrap();
    assert!(loop_config.active());
    assert_eq!(loop_digest, settings().unwrap().digest.unwrap());
    let history = vec![
        json!({"role":"user","content":"PRIVATE context"}),
        json!({"type":"function_call","call_id":"pending","name":"read","arguments":"PRIVATE"}),
    ];
    let selection = crate::provider::ProviderSelection::parse(
        &json!({"provider":"deepseek","model":"deepseek-chat"}),
        "unused",
    )
    .unwrap();
    let mut compaction = crate::context_compaction::Config::configured().unwrap();
    compaction.summary_reserve_bytes = 100;
    let request = Request {
        history: &history,
        base: "base",
        guidance: "",
        schemas: &[],
        contributions: &[],
        identity: crate::context_input::CallIdentity { turn: 1, call: 1 },
        selection: &selection,
        max_bytes: 150,
        compaction: &compaction,
    };
    let mut run = app.run(&rid).await.unwrap();
    let prepared = crate::context_assembly::prepare(request);
    assert!(prepared.is_err());
    let evidence = assembly(&app.store, &run, &request, &prepared).unwrap();
    let attempt = evidence["attempt_id"].as_str().unwrap().to_owned();
    run["latest_context_assembly"] = evidence;
    app.save(&mut run).await.unwrap();
    let r = RECORDER.get().unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let rows: Vec<String> =
                sqlx::query_scalar("SELECT payload FROM diagnostics WHERE category='run'")
                    .fetch_all(&r.pool)
                    .await
                    .unwrap();
            if !rows.is_empty() {
                let event: Value = serde_json::from_str(rows.last().unwrap()).unwrap();
                assert_eq!(event["run_id"], rid);
                assert_eq!(event["run_revision"], run["run_revision"]);
                assert!(!event.to_string().contains("PRIVATE"));
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let mut stale = run.clone();
    stale["run_revision"] = json!(999999);
    assert!(app.save(&mut stale).await.is_err());
    let committed_rows: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM diagnostics WHERE category='run'")
            .fetch_one(&r.pool)
            .await
            .unwrap();
    assert_eq!(committed_rows, 1);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM context_snapshots")
                .fetch_one(&r.pool)
                .await
                .unwrap();
            if n == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let public_account = app
        .store
        .create_user_account("uncaptured-public", "fixture-password-hash")
        .await
        .unwrap();
    let public = app
        .store
        .for_user(public_account["user_id"].as_str().unwrap())
        .await
        .unwrap();
    assert!(assembly(&public, &run, &request, &prepared).is_none());
    assert!(!provider_enabled(&public));
    committed(&public, &run, "tool_result", false);
    committed(&public, &run, "run_status", true);
    let token = crate::uuid();
    app.store.put(&format!("auth:{}",crate::digest(&json!(token))),&json!({"user_id":"admin","expires":crate::now()+3600,"generation":"fixture-generation","csrf":"fixture"})).await.unwrap();
    let uri = format!("/api/v3/sessions/{sid}/runs/{rid}/diagnostics/context/{attempt}");
    let response = crate::api::router(app.clone())
        .oneshot(
            HttpRequest::builder()
                .uri(&uri)
                .header("cookie", format!("v3_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body = axum::body::to_bytes(response.into_body(), 16777216)
        .await
        .unwrap();
    let capture: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(capture["history"], json!(history));
    let response = crate::api::router(app.clone())
        .oneshot(
            HttpRequest::builder()
                .uri(format!("/api/v3/sessions/{sid}/runs/{rid}/diagnostics"))
                .header("cookie", format!("v3_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&body).contains("PRIVATE"));
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT sealed FROM context_snapshots")
        .fetch_one(&r.pool)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("PRIVATE"));
    let moved = packages.join("removed-diagnostics");
    std::fs::rename(&package, &moved).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while bridge::available() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(assembly(&app.store, &run, &request, &prepared).is_none());
    assert!(
        !package.exists(),
        "Host must not recreate a removed package"
    );
    let missing = context(&app.store, &sid, &rid, &attempt).await.unwrap_err();
    assert_eq!(missing.0, axum::http::StatusCode::NOT_FOUND);
    let response = crate::api::router(app.clone())
        .oneshot(
            HttpRequest::builder()
                .uri("/api/v3/admin/diagnostics")
                .header("cookie", format!("v3_session={token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), 65536)
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&body).unwrap()["records"],
        json!([])
    );
    assert!(moved.join("logs.db").is_file());
    assert!(moved.join("diagnostics.json").is_file());
    assert!(!root.path().join("config/diagnostics.json").exists());
    std::fs::rename(&moved, &package).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while !bridge::available() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let pid_file = root.path().join("diagnostics/plugin.pid");
    let before = std::fs::read_to_string(&pid_file).unwrap();
    let update = package.join("webagent-diagnostics.next");
    std::fs::copy(&binary, &update).unwrap();
    std::fs::set_permissions(&update, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(update, package.join("webagent-diagnostics")).unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while std::fs::read_to_string(&pid_file).ok().as_deref() == Some(before.as_str())
            || !bridge::available()
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        read_snapshot(&r.pool, &app.store, &sid, &rid, &attempt)
            .await
            .unwrap()["history"],
        json!(history)
    );
    let writer = open(&packages).await.unwrap();
    sqlx::query("DROP TABLE context_snapshots")
        .execute(&writer)
        .await
        .unwrap();
    let _ = assembly(&app.store, &run, &request, &prepared);
    tokio::time::timeout(Duration::from_secs(3), async {
        while r.write_errors.load(Ordering::Relaxed) == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    std::fs::write(&path, serde_json::to_vec(&Config::disabled()).unwrap()).unwrap();
    tokio::time::timeout(Duration::from_secs(7), async {
        while settings().unwrap().config.active() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    assert!(assembly(&app.store, &run, &request, &prepared).is_none());
    println!("Verified live reload, private encrypted snapshot API, public-user exclusion, redaction, storage failure isolation, and disable.");
}

#[test]
fn committed_transition_projection_excludes_private_text_and_honors_categories() {
    let mut s = enabled("3c7e770c-82a2-484d-9e5a-8ad035f2ef4f");
    let run = json!({"id":"run","session_id":"session","status":"paused","phase":"model","run_revision":8,
        "error":"PRIVATE_ERROR","input":[{"text":"PRIVATE_INPUT"}],"pending":{"arguments":"PRIVATE_ARGUMENT","state":"executing","call_id":"call"}});
    let event = transition(&s, &run, "run_status", false).unwrap();
    assert_eq!(event["category"], "run");
    assert_eq!(event["run_revision"], 8);
    assert_eq!(event["level"], "warning");
    assert!(!event.to_string().contains("PRIVATE"));
    assert_eq!(
        transition(&s, &run, "tool_result", false).unwrap()["category"],
        "tool"
    );
    assert_eq!(
        transition(&s, &run, "run_status", true).unwrap()["category"],
        "recovery"
    );
    assert!(transition(&s, &run, "working_message", false).is_none());
    s.config.categories.tool = false;
    assert!(transition(&s, &run, "tool_result", false).is_none());
    s.config.categories.recovery = false;
    assert!(transition(&s, &run, "run_status", true).is_none());
    s.config.enabled = false;
    assert!(transition(&s, &run, "run_status", false).is_none());
}
#[test]
fn existing_configuration_gains_compatible_category_defaults() {
    let mut value = serde_json::to_value(Config::disabled()).unwrap();
    for k in ["run", "tool", "recovery"] {
        value["categories"].as_object_mut().unwrap().remove(k);
    }
    let c: Config = serde_json::from_value(value).unwrap();
    assert!(c.categories.run && c.categories.tool && c.categories.recovery);
    assert!(!c.enabled);
}

// Seed archive fixtures directly; production writes require the standalone protocol.
async fn persist_fixture(pool: &SqlitePool, event: &Value) -> anyhow::Result<()> {
    sqlx::query("INSERT INTO diagnostics(ts,category,run_id,session_id,payload) VALUES(?,?,?,?,?)")
        .bind(crate::now() as i64)
        .bind(event["category"].as_str())
        .bind(event["run_id"].as_str())
        .bind(event["session_id"].as_str())
        .bind(event.to_string())
        .execute(pool)
        .await?;
    Ok(())
}
