//! Handler tests over a real `StoreCore` and a recording host.
#![allow(clippy::unwrap_used)]

use crate::tools::{execute, Call};
use crate::{dashboard, notices, promotion, watch, AdministrationHost, Pending, SessionAccess};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_administration::dashboard::SchedulerRequest;
use webagent_host_core::test_support::store_core;
use webagent_host_core::StoreCore;

const SID: &str = "11111111-1111-4111-8111-111111111111";
const RUN: &str = "22222222-2222-4222-8222-222222222222";

/// One socket request the handlers sent.
#[derive(Clone, Debug)]
struct Sent {
    socket: &'static str,
    name: String,
    args: Value,
    trusted: Option<Value>,
}

struct Fake {
    store: StoreCore,
    gate: tokio::sync::Mutex<()>,
    sent: Mutex<Vec<Sent>>,
    permission: Permission,
    administrator: bool,
    /// When set, authority is refused with this message.
    refuse: Option<&'static str>,
    settings: Value,
    broker_reply: fn(&str, &Value) -> Value,
    events: Mutex<Vec<(String, String)>>,
}

impl Fake {
    async fn new(root: &Path) -> Self {
        Self {
            store: store_core(root, "admin").await,
            gate: tokio::sync::Mutex::new(()),
            sent: Mutex::new(Vec::new()),
            permission: Permission::Auto,
            administrator: true,
            refuse: None,
            settings: json!({}),
            broker_reply: |_, _| json!({"status":"success","output":{"status":"success"}}),
            events: Mutex::new(Vec::new()),
        }
    }
    fn sent(&self) -> Vec<Sent> {
        self.sent.lock().unwrap().clone()
    }
    fn push(&self, socket: &'static str, name: &str, args: &Value, trusted: Option<Value>) {
        let sent = Sent {
            socket,
            name: name.into(),
            args: args.clone(),
            trusted,
        };
        self.sent.lock().unwrap().push(sent);
    }
}

impl AdministrationHost for Fake {
    fn store(&self) -> &StoreCore {
        &self.store
    }
    fn actor(&self) -> &str {
        "admin"
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }
    fn session_agent<'a>(&'a self, _sid: &'a str) -> Pending<'a, Result<String>> {
        Box::pin(async { Ok("shared_default".to_owned()) })
    }
    fn run<'a>(&'a self, run_id: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(async move { Ok(json!({"id":run_id,"session_id":SID,"status":"running"})) })
    }
    fn access<'a>(
        &'a self,
        sid: &'a str,
        tools: &'a [(&'a str, &'a str)],
    ) -> Pending<'a, Result<Option<SessionAccess>>> {
        Box::pin(async move {
            if sid != SID {
                return Ok(None);
            }
            Ok(Some(SessionAccess {
                agent: "shared_default".into(),
                active: true,
                admitted: true,
                permissions: tools.iter().map(|_| self.permission).collect(),
                settings: self.settings.clone(),
                fingerprint: format!("{:?}", self.permission),
            }))
        })
    }
    fn administrator<'a>(&'a self, _sid: &'a str) -> Pending<'a, Result<bool>> {
        Box::pin(async { Ok(self.administrator) })
    }
    fn fingerprint<'a>(
        &'a self,
        _sid: &'a str,
        _run_id: &'a str,
        name: &'a str,
    ) -> Pending<'a, Result<String>> {
        Box::pin(async move {
            match self.refuse {
                Some(message) => bail!(message),
                None => Ok(format!("authority:{name}")),
            }
        })
    }
    fn admit<'a>(
        &'a self,
        sid: &'a str,
        run_id: &'a str,
        name: &'a str,
        _args: &'a Value,
    ) -> Pending<'a, Result<String>> {
        self.fingerprint(sid, run_id, name)
    }
    fn worker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        _sid: &'a str,
        trusted: Option<Value>,
        _cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        self.push("worker", name, args, trusted);
        Box::pin(async { Ok(json!({"status":"success","output":"worker"})) })
    }
    fn broker<'a>(
        &'a self,
        name: &'a str,
        args: &'a Value,
        _sid: &'a str,
        trusted: Option<Value>,
        _cancel: CancellationToken,
    ) -> Pending<'a, Result<Value>> {
        self.push("broker", name, args, trusted);
        let reply = (self.broker_reply)(name, args);
        Box::pin(async move { Ok(reply) })
    }
    fn shutoff(&self) -> Pending<'_, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
    fn policy(&self) -> Pending<'_, Result<Value>> {
        Box::pin(async { Ok(json!({"version":3,"rules":{"source_review":"allow"}})) })
    }
    fn rule(&self, policy: &Value, name: &str) -> String {
        policy["rules"][name].as_str().unwrap_or("deny").to_owned()
    }
    fn record<'a>(
        &'a self,
        _agent: Option<&'a str>,
        docs: Vec<(String, Value)>,
        _sid: &'a str,
        kind: &'a str,
        _payload: &'a Value,
    ) -> Pending<'a, Result<()>> {
        Box::pin(async move {
            for (key, value) in &docs {
                self.store.put(key, value).await?;
                self.events
                    .lock()
                    .unwrap()
                    .push((kind.to_owned(), key.clone()));
            }
            Ok(())
        })
    }
    fn repository<'a>(&'a self, _sid: &'a str) -> Pending<'a, Result<Value>> {
        Box::pin(async { Ok(json!({"agent_id":"shared_default","revision":1,"connections":[]})) })
    }
    fn optimization_settings(&self, settings: Option<Value>) -> Result<Value> {
        Ok(settings.unwrap_or_else(|| json!({"default":true})))
    }
    fn optimization_generation(&self) -> Option<String> {
        None
    }
    fn progress(&self, run: &Value) -> Value {
        json!({"run":run["id"]})
    }
    fn abilities_version(&self) -> String {
        "abilities".into()
    }
    fn promotion_socket(&self) -> &Path {
        Path::new("/nonexistent/promote.sock")
    }
    fn stopping(&self) -> CancellationToken {
        CancellationToken::new()
    }
    fn chat_ui_config_path(&self) -> &Path {
        Path::new("/nonexistent/chat.json")
    }
    fn compaction_config_path(&self) -> &Path {
        Path::new("/nonexistent/context-compaction.json")
    }
}

