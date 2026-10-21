//! Native duplicate-action authority; model certainty cannot establish new intent.
use crate::*;
use anyhow::bail;
use serde::{Deserialize, Serialize};

pub(crate) fn supported(name: &str) -> bool {
    matches!(
        name,
        "prompt_slot_write"
            | "edit_own_prompt"
            | "save_own_skill"
            | "remove_own_skill"
            | "test_owner_purchase"
    )
}
pub(crate) fn default_on(name: &str) -> bool {
    supported(name)
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Protection {
    Off,
    On,
}
impl Default for Protection {
    fn default() -> Self {
        Self::On
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    id: String,
    #[serde(default)]
    separate_from: Option<String>,
}
fn field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid action record field: {key}"))
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}
pub(crate) fn strip(args: &Value) -> Value {
    let mut result = args.clone();
    if let Some(object) = result.as_object_mut() {
        object.remove("_action");
    }
    result
}
pub(crate) fn summary(action: &Value) -> Value {
    json!({"id":action["id"],"tool":action["tool"],"state":action["state"],"attempt":action["attempt"],
        "revision":action["revision"],"requires_new_intent_approval":action["requires_new_intent_approval"],
        "separate_from":action["separate_from"],"retry_safe":action["retry_safe"]})
}
pub(crate) fn response(action: &Value) -> Value {
    if action["state"] == "completed" {
        let mut result = action["result"].clone();
        if !result.is_object() {
            result = json!({"status":"success","output":result});
        }
        result["action"] = summary(action);
        result["receipt"] = action["receipt"].clone();
        result["replayed_action"] = json!(true);
        return result;
    }
    json!({"status":"action_status","action":summary(action),"output":
        if action["state"]=="uncertain" {"Outcome is uncertain. Reconcile the existing action; do not retry with a new ID."}
        else {"Existing action found. Reuse its ID for status/retry; a matching additional action requires fresh user intent and specific authorization."}})
}
// Semantic effect identity deliberately excludes precondition revisions. Different
// content remains a separate action; merely targeting the same tool is not a match.
fn identity(name: &str, args: &Value) -> Value {
    match name {
        "test_owner_purchase" => {
            json!({"family":"purchase","merchant":args["merchant"],"item":args["item"],"recipient":args["recipient"]})
        }
        "edit_own_prompt" => json!({"family":"prompt","slot_name":args["slot_name"],"action":"put",
            "content":args["content"],"merge_mode":args["mode"].as_str().unwrap_or("replace")}),
        "prompt_slot_write" => {
            json!({"family":"prompt","slot_name":args["slot_name"],"action":args["action"],
            "content":args["content"],"merge_mode":args["merge_mode"].as_str().unwrap_or("replace"),
            "order_index":args["order_index"],"lock":args["lock"]})
        }
        "save_own_skill" => {
            json!({"family":name,"name":args["name"],"description":args["description"],
            "instructions":args["instructions"],"mode":args["mode"].as_str().unwrap_or("selectable")})
        }
        "remove_own_skill" => json!({"family":name,"name":args["name"]}),
        _ => json!({"family":name,"effect":args}),
    }
}
fn resource(agent: &str, name: &str) -> String {
    if name == "test_owner_purchase" {
        "provider:test".into()
    } else {
        format!("agent:{agent}")
    }
}
async fn ledger(app: &App, run: &Value) -> Result<Store> { ledger_on(&app.store,run).await }
pub(crate) async fn prepare(
    app: &App,
    run: &Value,
    name: &str,
    args: &Value,
    pending_id: &str,
    visibility: &Value,
) -> Result<Option<Value>> { prepare_on(&app.store,run,name,args,pending_id,visibility).await }
pub(crate) async fn preflight(
    app: &App,
    run: &mut Value,
    name: &str,
    args: &Value,
    visibility: &Value,
) -> Result<Option<Value>> { preflight_on(&app.store,run,name,args,visibility).await }
pub(crate) async fn claim(app: &App, run: &Value, pending: &Value) -> Result<Option<Value>> { claim_on(&app.store,run,pending).await }
pub(crate) async fn finish(
    app: &App,
    run: &Value,
    action: &Value,
    result: &Result<Value>,
) -> Result<Value> { finish_on(&app.store,run,action,result).await }
pub(crate) async fn reconcile(app: &App, run: &Value, action: &Value) -> Result<Option<Value>> { reconcile_on(&app.store,run,action).await }
pub(crate) async fn not_dispatched(app: &App, run: &Value, pending: &Value) -> Result<()> { not_dispatched_on(&app.store,run,pending).await }
pub(crate) async fn recover_results(app: &App, run: &mut Value) -> Result<bool> { recover_results_on(&app.store,run).await }
pub(crate) async fn interrupt_run(app: &App, run: &Value) -> Result<()> { interrupt_run_on(&app.store,run).await }
pub(crate) async fn lookup(app: &App, run: &Value, id: &str) -> Result<Value> {
    if !valid_id(id) {
        bail!("Invalid action ID");
    }
    let store = ledger(app, run).await?;
    let Some(action) = store.action_get(id).await? else {
        return Ok(Value::Null);
    };
    let agent = run["agent_id"].as_str().unwrap_or("shared_default");
    if action["requester"] != store.current_user_id()
        || action["resource"] != resource(agent, action["tool"].as_str().unwrap_or(""))
    {
        return Ok(Value::Null);
    }
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing session"))?;
    let (policy, _, visibility, state) = ability_controls(app, sid).await?;
    let access =
        crate::shared_chat::live_access(&app.store, store.current_user_id(), sid, agent).await?;
    let name = action["tool"].as_str().unwrap_or("");
    if !access.admitted()
        || live_tool_decision(
            app,
            &access,
            name,
            &policy,
            &visibility,
            &state,
            &run["loaded_abilities"],
            false,
        )
        .permission
            == access::Permission::Deny
    {
        return Ok(Value::Null);
    }
    let action = if matches!(action["state"].as_str(), Some("uncertain" | "running")) {
        reconcile(app, run, &action).await?.unwrap_or(action)
    } else {
        action
    };
    Ok(response(&action))
}
pub(crate) async fn statuses(app: &App, run: &Value) -> Result<Vec<Value>> {app.engine().action_statuses(run).await}

#[cfg(test)]
pub(crate) async fn test_purchase(
    app: &App,
    run: &Value,
    action: &Value,
    args: &Value,
) -> Result<Value> {
    if args["simulate"] == "fail_before_effect" && action["attempt"] == 1 {
        return Ok(json!({"status":"denied","output":"Provider confirmed no purchase occurred"}));
    }
    let key = action["provider_key"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing trusted provider key"))?;
    let result = json!({"status":"success","output":{"approved_for":run["requester_user_id"],"item":args["item"]},
        "receipt":{"external_id":format!("purchase-{}",digest(&json!(key))),"idempotency_key":key}});
    let saved = app.store.test_provider_purchase(key, &result).await?;
    if matches!(
        args["simulate"].as_str(),
        Some("timeout_after_effect" | "unverifiable")
    ) {
        return Ok(json!({"status":"timeout","uncertain":true}));
    }
    Ok(saved)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effect_identity_distinguishes_content_and_ignores_retry_preconditions() {
        let a = json!({"slot_name":"notes","content":"one","expected_revision":1});
        let b = json!({"slot_name":"notes","content":"one","expected_revision":2});
        assert_eq!(
            identity("edit_own_prompt", &a),
            identity("edit_own_prompt", &b)
        );
        assert_ne!(
            identity("edit_own_prompt", &a),
            identity(
                "edit_own_prompt",
                &json!({"slot_name":"notes","content":"two"})
            )
        );
        assert_eq!(
            identity("edit_own_prompt", &a)["family"],
            identity(
                "prompt_slot_write",
                &json!({"action":"put","slot_name":"notes","content":"one"})
            )["family"]
        );
    }
    async fn prepared(app: &App, id: &str, args: Value) -> (Value, Value) {
        let sid = crate::runtime_tests::conversation(app).await;
        crate::runtime_tests::activate_codebase(app, &sid).await;
        let rid = crate::runtime_tests::queued(app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        run["requester_user_id"] = json!(app.store.current_user_id());
        run["pending"] = json!({"id":id});
        let visibility = session_agent_abilities(app, &sid).await.unwrap();
        assert!(
            preflight(app, &mut run, "test_owner_purchase", &args, &visibility)
                .await
                .unwrap()
                .is_none()
        );
        let action = claim(app, &run, &run["pending"]).await.unwrap().unwrap()["action"].clone();
        (run, action)
    }
    #[tokio::test]
    async fn timeout_restart_and_provider_reconciliation_replay_one_effect() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let args = json!({"item":"book","simulate":"timeout_after_effect","_action":{"id":"purchase-one"}});
        let (run, action) = prepared(&app, "purchase-pending", args.clone()).await;
        let result = test_purchase(&app, &run, &action, &strip(&args)).await;
        assert_eq!(
            finish(&app, &run, &action, &result).await.unwrap()["action"]["state"],
            "uncertain"
        );
        assert_eq!(app.store.test_provider_effect_count().await.unwrap(), 1);
        // Reopening the user store simulates restart without retained conversation context.
        let mut restarted = app.clone();
        restarted.store = Store::open(_root.path()).await.unwrap();
        let resolved = reconcile(
            &restarted,
            &run,
            &restarted
                .store
                .action_get("purchase-one")
                .await
                .unwrap()
                .unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
        let replay = response(&resolved);
        assert_eq!(replay["action"]["state"], "completed");
        assert_eq!(replay["receipt"]["idempotency_key"], action["provider_key"]);
        assert_eq!(
            restarted.store.test_provider_effect_count().await.unwrap(),
            1
        );
    }
    #[tokio::test]
    async fn unavailable_reconciliation_never_reclaims_and_safe_failure_reuses_provider_key() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let args =
            json!({"item":"unknown","simulate":"unverifiable","_action":{"id":"unknown-action"}});
        let (run, action) = prepared(&app, "unknown-pending", args.clone()).await;
        let result = test_purchase(&app, &run, &action, &strip(&args)).await;
        finish(&app, &run, &action, &result).await.unwrap();
        let saved = app
            .store
            .action_get("unknown-action")
            .await
            .unwrap()
            .unwrap();
        assert!(reconcile(&app, &run, &saved).await.unwrap().is_none());
        assert_eq!(
            app.store
                .action_claim("unknown-action", true)
                .await
                .unwrap()["claimed"],
            false
        );
        let args =
            json!({"item":"safe","simulate":"fail_before_effect","_action":{"id":"safe-action"}});
        let (run, first) = prepared(&app, "safe-pending", args.clone()).await;
        let result = test_purchase(&app, &run, &first, &strip(&args)).await;
        assert_eq!(
            finish(&app, &run, &first, &result).await.unwrap()["action"]["state"],
            "confirmed_failed"
        );
        let next = app.store.action_claim("safe-action", true).await.unwrap()["action"].clone();
        assert_eq!(next["provider_key"], first["provider_key"]);
        let result = test_purchase(&app, &run, &next, &strip(&args)).await;
        assert_eq!(
            finish(&app, &run, &next, &result).await.unwrap()["action"]["state"],
            "completed"
        );
        assert_eq!(app.store.test_provider_effect_count().await.unwrap(), 2);
    }
    #[tokio::test]
    async fn native_receipt_resolves_crash_after_commit_and_before_ledger_save() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        run["pending"] = json!({"id":"native-pending"});
        let args = json!({"slot_name":"receipt_notes","content":"append once","expected_revision":0,"mode":"append","_action":{"id":"native-one"}});
        let visibility = session_agent_abilities(&app, &sid).await.unwrap();
        preflight(&app, &mut run, "edit_own_prompt", &args, &visibility)
            .await
            .unwrap();
        let action = claim(&app, &run, &run["pending"]).await.unwrap().unwrap()["action"].clone();
        app.store
            .self_edit_prompt_slot(
                "receipt_notes",
                "append once",
                "append",
                0,
                "native-pending",
                "original-scope",
            )
            .await
            .unwrap();
        interrupt_run(&app, &run).await.unwrap();
        let saved = app.store.action_get("native-one").await.unwrap().unwrap();
        let resolved = reconcile(&app, &run, &saved).await.unwrap().unwrap();
        assert_eq!(response(&resolved)["output"]["revision"], 1);
        assert_eq!(
            app.store
                .get_prompt_slot("receipt_notes")
                .await
                .unwrap()
                .unwrap()["content"],
            "append once"
        );
        assert_eq!(action["provider_key"], resolved["provider_key"]);
    }
    #[tokio::test]
    async fn configuration_off_cannot_bypass_existing_uncertain_intent() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let args = json!({"item":"held","simulate":"unverifiable","_action":{"id":"held-action"}});
        let (mut run, action) = prepared(&app, "held-pending", args.clone()).await;
        finish(&app, &run, &action, &Ok(json!({"status":"timeout"})))
            .await
            .unwrap();
        let visibility = json!({"tool_execution":{"test_owner_purchase":{"mode":"foreground","max_concurrency":1,"on_run_end":"stop","duplicate_protection":"off"}}});
        run["pending"] = json!({"id":"new-pending"});
        let result = preflight(
            &app,
            &mut run,
            "test_owner_purchase",
            &json!({"item":"held","simulate":"unverifiable"}),
            &visibility,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result["action"]["id"], "held-action");
        assert_eq!(result["action"]["state"], "uncertain");
        run["pending"] = json!({"id":"unprotected"});
        assert!(preflight(
            &app,
            &mut run,
            "test_owner_purchase",
            &json!({"item":"different"}),
            &visibility
        )
        .await
        .unwrap()
        .is_none());
        assert!(run["pending"]["action_id"].is_null());
    }
}

