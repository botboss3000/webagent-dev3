use super::*;
use base64::Engine;
use std::{collections::VecDeque, sync::Mutex};
const HEAD: &str = "1111111111111111111111111111111111111111";
const NEXT: &str = "2222222222222222222222222222222222222222";
const TREE: &str = "3333333333333333333333333333333333333333";
const BUILD: &str = "11111111-1111-4111-8111-111111111111";
struct Mock {
    replies: Mutex<VecDeque<(&'static str, &'static str, u16, Value)>>,
    requests: Mutex<Vec<ProviderRequest>>,
    serving: bool,
}
impl Mock {
    fn new(replies: Vec<(&'static str, &'static str, u16, Value)>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            requests: Mutex::new(vec![]),
            serving: false,
        }
    }
    fn done(&self) {
        assert!(
            self.replies.lock().unwrap().is_empty(),
            "Not all provider interactions occurred"
        );
    }
}
impl Transport for Mock {
    async fn request(&self, request: ProviderRequest) -> Result<Reply> {
        let reply = self.replies.lock().unwrap().pop_front();
        self.requests.lock().unwrap().push(request.clone());
        let Some((method, suffix, status, value)) = reply else {
            return Err(anyhow!("Fixture transport interrupted"));
        };
        assert_eq!(request.method, method);
        assert!(
            request.path.ends_with(suffix),
            "{} does not end with {suffix}",
            request.path
        );
        Ok(Reply { status, value })
    }
    async fn healthy(&self, url: &str) -> Result<bool> {
        assert!(preview_url(url));
        Ok(true)
    }
    async fn reachable(&self, url: &str) -> Result<bool> {
        assert!(preview_url(url));
        Ok(self.serving)
    }
}
fn config() -> Configuration {
    Configuration {
        github_owner: "fixture-owner".into(),
        github_repo: "fixture-site".into(),
        production_branch: "main".into(),
        cloudflare_account_id: "a".repeat(32),
        worker_name: "fixture-site".into(),
        build_token_uuid: BUILD.into(),
        build_command: BUILD_COMMAND.into(),
        root_directory: "/".into(),
        custom_domain: String::new(),
    }
}
async fn fixture() -> (tempfile::TempDir, App, Site) {
    let (root, app) = crate::runtime_tests::fixture_uninitialized().await;
    app.store
        .create_admin_account("admin", "fixture-only-password-hash", "fixture-generation")
        .await
        .unwrap();
    save(
        &app.store,
        "shared_default",
        "fixture",
        Save {
            expected_revision: 0,
            configuration: config(),
            revoked: false,
        },
    )
    .await
    .unwrap();
    let site = load(&app.store, "shared_default", "fixture").await.unwrap();
    seed_vault(&app.store, &site, "github", "fixture-token-only", 1).await;
    seed_vault(&app.store, &site, "cloudflare", "second-fixture-token", 1).await;
    (root, app, site)
}
async fn seed_vault(store: &Store, site: &Site, provider: &str, value: &str, revision: u64) {
    let name = vault_name(site, provider);
    let origin = if provider == "github" {
        "https://api.github.com"
    } else {
        "https://api.cloudflare.com"
    };
    let record = json!({"name":name,"origin":origin,"value":value,"revision":revision});
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
        .bind(format!("core_vault:{name}")).bind(store.seal(&record).unwrap()).execute(&store.pool).await.unwrap();
}
fn args(request: &str) -> Value {
    json!({"site_id":"fixture","connection_revision":1,"request_id":request})
}
#[test]
fn path_and_provider_boundary_validation() {
    for bad in [
        "../secret",
        ".git/config",
        "a/../b",
        "a\\b",
        "/absolute",
        "a%2fb",
        "a\nname",
        "a?x",
    ] {
        assert!(!path(bad), "{bad}");
    }
    for bad in [
        "https://workers.dev.evil.example/",
        "http://draft.workers.dev/",
        "https://user@draft.workers.dev/",
        "https://draft.workers.dev:444/",
    ] {
        assert!(!preview_url(bad), "{bad}");
    }
    assert!(preview_url("https://draft-fixture.user.workers.dev/"));
    assert!(branch("webagent/homepage"));
    assert!(!branch("main.lock"));
}
#[tokio::test]
async fn credentials_are_sealed_in_user_database_and_projection_is_secret_free() {
    let (_root, app, site) = fixture().await;
    let key = format!("core_vault:{}", vault_name(&site, "github"));
    let bytes: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_one(&app.store.pool)
        .await
        .unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("fixture-token-only"));
    let outside: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_optional(&app.store.app_pool)
        .await
        .unwrap();
    assert!(outside.is_none());
    assert!(!list(&app.store, "shared_default")
        .await
        .unwrap()
        .to_string()
        .contains("fixture-token-only"));
    let fresh = save(
        &app.store,
        "shared_default",
        "fixture",
        Save {
            expected_revision: site.revision,
            configuration: config(),
            revoked: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(fresh["revision"], 2);
    assert!(save(
        &app.store,
        "shared_default",
        "fixture",
        Save {
            expected_revision: 1,
            configuration: config(),
            revoked: false,
        }
    )
    .await
    .is_err());
}
#[tokio::test]
async fn other_user_cannot_read_owner_connection() {
    let (_root, app, _) = fixture().await;
    let account = app
        .store
        .create_user_account("source_control_other", "fixture-password-hash")
        .await
        .unwrap();
    let other = app
        .store
        .for_user(account["user_id"].as_str().unwrap())
        .await
        .unwrap();
    assert!(load(&other, "shared_default", "fixture").await.is_err());
    assert_eq!(
        list(&other, "shared_default").await.unwrap()["sites"],
        json!([])
    );
}
#[tokio::test]
async fn batches_files_into_one_commit_and_only_advances_draft() {
    let (_root, app, site) = fixture().await;
    let transport = Mock::new(vec![
        (
            "GET",
            "/git/ref/heads/webagent/home",
            200,
            json!({"object":{"sha":HEAD}}),
        ),
        (
            "GET",
            "/git/commits/1111111111111111111111111111111111111111",
            200,
            json!({"tree":{"sha":TREE}}),
        ),
        ("POST", "/git/blobs", 201, json!({"sha":TREE})),
        ("POST", "/git/trees", 201, json!({"sha":TREE})),
        ("POST", "/git/commits", 201, json!({"sha":NEXT})),
        (
            "PATCH",
            "/git/refs/heads/webagent/home",
            200,
            json!({"object":{"sha":NEXT}}),
        ),
    ]);
    let mut request = args("commit-batch");
    request["branch"] = json!("webagent/home");
    request["expected_head"] = json!(HEAD);
    request["message"] = json!("Update homepage");
    request["changes"] = json!([{"path":"public/index.html","content":"<h1>Updated</h1>"},{"path":"public/old.html","content":null}]);
    let result = run_operation(
        &app.store,
        site.clone(),
        &transport,
        "source_remote_commit",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "success");
    transport.done();
    let calls = transport.requests.lock().unwrap();
    let tree = calls
        .iter()
        .find(|r| r.path.ends_with("/git/trees"))
        .unwrap();
    assert_eq!(tree.body["tree"].as_array().unwrap().len(), 2);
    assert_eq!(calls.last().unwrap().body["force"], false);
    assert!(!calls
        .iter()
        .any(|r| r.method != "GET" && r.path.ends_with("/heads/main")));
    drop(calls);
    let replay = run_operation(
        &app.store,
        site,
        &transport,
        "source_remote_commit",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(transport.requests.lock().unwrap().len(), 6);
}
#[tokio::test]
async fn stale_branch_fails_before_external_writes() {
    let (_root, app, site) = fixture().await;
    let transport = Mock::new(vec![(
        "GET",
        "/git/ref/heads/webagent/home",
        200,
        json!({"object":{"sha":NEXT}}),
    )]);
    let mut request = args("stale");
    request["branch"] = json!("webagent/home");
    request["expected_head"] = json!(HEAD);
    request["message"] = json!("Edit");
    request["changes"] = json!([{"path":"index.html","content":"hello"}]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_remote_commit",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "failed");
    assert_eq!(transport.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn uncertain_write_is_saved_and_identical_retry_never_replays() {
    let (_root, app, site) = fixture().await;
    let transport = Mock::new(vec![
        (
            "GET",
            "/git/ref/heads/webagent/home",
            200,
            json!({"object":{"sha":HEAD}}),
        ),
        (
            "GET",
            "/git/commits/1111111111111111111111111111111111111111",
            200,
            json!({"tree":{"sha":TREE}}),
        ),
    ]);
    let mut request = args("uncertain");
    request["branch"] = json!("webagent/home");
    request["expected_head"] = json!(HEAD);
    request["message"] = json!("Edit");
    request["changes"] = json!([{"path":"index.html","content":"hello"}]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_remote_commit",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "uncertain");
    let current = load(&app.store, "shared_default", "fixture").await.unwrap();
    let replay = run_operation(
        &app.store,
        current.clone(),
        &transport,
        "source_remote_commit",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(replay["replayed"], true);
    assert_eq!(transport.requests.lock().unwrap().len(), 3);
    request["request_id"] = json!("new-id");
    assert!(run_operation(
        &app.store,
        current,
        &transport,
        "source_remote_commit",
        &request
    )
    .await
    .is_err());
}
#[tokio::test]
async fn approval_binding_changes_with_credential_and_preview_source() {
    let (_root, app, mut site) = fixture().await;
    let run = json!({"id":"test-run","session_id":"test-session","agent_id":"shared_default","requester_user_id":"admin"});
    let request = json!({"site_id":"fixture","connection_revision":1,"commit_sha":HEAD});
    let first = approval_binding(&app.store, &run, "source_publish", &request)
        .await
        .unwrap();
    site.runtime = json!({"preview":{"commit_sha":NEXT,"branch":"webagent/home"}});
    app.store
        .put(
            &site_key("shared_default", "fixture"),
            &serde_json::to_value(&site).unwrap(),
        )
        .await
        .unwrap();
    let second = approval_binding(&app.store, &run, "source_publish", &request)
        .await
        .unwrap();
    assert_ne!(first, second);
    save(
        &app.store,
        "shared_default",
        "fixture",
        Save {
            expected_revision: 1,
            configuration: config(),
            revoked: false,
        },
    )
    .await
    .unwrap();
    assert!(
        approval_binding(&app.store, &run, "source_publish", &request)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn provider_delete_success_does_not_mean_preview_is_disabled() {
    let (_root, app, mut site) = fixture().await;
    site.runtime = json!({"preview":{"build_id":BUILD,"branch":"webagent/home","commit_sha":HEAD,"status":"ready","url":"https://home-fixture.user.workers.dev/","preview_name":"home"}});
    let mut transport = Mock::new(vec![
        (
            "GET",
            "/builds/builds/11111111-1111-4111-8111-111111111111",
            200,
            json!({"status":"stopped","build_outcome":"success","build_trigger_metadata":{"commit_hash":HEAD}}),
        ),
        (
            "GET",
            "/previews/home",
            200,
            json!({"name":"home","urls":["https://home-fixture.user.workers.dev/"]}),
        ),
        ("GET", "/previews/home/deployments", 200, json!([])),
        (
            "DELETE",
            "/previews/home",
            200,
            json!({"success":true,"result":{}}),
        ),
        ("GET", "/previews/home", 404, json!({})),
    ]);
    transport.serving = true;
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_preview_stop",
        &args("stop"),
    )
    .await
    .unwrap();
    assert_eq!(result["output"]["output"]["status"], "cleanup_pending");
    transport.done();
}
#[tokio::test]
async fn changed_preview_refuses_publication_without_writes() {
    let (_root, app, mut site) = fixture().await;
    site.runtime = json!({"preview":{"build_id":BUILD,"branch":"webagent/home","commit_sha":NEXT,"connection_revision":1}});
    let transport = Mock::new(vec![(
        "GET",
        "/builds/builds/11111111-1111-4111-8111-111111111111",
        200,
        json!({"status":"stopped","build_outcome":"success","build_trigger_metadata":{"commit_hash":NEXT}}),
    )]);
    let mut request = args("publish");
    request["commit_sha"] = json!(HEAD);
    request["expected_production_sha"] = json!(HEAD);
    let result = run_operation(&app.store, site, &transport, "source_publish", &request)
        .await
        .unwrap();
    assert_eq!(result["status"], "failed");
    assert!(transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.method == "GET"));
}

const PRODUCTION_TRIGGER: &str = "22222222-2222-4222-8222-222222222222";
fn trigger_inventory() -> Value {
    let cfg = config();
    json!([
     {"trigger_uuid":BUILD,"branch_excludes":["*"],"deploy_command":PREVIEW_COMMAND,"build_command":BUILD_COMMAND,"root_directory":"/","repo_connection":{"provider_type":"github","provider_account_name":cfg.github_owner,"repo_name":cfg.github_repo}},
     {"trigger_uuid":PRODUCTION_TRIGGER,"branch_excludes":["*"],"deploy_command":DEPLOY_COMMAND,"build_command":BUILD_COMMAND,"root_directory":"/","repo_connection":{"provider_type":"github","provider_account_name":cfg.github_owner,"repo_name":cfg.github_repo}}
    ])
}
fn trigger_checks() -> Vec<(&'static str, &'static str, u16, Value)> {
    vec![
        (
            "GET",
            "/builds/workers/fixture-tag/triggers",
            200,
            trigger_inventory(),
        ),
        (
            "GET",
            "/builds/triggers/11111111-1111-4111-8111-111111111111/environment_variables",
            200,
            json!({"SKIP_DEPENDENCY_INSTALL":{"value":"true","is_secret":false}}),
        ),
        (
            "GET",
            "/builds/triggers/22222222-2222-4222-8222-222222222222/environment_variables",
            200,
            json!({"SKIP_DEPENDENCY_INSTALL":{"value":"true","is_secret":false}}),
        ),
    ]
}
fn static_source() -> Vec<(&'static str, &'static str, u16, Value)> {
    let config = json!({"name":"fixture-site","compatibility_date":"2026-10-01","assets":{"directory":"./public"},"previews":{},"workers_dev":true});
    vec![
        (
            "GET",
            "/git/trees/2222222222222222222222222222222222222222",
            200,
            json!({"tree":[{"path":"public/index.html","type":"blob","mode":"100644"},{"path":"wrangler.jsonc","type":"blob","mode":"100644"}],"truncated":false}),
        ),
        (
            "GET",
            "/contents/wrangler.jsonc",
            200,
            json!({"type":"file","size":config.to_string().len(),"encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(config.to_string())}),
        ),
    ]
}
fn configured(mut site: Site) -> Site {
    site.runtime = json!({"worker_tag":"fixture-tag","preview_trigger":BUILD,"production_trigger":PRODUCTION_TRIGGER,"provisioned":true});
    site
}
#[tokio::test]
async fn preview_build_is_pinned_and_returns_receipt_without_publishing() {
    let (_root, app, site) = fixture().await;
    let site = configured(site);
    let mut replies = vec![(
        "GET",
        "/git/ref/heads/webagent/home",
        200,
        json!({"object":{"sha":NEXT}}),
    )];
    replies.extend(static_source());
    replies.extend(trigger_checks());
    replies.push((
        "POST",
        "/builds/triggers/11111111-1111-4111-8111-111111111111/builds",
        200,
        json!({"build_uuid":BUILD}),
    ));
    let transport = Mock::new(replies);
    let mut request = args("preview");
    request["branch"] = json!("webagent/home");
    request["commit_sha"] = json!(NEXT);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_preview_start",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "success");
    assert_eq!(result["output"]["output"]["commit_sha"], NEXT);
    let calls = transport.requests.lock().unwrap();
    let build = calls.last().unwrap();
    assert_eq!(build.body["commit_hash"], NEXT);
    assert_eq!(build.body["branch"], "webagent/home");
    assert_eq!(calls.iter().filter(|r| r.method != "GET").count(), 1);
    transport.done();
}
#[tokio::test]
async fn publication_build_and_live_verification_bind_exact_commit() {
    let (_root, app, site) = fixture().await;
    let mut site = configured(site);
    site.runtime["preview"] = json!({"build_id":BUILD,"branch":"webagent/home","commit_sha":NEXT,"connection_revision":1,"preview_name":format!("wa-{NEXT}")});
    let mut replies = vec![
        (
            "GET",
            "/builds/builds/11111111-1111-4111-8111-111111111111",
            200,
            json!({"status":"stopped","build_outcome":"success","build_trigger_metadata":{"commit_hash":NEXT}}),
        ),
        (
            "GET",
            "/git/ref/heads/webagent/home",
            200,
            json!({"object":{"sha":NEXT}}),
        ),
        (
            "GET",
            "/git/ref/heads/main",
            200,
            json!({"object":{"sha":HEAD}}),
        ),
    ];
    replies.extend(trigger_checks());
    replies.push((
        "GET",
        "/workers/scripts/fixture-site/deployments",
        200,
        json!({"deployments":[{"id":BUILD,"versions":[{"version_id":BUILD,"percentage":100}]}]}),
    ));
    replies.push((
        "PATCH",
        "/git/refs/heads/main",
        200,
        json!({"object":{"sha":NEXT}}),
    ));
    replies.extend(static_source());
    replies.extend(trigger_checks());
    replies.push((
        "POST",
        "/builds/triggers/22222222-2222-4222-8222-222222222222/builds",
        200,
        json!({"build_uuid":PRODUCTION_TRIGGER}),
    ));
    let transport = Mock::new(replies);
    let mut request = args("publish-success");
    request["commit_sha"] = json!(NEXT);
    request["expected_production_sha"] = json!(HEAD);
    let result = run_operation(&app.store, site, &transport, "source_publish", &request)
        .await
        .unwrap();
    assert_eq!(result["status"], "success");
    transport.done();
    let current = load(&app.store, "shared_default", "fixture").await.unwrap();
    assert_eq!(current.runtime["production"]["commit_sha"], NEXT);
    assert_eq!(
        current.runtime["rollback_target"]["versions"][0]["version_id"],
        BUILD
    );
    let transport = Mock::new(vec![
        (
            "GET",
            "/builds/builds/11111111-1111-4111-8111-111111111111",
            200,
            json!({"status":"stopped","build_outcome":"success","build_trigger_metadata":{"commit_hash":NEXT}}),
        ),
        (
            "GET",
            "/builds/builds/22222222-2222-4222-8222-222222222222",
            200,
            json!({"status":"stopped","build_outcome":"success","build_trigger_metadata":{"commit_hash":NEXT}}),
        ),
        (
            "GET",
            "/workers/scripts/fixture-site/deployments",
            200,
            json!({"deployments":[{"id":PRODUCTION_TRIGGER,"versions":[{"version_id":PRODUCTION_TRIGGER,"percentage":100}]}]}),
        ),
        (
            "GET",
            "/workers/workers/fixture-site/versions/22222222-2222-4222-8222-222222222222",
            200,
            json!({"annotations":{"workers/tag":NEXT}}),
        ),
        (
            "GET",
            "/workers/subdomain",
            200,
            json!({"subdomain":"fixture-user"}),
        ),
    ]);
    let status = run_operation(
        &app.store,
        current,
        &transport,
        "source_site_status",
        &args("read-status"),
    )
    .await
    .unwrap();
    assert_eq!(
        status["output"]["builds"]["production"]["status"],
        "verified"
    );
    transport.done();
}
#[tokio::test]
async fn known_orphan_immutable_write_is_reconciled_without_replay() {
    let (_root, app, mut site) = fixture().await;
    let receipt = receipt_key("shared_default", "fixture", "orphan");
    app.store.put(&receipt,&json!({"status":"uncertain","steps":[{"state":"possibly_sent","provider":"Github","method":"POST","path":"/repos/fixture-owner/fixture-site/git/blobs"}]})).await.unwrap();
    site.runtime = json!({"uncertain_receipt":"orphan"});
    app.store
        .put(
            &site_key("shared_default", "fixture"),
            &serde_json::to_value(&site).unwrap(),
        )
        .await
        .unwrap();
    let transport = Mock::new(vec![]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_site_status",
        &args("status"),
    )
    .await
    .unwrap();
    assert!(result["output"]["runtime"]["uncertain_receipt"].is_null());
    assert!(transport.requests.lock().unwrap().is_empty());
    let saved = app.store.get(&receipt).await.unwrap().unwrap();
    assert_eq!(saved["status"], "uncertain");
    assert_eq!(saved["reconciliation"]["status"], "known_partial_effects");
}
#[tokio::test]
async fn stopped_preview_stays_disabled_on_status_refresh() {
    let (_root, app, mut site) = fixture().await;
    site.runtime = json!({"preview":{"build_id":BUILD,"status":"disabled"}});
    let transport = Mock::new(vec![]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_site_status",
        &args("status"),
    )
    .await
    .unwrap();
    assert_eq!(result["output"]["builds"]["preview"]["status"], "disabled");
    assert!(transport.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn new_site_provisioning_is_private_remote_and_does_not_publish_draft() {
    let (_root, app, site) = fixture().await;
    let mut replies = vec![
        ("GET", "/repos/fixture-owner/fixture-site", 404, json!({})),
        ("GET", "/workers/workers/fixture-site", 404, json!({})),
        ("GET", "/user", 200, json!({"login":"fixture-owner"})),
        ("POST", "/user/repos", 201, json!({"id":101})),
        (
            "GET",
            "/repos/fixture-owner/fixture-site",
            200,
            json!({"id":101,"default_branch":"main"}),
        ),
        (
            "GET",
            "/git/ref/heads/main",
            200,
            json!({"object":{"sha":HEAD}}),
        ),
        (
            "GET",
            "/git/ref/heads/webagent/initial-site",
            404,
            json!({}),
        ),
        (
            "GET",
            "/git/ref/heads/main",
            200,
            json!({"object":{"sha":HEAD}}),
        ),
        (
            "GET",
            "/git/commits/1111111111111111111111111111111111111111",
            200,
            json!({"tree":{"sha":TREE}}),
        ),
        ("POST", "/git/blobs", 201, json!({"sha":TREE})),
        ("POST", "/git/blobs", 201, json!({"sha":TREE})),
        ("POST", "/git/blobs", 201, json!({"sha":TREE})),
        ("POST", "/git/trees", 201, json!({"sha":TREE})),
        ("POST", "/git/commits", 201, json!({"sha":NEXT})),
        ("POST", "/git/refs", 201, json!({"object":{"sha":NEXT}})),
        (
            "PUT",
            "/workers/scripts/fixture-site",
            200,
            json!({"tag":"fixture-tag"}),
        ),
        ("GET", "/users/fixture-owner", 200, json!({"id":51})),
        (
            "PUT",
            "/builds/repos/connections",
            200,
            json!({"repo_connection_uuid":BUILD}),
        ),
        (
            "POST",
            "/builds/triggers",
            200,
            json!({"trigger_uuid":BUILD}),
        ),
        (
            "PATCH",
            "/builds/triggers/11111111-1111-4111-8111-111111111111/environment_variables",
            200,
            json!({}),
        ),
        (
            "POST",
            "/builds/triggers",
            200,
            json!({"trigger_uuid":PRODUCTION_TRIGGER}),
        ),
        (
            "PATCH",
            "/builds/triggers/22222222-2222-4222-8222-222222222222/environment_variables",
            200,
            json!({}),
        ),
    ];
    replies.extend(trigger_checks());
    let transport = Mock::new(replies);
    let mut request = args("new-site");
    request["operation"] = json!("create");
    let result = run_operation(&app.store, site, &transport, "source_site_create", &request)
        .await
        .unwrap();
    assert_eq!(result["status"], "success", "{result}");
    transport.done();
    let calls = transport.requests.lock().unwrap();
    assert_eq!(
        calls.iter().find(|r| r.path == "/user/repos").unwrap().body["private"],
        true
    );
    assert!(calls.iter().any(|r| r.multipart));
    assert!(!calls
        .iter()
        .any(|r| r.method == "PATCH" && r.path.ends_with("/heads/main")));
    assert!(!calls
        .iter()
        .any(|r| r.method == "POST" && r.path.ends_with("/builds")));
    for trigger in calls
        .iter()
        .filter(|r| r.method == "POST" && r.path.ends_with("/builds/triggers"))
    {
        assert_eq!(trigger.body["branch_excludes"], json!(["*"]));
    }
}
#[tokio::test]
async fn rollback_requires_recorded_version_and_rechecks_active_deployment() {
    let (_root, app, mut site) = fixture().await;
    site.runtime =
        json!({"rollback_target":{"id":BUILD,"versions":[{"version_id":BUILD,"percentage":100}]}});
    let transport = Mock::new(vec![
        (
            "GET",
            "/workers/scripts/fixture-site/deployments",
            200,
            json!({"deployments":[{"id":PRODUCTION_TRIGGER}]}),
        ),
        (
            "POST",
            "/workers/scripts/fixture-site/deployments",
            200,
            json!({"id":BUILD}),
        ),
    ]);
    let mut request = args("rollback");
    request["version_id"] = json!(BUILD);
    request["expected_deployment_id"] = json!(PRODUCTION_TRIGGER);
    let result = run_operation(&app.store, site, &transport, "source_rollback", &request)
        .await
        .unwrap();
    assert_eq!(result["status"], "success");
    transport.done();
    let calls = transport.requests.lock().unwrap();
    assert_eq!(calls.last().unwrap().body["versions"][0]["percentage"], 100);
    assert_eq!(
        calls.last().unwrap().body["versions"][0]["version_id"],
        BUILD
    );
}
#[tokio::test]
async fn cancellation_waits_for_terminal_build_before_deleting_preview() {
    let (_root, app, mut site) = fixture().await;
    site.runtime = json!({"preview":{"build_id":BUILD,"branch":"webagent/home","commit_sha":NEXT,"status":"running","preview_name":"home"}});
    let running = json!({"status":"running","build_trigger_metadata":{"commit_hash":NEXT}});
    let transport = Mock::new(vec![
        (
            "GET",
            "/builds/builds/11111111-1111-4111-8111-111111111111",
            200,
            running.clone(),
        ),
        (
            "PUT",
            "/builds/builds/11111111-1111-4111-8111-111111111111/cancel",
            200,
            json!({}),
        ),
        (
            "GET",
            "/builds/builds/11111111-1111-4111-8111-111111111111",
            200,
            running,
        ),
    ]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_preview_stop",
        &args("cancel-pending"),
    )
    .await
    .unwrap();
    assert_eq!(result["output"]["output"]["status"], "cleanup_pending");
    transport.done();
    assert!(transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.method != "DELETE"));
}
#[tokio::test]
async fn source_validation_rejects_executable_worker_configuration_before_build() {
    let (_root, app, site) = fixture().await;
    let mut source = static_source();
    let config = json!({"name":"fixture-site","main":"worker.js","assets":{"directory":"./public"},"previews":{}});
    source[1].3 = json!({"type":"file","size":config.to_string().len(),"encoding":"base64","content":base64::engine::general_purpose::STANDARD.encode(config.to_string())});
    let mut replies = vec![(
        "GET",
        "/git/ref/heads/webagent/home",
        200,
        json!({"object":{"sha":NEXT}}),
    )];
    replies.extend(source);
    let transport = Mock::new(replies);
    let mut request = args("reject-backend");
    request["branch"] = json!("webagent/home");
    request["commit_sha"] = json!(NEXT);
    let result = run_operation(
        &app.store,
        configured(site),
        &transport,
        "source_preview_start",
        &request,
    )
    .await
    .unwrap();
    assert_eq!(result["status"], "failed");
    assert!(transport
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|r| r.method == "GET"));
    transport.done();
}

#[tokio::test]
async fn native_owner_approval_binding_is_required_and_revoked_by_rotation() {
    let (_root, app, _site) = fixture().await;
    let sid = crate::runtime_tests::conversation(&app).await;
    let request = json!({"site_id":"fixture","connection_revision":1,"request_id":"approval-boundary","commit_sha":NEXT,"expected_production_sha":HEAD});
    let mut run = json!({"id":"source-approval-test","session_id":sid,"agent_id":"shared_default","requester_user_id":"admin","pending":{"id":"source-pending","name":"source_publish","arguments":request,"human_approved":false}});
    assert!(crate::credential_authorization::declaration("source_publish").is_some());
    let denied = execute(
        &app,
        &sid,
        &run,
        "source_publish",
        &request,
        "source-pending",
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(
        denied.to_string().contains("fresh human approval"),
        "{denied}"
    );
    let binding =
        crate::credential_authorization::resolve(&app.store, &run, "source_publish", &request)
            .await
            .unwrap()
            .unwrap();
    run["pending"]["authorization"] = binding;
    run["pending"]["human_approved"] = json!(true);
    assert!(
        crate::credential_authorization::valid(&app.store, &run, &run["pending"])
            .await
            .unwrap()
    );
    save(
        &app.store,
        "shared_default",
        "fixture",
        Save {
            expected_revision: 1,
            configuration: config(),
            revoked: false,
        },
    )
    .await
    .unwrap();
    assert!(
        !crate::credential_authorization::valid(&app.store, &run, &run["pending"])
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn settings_api_returns_conflict_for_stale_revision_and_never_returns_tokens() {
    let (_root, app, _site) = fixture().await;
    let identity = json!({"_user_id":"admin"});
    let response = settings_get(
        State(app.clone()),
        Extension(identity.clone()),
        Path("shared_default".into()),
    )
    .await
    .unwrap();
    assert!(!response.0.to_string().contains("fixture-token-only"));
    let error = settings_put(
        State(app.clone()),
        Extension(identity),
        Path(("shared_default".into(), "fixture".into())),
        Json(Save {
            expected_revision: 0,
            configuration: config(),
            revoked: false,
        }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.0, StatusCode::CONFLICT);
    let outsider = app
        .store
        .create_user_account("source_control_visitor", "fixture-password-hash")
        .await
        .unwrap();
    let denied = settings_get(
        State(app),
        Extension(json!({"_user_id":outsider["user_id"]})),
        Path("shared_default".into()),
    )
    .await
    .unwrap_err();
    assert_eq!(denied.0, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn uncertain_build_cannot_adopt_an_older_build_of_the_same_commit() {
    let (_root, app, mut site) = fixture().await;
    site = configured(site);
    site.runtime["uncertain_receipt"] = json!("build-race");
    let key = receipt_key("shared_default", "fixture", "build-race");
    app.store.put(&key,&json!({"tool":"source_preview_start","status":"uncertain","started_at_ms":2000,"steps":[{"state":"possibly_sent","provider":"Cloudflare","method":"POST","path":format!("/client/v4/accounts/{}/builds/triggers/{BUILD}/builds","a".repeat(32)),"commit_hash":NEXT,"branch":"webagent/home"}]})).await.unwrap();
    let transport = Mock::new(vec![(
        "GET",
        "/builds/workers/fixture-tag/builds",
        200,
        json!([{"build_uuid":BUILD,"created_on":"1970-01-01T00:00:01Z","trigger":{"trigger_uuid":BUILD},"build_trigger_metadata":{"commit_hash":NEXT,"branch":"webagent/home","build_trigger_source":"api"}}]),
    )]);
    let result = run_operation(
        &app.store,
        site,
        &transport,
        "source_site_status",
        &args("status"),
    )
    .await
    .unwrap();
    assert_eq!(
        result["output"]["runtime"]["uncertain_receipt"],
        "build-race"
    );
    assert!(result["output"]["runtime"]["preview"].is_null());
    transport.done();
}

#[tokio::test]
async fn vault_rotation_invalidates_publish_approval_without_connection_change() {
    let (_root, app, site) = fixture().await;
    let run = json!({"id":"run","session_id":"session","agent_id":"shared_default","requester_user_id":"admin"});
    let request = json!({"site_id":"fixture","connection_revision":1});
    let first = approval_binding(&app.store, &run, "source_publish", &request)
        .await
        .unwrap();
    seed_vault(&app.store, &site, "github", "fixture-token-only", 2).await;
    let second = approval_binding(&app.store, &run, "source_publish", &request)
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        load(&app.store, "shared_default", "fixture")
            .await
            .unwrap()
            .revision,
        1
    );
}
#[tokio::test]
async fn missing_vault_entries_return_private_form_metadata_without_values() {
    let (_root, app, site) = fixture().await;
    sqlx::query("DELETE FROM docs WHERE key LIKE 'core_vault:%'")
        .execute(&app.store.pool)
        .await
        .unwrap();
    let secret = credentials(&app.store, &site).await.unwrap();
    let public = projection(&site, &secret);
    assert_eq!(public["github_configured"], false);
    assert_eq!(public["cloudflare_configured"], false);
    assert_eq!(
        public["credential_requests"][0]["origin"],
        "https://api.github.com"
    );
    assert!(!public.to_string().contains("fixture-token-only"));
}
#[tokio::test]
async fn vault_origin_mismatch_is_rejected_before_provider_use() {
    let (_root, app, site) = fixture().await;
    let name = vault_name(&site, "github");
    sqlx::query("UPDATE docs SET value=? WHERE key=?")
        .bind(app.store.seal(&json!({"name":name,"value":uuid(),"revision":2,"origin":"https://other.example"})).unwrap())
        .bind(format!("core_vault:{name}")).execute(&app.store.pool).await.unwrap();
    assert!(credentials(&app.store, &site).await.is_err());
}

#[tokio::test]
async fn vault_revision_change_stops_an_inflight_provider_request() {
    let (_root, app, site) = fixture().await;
    let credential_digest =
        digest(&serde_json::to_value(credentials(&app.store, &site).await.unwrap()).unwrap());
    let mock = Mock::new(vec![]);
    let mut op = Operation {
        store: &app.store,
        site: site.clone(),
        transport: &mock,
        credential_digest,
        receipt: json!({"steps":[]}),
        key: "fixture-receipt".into(),
    };
    seed_vault(&app.store, &site, "github", "fixture-token-only", 2).await;
    assert!(op
        .send(ProviderRequest::gh("GET", "/user".into(), Value::Null))
        .await
        .is_err());
    assert!(mock.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn model_can_configure_public_site_metadata_and_receive_vault_requests() {
    let (_root, app, _site) = fixture().await;
    let args = json!({"site_id":"newsite","connection_revision":0,"request_id":"setup","configuration":config()});
    let output = configure(&app.store, "shared_default", &args)
        .await
        .unwrap();
    assert_eq!(output["revision"], 1);
    assert_eq!(output["github_configured"], false);
    assert_eq!(output["cloudflare_configured"], false);
    assert!(output["credential_requests"][0]["name"]
        .as_str()
        .unwrap()
        .starts_with("source_"));
    assert!(!output.to_string().contains("fixture-token-only"));
    assert!(configure(&app.store, "shared_default", &args)
        .await
        .is_err());
    let mut unsafe_args = args.clone();
    unsafe_args["site_id"] = json!("other");
    unsafe_args["configuration"]["github_token"] = json!(uuid());
    assert!(configure(&app.store, "shared_default", &unsafe_args)
        .await
        .is_err());
}

#[tokio::test]
async fn agent_configuration_cannot_reenable_a_human_revoked_connection() {
    let (_root, app, site) = fixture().await;
    save(
        &app.store,
        "shared_default",
        &site.id,
        Save {
            expected_revision: 1,
            configuration: config(),
            revoked: true,
        },
    )
    .await
    .unwrap();
    let args = json!({"site_id":site.id,"connection_revision":2,"request_id":"reenable","configuration":config()});
    assert!(configure(&app.store, "shared_default", &args)
        .await
        .is_err());
    assert_eq!(
        app.store
            .get(&site_key("shared_default", &site.id))
            .await
            .unwrap()
            .unwrap()["revoked"],
        true
    );
}

#[tokio::test]
async fn minimal_configuration_needs_no_cloudflare_ids_and_returns_creation_links() {
    let (_root, app, _site) = fixture().await;
    let args = json!({"site_id":"minimal","connection_revision":0,"request_id":"setup-minimal","configuration":{"github_owner":"fixture-owner","github_repo":"fixture-min","worker_name":"fixture-min","custom_domain":"fixture-min.example.com"}});
    let output = configure(&app.store, "shared_default", &args).await.unwrap();
    assert_eq!(output["configuration"]["production_branch"], "main");
    assert_eq!(output["configuration"]["cloudflare_account_id"], "");
    assert_eq!(output["configuration"]["build_command"], BUILD_COMMAND);
    assert_eq!(output["credential_requests"][0]["create_url"], "https://github.com/settings/personal-access-tokens/new");
    assert_eq!(output["credential_requests"][1]["create_url"], "https://dash.cloudflare.com/profile/api-tokens");
    assert_eq!(output["setup_links"][0]["url"], GITHUB_APP_INSTALL);
    let missing = json!({"site_id":"broken","connection_revision":0,"request_id":"setup-broken"});
    let error = configure(&app.store, "shared_default", &missing).await.unwrap_err().to_string();
    assert!(error.contains("github_owner, github_repo and worker_name"), "{error}");
    let mut bad = args.clone();
    bad["site_id"] = json!("bad-domain");
    bad["configuration"]["custom_domain"] = json!("Bad_Domain");
    assert!(configure(&app.store, "shared_default", &bad).await.is_err());
}
async fn discovery_operation<'a>(app: &'a App, site: &Site, mock: &'a Mock) -> Operation<'a, Mock> {
    Operation {
        store: &app.store,
        site: site.clone(),
        transport: mock,
        credential_digest: digest(&serde_json::to_value(credentials(&app.store, site).await.unwrap()).unwrap()),
        receipt: json!({"steps":[]}),
        key: "fixture-discovery-receipt".into(),
    }
}
#[tokio::test]
async fn cloudflare_ids_are_discovered_and_custom_domain_attached_once() {
    let (_root, app, mut site) = fixture().await;
    site.configuration.cloudflare_account_id.clear();
    site.configuration.build_token_uuid.clear();
    site.configuration.custom_domain = "fixture.example.com".into();
    let account = "b".repeat(32);
    let mock = Mock::new(vec![
        ("GET", "/client/v4/accounts", 200, json!({"success":true,"result":[{"id":account,"name":"Fixture"}]})),
        ("GET", "/builds/tokens", 200, json!({"success":true,"result":[{"build_token_name":"fixture","build_token_uuid":BUILD}]})),
        ("PUT", "/workers/domains", 200, json!({"success":true,"result":{"hostname":"fixture.example.com"}})),
    ]);
    let mut op = discovery_operation(&app, &site, &mock).await;
    op.resolve_cloudflare().await.unwrap();
    assert_eq!(op.account_id(), account);
    assert_eq!(op.build_token(), BUILD);
    op.attach_domain().await.unwrap();
    op.attach_domain().await.unwrap();
    mock.done();
    let calls = mock.requests.lock().unwrap();
    let domain = calls.last().unwrap();
    assert_eq!(domain.path, format!("/client/v4/accounts/{account}/workers/domains"));
    assert_eq!(domain.body, json!({"hostname":"fixture.example.com","service":"fixture-site"}));
}
#[tokio::test]
async fn several_cloudflare_accounts_require_an_explicit_choice() {
    let (_root, app, mut site) = fixture().await;
    site.configuration.cloudflare_account_id.clear();
    let mock = Mock::new(vec![(
        "GET",
        "/client/v4/accounts",
        200,
        json!({"result":[{"id":"c".repeat(32),"name":"One"},{"id":"d".repeat(32),"name":"Two"}]}),
    )]);
    let mut op = discovery_operation(&app, &site, &mock).await;
    let error = op.resolve_cloudflare().await.unwrap_err().to_string();
    assert!(error.contains("2 accounts") && error.contains("One = ") && error.contains("cloudflare_account_id"), "{error}");
    assert!(op.account_id().is_empty());
}
#[tokio::test]
async fn binary_files_commit_as_base64_blobs_and_invalid_base64_never_reaches_github() {
    let (_root, app, site) = fixture().await;
    let encoded = base64::engine::general_purpose::STANDARD.encode([0u8, 1, 2, 255]);
    let transport = Mock::new(vec![
        ("GET", "/git/ref/heads/webagent/images", 200, json!({"object":{"sha":HEAD}})),
        ("GET", "/git/commits/1111111111111111111111111111111111111111", 200, json!({"tree":{"sha":TREE}})),
        ("POST", "/git/blobs", 201, json!({"sha":TREE})),
        ("POST", "/git/trees", 201, json!({"sha":TREE})),
        ("POST", "/git/commits", 201, json!({"sha":NEXT})),
        ("PATCH", "/git/refs/heads/webagent/images", 200, json!({"object":{"sha":NEXT}})),
    ]);
    let mut request = args("commit-binary");
    request["branch"] = json!("webagent/images");
    request["expected_head"] = json!(HEAD);
    request["message"] = json!("Add photo");
    request["changes"] = json!([{"path":"public/photo.png","content_base64":encoded}]);
    let result = run_operation(&app.store, site.clone(), &transport, "source_remote_commit", &request).await.unwrap();
    assert_eq!(result["status"], "success", "{result}");
    transport.done();
    let blob = transport.requests.lock().unwrap().iter().find(|r| r.path.ends_with("/git/blobs")).unwrap().clone();
    assert_eq!(blob.body, json!({"content":encoded,"encoding":"base64"}));
    let empty = Mock::new(vec![]);
    let mut invalid = args("commit-invalid-binary");
    invalid["branch"] = json!("webagent/images");
    invalid["expected_head"] = json!(HEAD);
    invalid["message"] = json!("Broken");
    invalid["changes"] = json!([{"path":"public/x.png","content_base64":"not base64!"}]);
    let result = run_operation(&app.store, load(&app.store, "shared_default", "fixture").await.unwrap(), &empty, "source_remote_commit", &invalid).await.unwrap();
    assert_eq!(result["status"], "failed", "{result}");
    assert!(empty.requests.lock().unwrap().is_empty());
}