async fn call(host: &Fake, name: &str, args: Value) -> Result<Value> {
    let run = json!({"id":RUN,"session_id":SID,"asset_revision":"rev-1"});
    let call = Call {
        sid: SID,
        run_id: RUN,
        run: &run,
        agent: "shared_default",
        name,
        args: &args,
        pending_id: "call-1",
        policy_version: 3,
    };
    execute(host, call, CancellationToken::new()).await
}

fn root() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

#[tokio::test]
async fn broker_calls_carry_authority_and_register_renewal_only_for_long_work() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    call(&host, "v3_build_status", json!({"task_id":"t"}))
        .await
        .unwrap();
    assert!(host
        .store
        .get(&format!("administrator_host:{SID}"))
        .await
        .unwrap()
        .is_none());
    call(
        &host,
        "build_work",
        json!({"action":"finish","task_id":"task-0001","x":null}),
    )
    .await
    .unwrap();
    let renewal = host
        .store
        .get(&format!("administrator_host:{SID}"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(renewal["tool_name"], "build_work_finish");
    assert_eq!(renewal["access_fingerprint"], "authority:build_work_finish");
    let sent = host.sent();
    let finish = sent.last().unwrap();
    assert_eq!(finish.socket, "broker");
    assert!(
        finish.args.get("x").is_none(),
        "nulls are dropped before the broker"
    );
    assert!(finish.args["request_id"]
        .as_str()
        .unwrap()
        .starts_with("broker-"));
    let trusted = finish.trusted.as_ref().unwrap();
    assert_eq!(trusted["user_id"], "admin");
    assert_eq!(trusted["access_fingerprint"], "authority:build_work_finish");
}

#[tokio::test]
async fn refused_authority_never_reaches_the_broker() {
    let dir = root();
    let mut host = Fake::new(dir.path()).await;
    host.refuse = Some("Authorization rejected");
    let error = call(&host, "v3_source_apply", json!({})).await.unwrap_err();
    assert_eq!(error.to_string(), "Authorization rejected");
    assert!(host.sent().is_empty());
}

#[tokio::test]
async fn live_files_use_the_worker_with_the_run_revision() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    call(&host, "file_read", json!({"path":"src/a.rs","limit":null}))
        .await
        .unwrap();
    let sent = host.sent();
    assert_eq!(sent[0].socket, "worker");
    assert_eq!(sent[0].args, json!({"path":"src/a.rs"}));
    assert_eq!(sent[0].trusted, Some(json!({"codebase_revision":"rev-1"})));
    call(
        &host,
        "file_read",
        json!({"path":"src/a.rs","broker_id":"new"}),
    )
    .await
    .unwrap();
    assert_eq!(host.sent()[1].socket, "broker");
}