#[cfg(test)]
mod loop_tests {
    use super::*;
    async fn app(repeat: bool) -> (tempfile::TempDir, App, tokio::task::JoinHandle<()>) {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        let counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mock=Router::new().route("/responses",post(move || {
            let counter=counter.clone();
            async move {
                let index=counter.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                let output=if repeat && index==0 {json!([{"type":"function_call","call_id":"repeat-call",
                    "name":"prompt_slot_write","arguments":args("loop-action",0,None).to_string()}])}
                    else {json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Action verified."}]}])};
                let event=json!({"type":"response.completed","response":{"status":"completed","output":output}});
                ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, mock).await.unwrap();
        });
        app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
        (root, app, server)
    }
    fn args(id: &str, revision: i64, separate: Option<&str>) -> Value {
        json!({"action":"put","slot_name":"duplicate_action_test","content":"Harmless verification marker.",
            "expected_revision":revision,"_action":{"id":id,"separate_from":separate}})
    }
    async fn tool_run(app: &App, calls: Vec<Value>, generation: &str) -> (String, String) {
        let sid = crate::runtime_tests::conversation(app).await;
        app.store
            .put(
                &format!("session_mode:{sid}"),
                &json!({"mode":"write","execution_mode":"auto","revision":0}),
            )
            .await
            .unwrap();
        crate::runtime_tests::activate_codebase(app, &sid).await;
        let rid = crate::runtime_tests::queued(app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        run["phase"] = json!("tools");
        run["calls"] = json!(calls);
        run["input"]
            .as_array_mut()
            .unwrap()
            .extend(calls.iter().cloned());
        run["loaded_abilities"] = json!(["codebase_admin"]);
        run["context_hydration_pending"] = json!(false);
        app.store
            .save_run_observed(&mut run, "model_output", &json!({"output":calls}))
            .await
            .unwrap();
        assert!(app
            .store
            .claim_run(&mut run, "action-loop-test", generation)
            .await
            .unwrap());
        (sid, rid)
    }
    fn call(id: &str, args: Value) -> Value {
        json!({"type":"function_call","call_id":id,"name":"prompt_slot_write","arguments":args.to_string()})
    }
    #[tokio::test]
    async fn cancelled_run_restart_can_reconcile_running_claim_without_reexecution() {
        let (_root, app, server) = app(false).await;
        let (_, rid) = tool_run(
            &app,
            vec![call("cancelled-call", args("cancelled-action", 0, None))],
            "cancelled-generation",
        )
        .await;
        let mut run = app.run(&rid).await.unwrap();
        let arguments = args("cancelled-action", 0, None);
        run["pending"] = json!({"id":"cancelled-pending","call_id":"cancelled-call","name":"prompt_slot_write","arguments":arguments,"state":"executing"});
        assert!(
            preflight(&app, &mut run, "prompt_slot_write", &arguments, &json!({}))
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            claim(&app, &run, &run["pending"]).await.unwrap().unwrap()["claimed"],
            true
        );
        app.save(&mut run).await.unwrap();
        assert_eq!(
            crate::api::sessions::execute_prompt_slot_receipted(
                &app,
                "prompt_slot_write",
                &strip(&arguments),
                Some("cancelled-pending")
            )
            .await
            .unwrap()["status"],
            "success"
        );
        let _ = crate::command(
            State(app.clone()),
            Path((rid.clone(), "cancel".into())),
            Json(json!({"client_request_id":"cancel-receipt-test"})),
        )
        .await
        .unwrap();
        assert_eq!(
            app.store
                .action_get("cancelled-action")
                .await
                .unwrap()
                .unwrap()["state"],
            "running"
        );
        let mut restarted = app.clone();
        restarted.store = Store::open(_root.path()).await.unwrap();
        let mut run = restarted.run(&rid).await.unwrap();
        assert_eq!(run["status"], "recovery");
        let saved = lookup(&restarted, &run, "cancelled-action").await.unwrap();
        assert_eq!(saved["action"]["state"], "completed");
        assert_eq!(saved["action"]["attempt"], 1);
        run["pending"] = json!({"id":"retry-pending"});
        let replay = preflight(
            &restarted,
            &mut run,
            "prompt_slot_write",
            &arguments,
            &json!({}),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(replay["replayed_action"], true);
        assert_eq!(replay["output"]["slot"]["revision"], 1);
        assert_eq!(restarted.run(&rid).await.unwrap()["status"], "recovery");
        server.abort();
    }
    #[tokio::test]
    async fn restart_recovers_committed_effect_and_compaction_retains_action_result() {
        let (_root, app, server) = app(false).await;
        let (sid, rid) = tool_run(
            &app,
            vec![call("interrupted", args("restart-action", 0, None))],
            "before-restart",
        )
        .await;
        let mut run = app.run(&rid).await.unwrap();
        let arguments = args("restart-action", 0, None);
        run["pending"] = json!({"id":"restart-pending","call_id":"interrupted","name":"prompt_slot_write","arguments":arguments,"state":"executing"});
        assert!(
            preflight(&app, &mut run, "prompt_slot_write", &arguments, &json!({}))
                .await
                .unwrap()
                .is_none()
        );
        let action = claim(&app, &run, &run["pending"]).await.unwrap().unwrap()["action"].clone();
        app.save(&mut run).await.unwrap();
        let result = crate::api::sessions::execute_prompt_slot_receipted(
            &app,
            "prompt_slot_write",
            &strip(&arguments),
            Some("restart-pending"),
        )
        .await
        .unwrap();
        assert_eq!(result["status"], "success");
        // Crash after the native mutation commits but before the action outcome is saved.
        let mut restarted = app.clone();
        restarted.store = Store::open(_root.path()).await.unwrap();
        crate::recover(&restarted, true).await.unwrap();
        let mut recovered = restarted.run(&rid).await.unwrap();
        assert_eq!(recovered["uncertain"], false);
        assert_eq!(
            restarted
                .store
                .action_get("restart-action")
                .await
                .unwrap()
                .unwrap()["state"],
            "completed"
        );
        let source = recovered["input"].clone();
        recovered["input"] = json!([{"role":"user","content":"Action already handled; continue from this summary."}]);
        recovered["compactions"] = json!(1);
        restarted.store.save_run_observed(&mut recovered,"context_compacted",&json!({"run_id":rid,"segment_id":format!("{rid}-1"),"source_digest":digest(&source),"source":source,"dropped_items":source.as_array().unwrap().len()})).await.unwrap();
        assert_eq!(restarted.store.context(&sid).await.unwrap().len(), 1);
        let saved = restarted
            .store
            .action_get("restart-action")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(response(&saved)["output"]["slot"]["revision"], 1);
        assert_eq!(saved["provider_key"], action["provider_key"]);
        assert!(restarted
            .store
            .claim_run(&mut recovered, "action-loop-test", "after-restart")
            .await
            .unwrap());
        crate::execute_run(
            restarted.clone(),
            rid.clone(),
            "after-restart".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(restarted.run(&rid).await.unwrap()["status"], "completed");
        assert_eq!(
            restarted
                .store
                .get_prompt_slot("duplicate_action_test")
                .await
                .unwrap()
                .unwrap()["revision"],
            1
        );
        assert_eq!(
            restarted
                .store
                .chat_snapshot(&sid)
                .await
                .unwrap()
                .0
                .iter()
                .filter(|m| m["role"] == "assistant" && m["content"] == "Action verified.")
                .count(),
            1
        );
        server.abort();
    }
    #[tokio::test]
    async fn repeated_action_across_model_calls_mutates_once_and_publishes_once() {
        let (_root, app, server) = app(true).await;
        let (sid, rid) = tool_run(
            &app,
            vec![call("first-call", args("loop-action", 0, None))],
            "loop-generation",
        )
        .await;
        crate::execute_run(
            app.clone(),
            rid.clone(),
            "loop-generation".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
        assert_eq!(
            app.store
                .get_prompt_slot("duplicate_action_test")
                .await
                .unwrap()
                .unwrap()["revision"],
            1
        );
        let context = app.store.context(&sid).await.unwrap();
        let results = context
            .iter()
            .filter(|v| v["type"] == "function_call_output")
            .map(|v| serde_json::from_str::<Value>(v["output"].as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 2);
        assert_eq!(results[1]["replayed_action"], true);
        assert!(!results[0]["receipt"].is_null());
        let (messages, _) = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(
            messages
                .iter()
                .filter(|m| m["role"] == "assistant" && m["content"] == "Action verified.")
                .count(),
            1
        );
        server.abort();
    }
    #[tokio::test]
    async fn batch_duplicate_ids_and_separate_intent_preserve_normal_authorization() {
        let (_root, app, server) = app(false).await;
        let (_, rid) = tool_run(
            &app,
            vec![
                call("first", args("batch-one", 0, None)),
                call("duplicate", args("batch-two", 0, None)),
            ],
            "batch-generation",
        )
        .await;
        crate::execute_run(
            app.clone(),
            rid.clone(),
            "batch-generation".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
        assert_eq!(
            app.store
                .get_prompt_slot("duplicate_action_test")
                .await
                .unwrap()
                .unwrap()["revision"],
            1
        );
        let (sid, rid) = tool_run(
            &app,
            vec![call(
                "additional",
                args("additional-action", 1, Some("batch-one")),
            )],
            "additional-generation",
        )
        .await;
        crate::execute_run(
            app.clone(),
            rid.clone(),
            "additional-generation".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        let waiting = app.run(&rid).await.unwrap();
        assert_eq!(waiting["status"], "waiting_approval");
        assert_eq!(waiting["pending"]["new_intent_confirmation"], true);
        let approval_id = waiting["pending"]["id"].as_str().unwrap().to_owned();
        let mut other = app.clone();
        other.chat_actor = Some("another-requester".into());
        assert_eq!(
            crate::api::chat::approval(
                State(other),
                Path(approval_id.clone()),
                Json(json!({"client_request_id":"wrong-owner","decision":"approve"}))
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::FORBIDDEN
        );
        crate::api::chat::approval(
            State(app.clone()),
            Path(approval_id),
            Json(json!({"client_request_id":"confirm-additional","decision":"approve"})),
        )
        .await
        .unwrap();
        let mut run = app.run(&rid).await.unwrap();
        assert!(app
            .store
            .claim_run(&mut run, "action-loop-test", "approved-generation")
            .await
            .unwrap());
        crate::execute_run(
            app.clone(),
            rid.clone(),
            "approved-generation".into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&rid).await.unwrap()["status"], "completed");
        assert_eq!(
            app.store
                .get_prompt_slot("duplicate_action_test")
                .await
                .unwrap()
                .unwrap()["revision"],
            2
        );
        assert_eq!(
            app.store
                .chat_snapshot(&sid)
                .await
                .unwrap()
                .0
                .iter()
                .filter(|m| m["role"] == "assistant" && m["content"] == "Action verified.")
                .count(),
            1
        );
        server.abort();
    }
}

async fn ledger_on(base: &Store, run: &Value) -> Result<Store> {
    let requester = run["requester_user_id"]
        .as_str()
        .unwrap_or(base.current_user_id());
    if requester == base.current_user_id() {
        Ok(base.clone())
    } else {
        base.for_user(requester).await
    }
}

pub(crate) async fn prepare_on(
    base: &Store,
    run: &Value,
    name: &str,
    args: &Value,
    pending_id: &str,
    visibility: &Value,
) -> Result<Option<Value>> {
    if !supported(name) {
        if args.get("_action").is_some() {
            bail!("Tool does not support action metadata");
        }
        return Ok(None);
    }
    let store = ledger_on(base, run).await?;
    let clean = strip(args);
    let meta: Option<Metadata> = args
        .get("_action")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?;
    if meta.as_ref().is_some_and(|m| {
        !valid_id(&m.id) || m.separate_from.as_ref().is_some_and(|id| !valid_id(id))
    }) {
        bail!("Invalid action identity");
    }
    let id = meta
        .as_ref()
        .map(|m| m.id.clone())
        .unwrap_or_else(|| format!("action-{pending_id}"));
    // Existing IDs remain authoritative even after configuration is switched off.
    let existing = store.action_get(&id).await?;
    let settings = crate::tool_execution::resolve(visibility, name)?;

    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing action session"))?;
    let rid = run["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing action run"))?;
    let agent = run["agent_id"].as_str().unwrap_or("shared_default");
    let mut effect = identity(name, &clean);
    // Empty optional prompt metadata is equivalent across both prompt adapters.
    if effect["family"] == "prompt" && effect["order_index"].is_null() && effect["lock"].is_null() {
        if let Some(object) = effect.as_object_mut() {
            object.remove("order_index");
            object.remove("lock");
        }
    }
    let candidate = json!({"schema_version":1,"id":id,"requester":store.current_user_id(),"tool":name,
        "resource":resource(agent,name),"agent_id":agent,"session_id":sid,"run_id":rid,
        "origin":base.action_origin(sid,rid).await?,"arguments":clean,"arguments_digest":digest(&clean),
        "intent_key":digest(&effect),"provider_key":format!("wa-{}",digest(&json!([store.current_user_id(),resource(agent,name),id]))),
        "pending_id":pending_id,"created_at":now()});
    if settings.duplicate_protection == Some(Protection::Off)
        && existing.is_none()
        && store
            .action_match(
                field(&candidate, "resource")?,
                field(&candidate, "intent_key")?,
            )
            .await?
            .is_none()
    {
        return Ok(None);
    }
    Ok(Some(
        store
            .action_prepare(
                &candidate,
                meta.as_ref().and_then(|m| m.separate_from.as_deref()),
            )
            .await?,
    ))
}

pub(crate) async fn preflight_on(
    base: &Store,
    run: &mut Value,
    name: &str,
    args: &Value,
    visibility: &Value,
) -> Result<Option<Value>> {
    let pid = run["pending"]["action_pending_id"]
        .as_str()
        .or_else(|| run["pending"]["id"].as_str())
        .unwrap_or("")
        .to_owned();
    let Some(prepared) = prepare_on(base, run, name, args, &pid, visibility).await? else {
        return Ok(None);
    };
    let mut prepared = prepared;
    if matches!(
        prepared["action"]["state"].as_str(),
        Some("uncertain" | "running")
    ) {
        if let Some(resolved) = reconcile_on(base, run, &prepared["action"]).await? {
            prepared["action"] = resolved;
        }
    }
    let action = &prepared["action"];
    match prepared["kind"].as_str() {
        Some("conflict") => Ok(Some(
            json!({"status":"denied","output":"Action ID conflicts with its saved intent/arguments","action":summary(action)}),
        )),
        Some("duplicate") => {
            let mut value = if action["arguments_digest"] != digest(&strip(args))
                && action["tool"] == "test_owner_purchase"
            {
                json!({"status":"new_intent_required","action":summary(action),"output":"Similar action found with different commercial details. Distinguish new intent and obtain specific authorization; no execution occurred."})
            } else {
                response(action)
            };
            value["duplicate_notice"]=json!("Matching intent found. This is the prior action, not authorization for an additional action.");
            value["requested_action_id"] = args["_action"]["id"].clone();
            Ok(Some(value))
        }
        _ => {
            run["pending"]["action_id"] = action["id"].clone();
            run["pending"]["action_pending_id"] = action["pending_id"].clone();
            run["pending"]["new_intent_confirmation"] =
                action["requires_new_intent_approval"].clone();
            run["pending"]["separate_from"] = action["separate_from"].clone();
            if matches!(
                action["state"].as_str(),
                Some("running" | "completed" | "uncertain")
            ) {
                Ok(Some(response(action)))
            } else {
                Ok(None)
            }
        }
    }
}

pub(crate) async fn claim_on(base: &Store, run: &Value, pending: &Value) -> Result<Option<Value>> {
    let Some(id) = pending["action_id"].as_str() else {
        return Ok(None);
    };
    let store = ledger_on(base, run).await?;
    Ok(Some(
        store
            .action_claim(id, pending["human_approved"] == true)
            .await?,
    ))
}

pub(crate) async fn finish_on(
    base: &Store,
    run: &Value,
    action: &Value,
    result: &Result<Value>,
) -> Result<Value> {
    let store = ledger_on(base, run).await?;
    let (state, value, retry_safe) = match result {
        Ok(v) if v["status"] == "success" && v["uncertain"] != true => {
            ("completed", v.clone(), false)
        }
        Ok(v) if v["status"] == "denied" && v["uncertain"] != true => {
            ("confirmed_failed", v.clone(), true)
        }
        _ => (
            "uncertain",
            result.as_ref().ok().cloned().unwrap_or(Value::Null),
            false,
        ),
    };
    let receipt = if state == "completed" && action["tool"] != "test_owner_purchase" {
        base
            .action_agent_receipt(
                action["agent_id"].as_str().unwrap_or("shared_default"),
                action["tool"].as_str().unwrap_or(""),
                action["pending_id"].as_str().unwrap_or(""),
            )
            .await?
            .unwrap_or(Value::Null)
    } else {
        value["receipt"].clone()
    };
    let changed = store
        .action_finish(
            field(action, "id")?,
            field(action, "claim")?,
            state,
            &value,
            &receipt,
            retry_safe,
        )
        .await?;
    let saved = store
        .action_get(field(action, "id")?)
        .await?
        .ok_or_else(|| anyhow!("Missing action outcome"))?;
    if !changed {
        return Ok(response(&saved));
    }
    if state == "uncertain" {
        return Ok(
            json!({"status":"uncertain","uncertain":true,"action":summary(&saved),"output":"Action outcome requires reconciliation; no blind retry"}),
        );
    }
    let mut value = value;
    value["action"] = summary(&saved);
    value["receipt"] = saved["receipt"].clone();
    Ok(value)
}

pub(crate) async fn reconcile_on(base: &Store, run: &Value, action: &Value) -> Result<Option<Value>> {
    let name = action["tool"].as_str().unwrap_or("");
    #[cfg(test)]
    if name == "test_owner_purchase" {
        if action["arguments"]["simulate"] == "unverifiable" {
            return Ok(None);
        }
        let Some(result) = base
            .test_provider_lookup(field(action, "provider_key")?)
            .await?
        else {
            return Ok(None);
        };
        let store = ledger_on(base, run).await?;
        store
            .action_finish(
                field(action, "id")?,
                action["claim"].as_str().unwrap_or(""),
                "completed",
                &result,
                &result["receipt"],
                false,
            )
            .await?;
        return store.action_get(field(action, "id")?).await;
    }
    let Some(receipt) = base
        .action_agent_receipt(
            action["agent_id"].as_str().unwrap_or("shared_default"),
            name,
            action["pending_id"].as_str().unwrap_or(""),
        )
        .await?
    else {
        return Ok(None);
    };
    if name == "prompt_slot_write" && receipt["arguments_digest"] != action["arguments_digest"] {
        bail!("Action reconciliation receipt mismatch");
    }
    // The self-edit receipt key is native-owned and bound to the original pending
    // identity. Its mutation and result commit together in the agent transaction.
    let result = &receipt["result"];
    if result["status"] != "success" {
        return Ok(None);
    }
    let store = ledger_on(base, run).await?;
    store
        .action_finish(
            field(action, "id")?,
            action["claim"].as_str().unwrap_or(""),
            "completed",
            result,
            &receipt,
            false,
        )
        .await?;
    store.action_get(field(action, "id")?).await
}

pub(crate) async fn not_dispatched_on(base: &Store, run: &Value, pending: &Value) -> Result<()> {
    if let Some(id) = pending["action_id"].as_str() {
        ledger_on(base, run).await?.action_not_dispatched(id).await?;
    }
    Ok(())
}

pub(crate) async fn recover_results_on(base: &Store, run: &mut Value) -> Result<bool> {
    let store = ledger_on(base, run).await?;
    let has_batch = run["batch"].is_array();
    let pending = if has_batch {
        run["batch"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
            .filter(|(_, i)| matches!(i["state"].as_str(), Some("executing" | "uncertain")))
            .map(|(n, i)| (Some(n), i["pending"].clone()))
            .collect::<Vec<_>>()
    } else {
        vec![(None, run["pending"].clone())]
    };
    let mut resolved = true;
    for (index, pending) in pending {
        let name = pending["name"].as_str().unwrap_or("");
        if tools::is_read(name) {
            continue;
        }
        let Some(id) = pending["action_id"].as_str() else {
            resolved = false;
            continue;
        };
        let Some(mut action) = store.action_get(id).await? else {
            resolved = false;
            continue;
        };
        if action["state"] == "uncertain" {
            action = reconcile_on(base, run, &action).await?.unwrap_or(action);
        }
        if !matches!(
            action["state"].as_str(),
            Some("completed" | "confirmed_failed")
        ) {
            resolved = false;
            continue;
        }
        let result = if action["state"] == "completed" {
            response(&action)
        } else {
            let mut result = action["result"].clone();
            result["action"] = summary(&action);
            result
        };
        if let Some(index) = index {
            run["batch"][index]["state"] = json!("done");
            run["batch"][index]["result"] = result;
        } else {
            crate::agent_loop::append_tool(run, &result)?;
        }
    }
    if resolved {
        if let Some(items) = run["batch"].as_array_mut() {
            for item in items {
                if item["state"] == "executing"
                    && tools::is_read(item["pending"]["name"].as_str().unwrap_or(""))
                {
                    item["state"] = json!("requested");
                    item["pending"]["state"] = json!("requested");
                }
            }
        }
    }
    Ok(resolved)
}

pub(crate) async fn interrupt_run_on(base: &Store, run: &Value) -> Result<()> {
    let store = ledger_on(base, run).await?;
    for pending in std::iter::once(&run["pending"]).chain(
        run["batch"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|i| &i["pending"]),
    ) {
        if let Some(id) = pending["action_id"].as_str() {
            store.action_interrupt(id).await?;
        }
    }
    Ok(())
}
impl webagent_engine::runtime::actions::Actions<Store> for crate::tool_execution::NativeActions {
 fn strip(args:&Value)->Value {strip(args)}
 fn response(action:&Value)->Value {response(action)}
 fn summary(action:&Value)->Value {summary(action)}
 fn ledger<'a>(&'a self,store:&'a Store,run:&'a Value)->impl std::future::Future<Output=Result<Store>>+Send+'a {ledger_on(store,run)}
 fn list_for_agent<'a>(&'a self,ledger:&'a Store,agent:&'a str)->impl std::future::Future<Output=Result<Vec<Value>>>+Send+'a {async move {ledger.action_list(&format!("agent:{agent}")).await}}
 fn prepare<'a>(&'a self,store:&'a Store,run:&'a Value,name:&'a str,args:&'a Value,pending_id:&'a str,visibility:&'a Value)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {prepare_on(store,run,name,args,pending_id,visibility)}
 fn preflight<'a>(&'a self,store:&'a Store,run:&'a mut Value,name:&'a str,args:&'a Value,visibility:&'a Value)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {preflight_on(store,run,name,args,visibility)}
 fn claim<'a>(&'a self,store:&'a Store,run:&'a Value,pending:&'a Value)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {claim_on(store,run,pending)}
 fn finish<'a>(&'a self,store:&'a Store,run:&'a Value,action:&'a Value,result:&'a Result<Value>)->impl std::future::Future<Output=Result<Value>>+Send+'a {finish_on(store,run,action,result)}
 fn reconcile<'a>(&'a self,store:&'a Store,run:&'a Value,action:&'a Value)->impl std::future::Future<Output=Result<Option<Value>>>+Send+'a {reconcile_on(store,run,action)}
 fn not_dispatched<'a>(&'a self,store:&'a Store,run:&'a Value,pending:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a {not_dispatched_on(store,run,pending)}
 fn recover_results<'a>(&'a self,store:&'a Store,run:&'a mut Value)->impl std::future::Future<Output=Result<bool>>+Send+'a {recover_results_on(store,run)}
 fn interrupt_run<'a>(&'a self,store:&'a Store,run:&'a Value)->impl std::future::Future<Output=Result<()>>+Send+'a {interrupt_run_on(store,run)}
}