#[tokio::test]
async fn workspace_receipts_run_once_and_replay() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let args = json!({"action":"review","request_id":"review-1"});
    let first = call(&host, "v3_workspace_action", args.clone())
        .await
        .unwrap();
    assert_eq!(first["status"], "success");
    let replay = call(&host, "v3_workspace_action", args).await.unwrap();
    assert_eq!(replay["replayed_receipt"], true);
    assert_eq!(host.sent().len(), 1, "a replay never re-runs the mutation");
    let conflict = json!({"action":"seed","request_id":"review-1"});
    assert!(call(&host, "v3_workspace_action", conflict).await.is_err());
    let receipt = call(
        &host,
        "v3_workspace_receipt",
        json!({"request_id":"review-1"}),
    )
    .await
    .unwrap();
    assert_eq!(receipt["output"]["request_id"], "review-1");
}

#[tokio::test]
async fn diagnostics_and_browser_links_stay_in_the_session() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let own = call(&host, "read_diagnostics", json!({})).await.unwrap();
    assert_eq!(own["output"]["run"], RUN);
    let bad = call(&host, "read_diagnostics", json!({"run_id":"nope"}))
        .await
        .unwrap();
    assert_eq!(bad["error_kind"], "diagnostics_unavailable");
    let link = call(&host, "dev_browser_url", json!({})).await.unwrap();
    assert!(link["output"]["url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/app?session_id={SID}")));
    let foreign = json!({"path":"/app?session_id=other"});
    assert!(call(&host, "dev_browser_url", foreign).await.is_err());
}

#[tokio::test]
async fn live_checkout_git_needs_a_current_administrator() {
    let dir = root();
    let mut host = Fake::new(dir.path()).await;
    host.administrator = false;
    let args = json!({"repo":"__root__","operation":"status"});
    assert!(call(&host, "git_tool", args.clone()).await.is_err());
    host.administrator = true;
    call(&host, "git_tool", args).await.unwrap();
    let sent = host.sent();
    assert_eq!(sent[0].name, "administrator_file_git");
    assert_eq!(sent[0].args["action"], "git_tool");
}

#[tokio::test]
async fn performance_needs_an_installed_plugin() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let error = call(&host, "performance_work", json!({"action":"status"}))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Optimization plugin unavailable");
    let smuggled = json!({"action":"status","settings":{}});
    assert!(call(&host, "performance_work", smuggled).await.is_err());
}

#[tokio::test]
async fn repository_access_reads_without_verification() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let value = call(&host, "repository_access", json!({"verify_remote":false}))
        .await
        .unwrap();
    assert_eq!(value["output"]["mutations_performed"], false);
    assert!(call(&host, "repository_access", json!({"token":"x"}))
        .await
        .is_err());
}

#[tokio::test]
async fn notices_follow_current_tool_access() {
    let dir = root();
    let mut host = Fake::new(dir.path()).await;
    host.settings = json!({"git_control":{"notify_repo_changes":true}});
    host.broker_reply = |_, _| json!({"status":"success","output":{"notices":[],"total":0}});
    host.store
        .put(
            &format!("administrator_notice:{SID}"),
            &json!({"dirty":true}),
        )
        .await
        .unwrap();
    let shown = notices::read(&host, SID).await.unwrap();
    assert_eq!(shown["notice"]["dirty"], true);
    assert_eq!(shown["build_notices_unavailable"], false);
    host.permission = Permission::Ask;
    let gated = notices::read(&host, SID).await.unwrap();
    assert!(
        gated["notice"].is_null(),
        "an approval-gated Git tool hides the notice"
    );
    host.permission = Permission::Deny;
    let review = notices::review(&host, SID, &"a".repeat(64))
        .await
        .unwrap_err();
    assert_eq!(review.0.as_u16(), 403);
    let missing = notices::read(&host, "33333333-3333-4333-8333-333333333333").await;
    assert_eq!(missing.unwrap_err().0.as_u16(), 404);
}

#[tokio::test]
async fn renewal_revokes_jobs_whose_authority_changed() {
    let dir = root();
    let mut host = Fake::new(dir.path()).await;
    let record = json!({"session_id":SID,"run_id":RUN,"tool_name":"build_work",
        "access_fingerprint":"authority:build_work"});
    host.store
        .put(&format!("administrator_host:{SID}"), &record)
        .await
        .unwrap();
    watch::renew_jobs(&host).await.unwrap();
    assert_eq!(host.sent()[0].name, "administrator_host_lease");
    host.refuse = Some("revoked");
    watch::renew_jobs(&host).await.unwrap();
    assert_eq!(host.sent()[1].name, "administrator_host_revoke");
    assert!(host
        .store
        .get(&format!("administrator_host:{SID}"))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn dashboard_requests_are_checked_before_the_broker() {
    let dir = root();
    let mut host = Fake::new(dir.path()).await;
    assert_eq!(
        dashboard::result(&host, "x").await.unwrap_err().0.as_u16(),
        400
    );
    let bad: SchedulerRequest =
        serde_json::from_value(json!({"action":"priority","priority":3})).unwrap();
    assert_eq!(
        dashboard::scheduler(&host, &bad)
            .await
            .unwrap_err()
            .0
            .as_u16(),
        400
    );
    assert!(host.sent().is_empty());
    host.broker_reply = |_, _| json!({"status":"success","output":{"job":null}});
    let job = "44444444-4444-4444-8444-444444444444";
    assert_eq!(
        dashboard::result(&host, job).await.unwrap_err().0.as_u16(),
        404
    );
    host.broker_reply = |_, _| json!({"status":"success","output":{"jobs":[]}});
    assert_eq!(dashboard::list(&host).await.unwrap(), json!({"jobs":[]}));
    let sent = host.sent();
    assert_eq!(
        sent.last().unwrap().trusted.as_ref().unwrap()["session_id"],
        "00000000-0000-0000-0000-000000000000"
    );
}

#[tokio::test]
async fn promotions_refuse_unknown_sessions_and_recover_dispatching_records() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let body = json!({"client_request_id":"55555555-5555-4555-8555-555555555555",
        "expected_review_sha256":"a".repeat(64)});
    assert_eq!(
        promotion::promote(&host, SID, &body)
            .await
            .unwrap_err()
            .0
            .as_u16(),
        404
    );
    let missing = json!({"expected_review_sha256":"a".repeat(64)});
    assert_eq!(
        promotion::promote(&host, SID, &missing)
            .await
            .unwrap_err()
            .0
            .as_u16(),
        400
    );
    let record = json!({"session_id":SID,"client_request_id":"r1","state":"dispatching"});
    host.store
        .put(&format!("promotion:{SID}:r1"), &record)
        .await
        .unwrap();
    promotion::recover(&host).await.unwrap();
    let recovered = host
        .store
        .get(&format!("promotion:{SID}:r1"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recovered["state"], "uncertain");
    let events = host.events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![(
            "promotion_recovered".to_owned(),
            format!("promotion:{SID}:r1")
        )]
    );
}

#[tokio::test]
async fn code_index_maintenance_routes_snapshots_to_worker_and_live_work_to_broker() {
    let dir = root();
    let host = Fake::new(dir.path()).await;
    let read = json!({"action":"get","root":"published","entry_id":"e","task_id":"t"});
    crate::code_index::maintenance(&host, SID, RUN, &read, CancellationToken::new())
        .await
        .unwrap();
    let sync = json!({"action":"sync","task_id":"t"});
    crate::code_index::maintenance(&host, SID, RUN, &sync, CancellationToken::new())
        .await
        .unwrap();
    let sent = host.sent();
    assert_eq!(sent[0].socket, "worker");
    assert_eq!(
        sent[0].args,
        json!({"action":"get","root":"published","entry_id":"e"})
    );
    assert!(sent[0].trusted.is_none());
    assert_eq!(sent[1].socket, "broker");
    assert_eq!(sent[1].name, "code_index_maintenance");
    assert_eq!(
        sent[1].args,
        json!({"action":"sync","root":"live","task_id":"t"})
    );
    let trusted = sent[1].trusted.as_ref().unwrap();
    assert_eq!(trusted["session_id"], SID);
    assert_eq!(
        trusted["access_fingerprint"],
        "authority:code_index_maintenance"
    );
    assert!(crate::code_index::maintenance(
        &host,
        SID,
        RUN,
        &json!({"action":"record","root":"published"}),
        CancellationToken::new()
    )
    .await
    .is_err());
}
