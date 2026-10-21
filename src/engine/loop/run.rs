//! Thin role composition for the independently compiled Engine execution.
use super::*;
use crate::engine_runtime::Engine;
pub(crate) async fn execute_run(app:App,id:String,generation:String,cancel:CancellationToken)->Result<()> {Engine::execute_run(app.engine(),id,generation,cancel).await}
pub(crate) fn settle_abandoned_interrupt(run:&mut Value)->Result<()> {Engine::settle_abandoned_interrupt(run)}
pub(crate) async fn settle_followup_interrupt(app:&App,run:&mut Value)->Result<bool> {Engine::settle_followup_interrupt(&app.engine(),run).await}
fn record_tool_interruption(run:&mut Value,reason:&str) {Engine::record_tool_interruption(run,reason)}
fn record_interrupted_tool(run:&mut Value,name:&str)->Result<Option<Value>> {Engine::record_interrupted_tool(run,name)}
async fn preserve_restart_interruption(app:&App,run:&mut Value)->Result<bool> {Engine::preserve_restart_interruption(&app.engine(),run).await}
async fn commit_tool_transport_failure(app:&App,run:&mut Value,name:&str)->Result<()> {Engine::commit_tool_transport_failure(&app.engine(),run,name).await}
#[cfg(test)]
fn followup_interruption_result(executing:bool,completed:Option<&Value>)->Value {Engine::fixture_followup_interruption_result(executing,completed)}
#[cfg(test)]
fn execute_run_inner(app:App,id:String,generation:String,cancel:CancellationToken)->impl std::future::Future<Output=Result<()>> {
 Engine::fixture_execute_run_inner(app.engine(),id,generation,cancel)
}
#[cfg(test)]
mod interrupted_tool_tests {
    use super::*;

    fn pending(name:&str)->Value {
        json!({"id":"run-1","session_id":"session-1","generation":"generation-1",
            "status":"running","turns":1,"call_index":0,"uncertain":false,
            "input":[{"type":"function_call","call_id":"call-1","name":name,"arguments":"{}"}],
            "pending":{"name":name,"call_id":"call-1","state":"executing"},
            "workflow":{"receipts":{"pre_tool":{"checkpoint":"pre_tool",
                "generation":"generation-1","policy_revision":"policy-1","input_digest":"digest",
                "turn":1,"call_index":0,"result":{"decision":"pass"}}}}})
    }

    #[test]
    fn followup_interrupt_does_not_invent_cancellation_or_lose_completed_effects() {
        let not_dispatched=followup_interruption_result(false,None);
        assert_eq!(not_dispatched["execution_state"],"not_dispatched");
        assert_eq!(not_dispatched["interrupted_by"],"user");
        let interrupted_read=followup_interruption_result(true,None);
        assert_eq!(interrupted_read["execution_state"],"read_interrupted");
        assert!(interrupted_read.get("cancellation_confirmed").is_none());
        let completed=json!({"status":"success","output":{"operation_id":"operation-1","written":true}});
        assert_eq!(followup_interruption_result(true,Some(&completed)),completed);
    }

    #[tokio::test]
    async fn abandoned_write_handoff_persists_unknown_pair_without_replay() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
        store.put("session:session-1",&json!({"id":"session-1","status":"active","agent_id":"shared_default"})).await.unwrap();
        let mut old=pending("file_write");
        old["calls"]=json!([old["input"][0].clone()]);
        old["run_revision"]=json!(0);old["updated_at"]=json!(now());
        store.put("run:run-1",&old).await.unwrap();
        let calls=old["calls"].clone();
        store.save_run(&mut old,"model_output",&json!({"output":calls})).await.unwrap();
        old["status"]=json!("cancelled");old["recovery_abandoned"]=json!(true);
        old["pending"]["id"]=json!("write-invocation");
        store.save_run(&mut old,"run_status",&json!({"run_id":"run-1"})).await.unwrap();
        let entry=json!({"id":"followup-1","message_id":"user:followup-1","session_id":"session-1",
            "agent_id":"shared_default","revision":0,"status":"pending","review_status":"acceptable",
            "choice":"interrupt","target_run_id":"run-1","content":"check the result",
            "context_content":"check the result","attachments":[]});
        store.followup_submit("session-1",entry,vec![],"submission","digest",&json!({})).await.unwrap();
        settle_abandoned_interrupt(&mut old).unwrap();
        let once=old["input"].clone();settle_abandoned_interrupt(&mut old).unwrap();
        assert_eq!(old["input"],once);
        let mut next=json!({"id":"replacement","session_id":"session-1","status":"queued",
            "generation":"","run_revision":0,"updated_at":now(),"input":old["input"].clone()});
        assert!(store.followup_consume("session-1","interrupt",&mut next,Some(&mut old)).await.unwrap());
        let reopened=Store::open(root.path()).await.unwrap();
        let context=reopened.context("session-1").await.unwrap();
        let i=context.iter().position(|item|item["type"]=="function_call"&&item["call_id"]=="call-1").unwrap();
        assert_eq!(context[i+1]["type"],"function_call_output");
        let result:Value=serde_json::from_str(context[i+1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["execution_state"],"outcome_unknown");assert_eq!(result["no_retry"],true);
        assert_eq!(result["operation_refs"]["invocation_id"],"write-invocation");
        assert_eq!(context[i+2]["content"],"check the result");
        assert!(old["pending"].is_null());
    }

    #[tokio::test]
    async fn selected_interrupt_settles_remaining_serial_calls_before_terminal_commit() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();
        let template=run.clone();
        assert!(app.store.claim_run(&mut run,"serial-interrupt-test",&uuid()).await.unwrap());
        let calls=json!([
            {"type":"function_call","call_id":"read-serial","name":"file_read","arguments":"{}"},
            {"type":"function_call","call_id":"publish-undispatched","name":"publish_main_message","arguments":"{}"}
        ]);
        run["input"].as_array_mut().unwrap().extend(calls.as_array().unwrap().iter().cloned());
        run["calls"]=calls.clone();run["turns"]=json!(1);run["call_index"]=json!(0);
        run["phase"]=json!("tools");
        run["pending"]=json!({"name":"file_read","call_id":"read-serial","state":"executing"});
        let policy=workflow::Policy::resolve(None).unwrap();
        workflow::record(&mut run,&policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:"file_read".into(),arguments_digest:"fixture".into(),policy_version:1,
            mode_revision:0,visibility_revision:0,ability_state_revision:0,
        }),workflow::Decision::Pass).unwrap();
        app.store.save_run_observed(&mut run,"model_output",&json!({"output":calls})).await.unwrap();
        let e=json!({"id":"selected-interrupt","message_id":"user:selected-interrupt","session_id":sid,
            "agent_id":"shared_default","revision":0,"choice":"interrupt","status":"pending",
            "review_status":"acceptable","target_run_id":rid,"content":"change direction",
            "context_content":"change direction","attachments":[],"template":template});
        app.store.followup_submit(&sid,e,vec![],"selected-interrupt","fixture-digest",&json!({})).await.unwrap();
        // Exercise the production transport-error commit/handoff, not a hand-built
        // equivalent. Committing failed before the second result used to reject it.
        commit_tool_transport_failure(&app,&mut run,"file_read").await.unwrap();
        assert_eq!(run["status"],"cancelled");
        let q=app.store.followup_queue(&sid).await.unwrap();
        assert_eq!(q["entries"][0]["status"],"delivered");
        let next=app.run(q["entries"][0]["consuming_run_id"].as_str().unwrap()).await.unwrap();
        let input=next["input"].as_array().unwrap();
        for call in ["read-serial","publish-undispatched"] {
            assert_eq!(input.iter().filter(|item|item["type"]=="function_call_output"&&item["call_id"]==call).count(),1);
        }
        let result:Value=serde_json::from_str(input.iter().find(|item|item["type"]=="function_call_output"
            &&item["call_id"]=="publish-undispatched").unwrap()["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["execution_state"],"not_dispatched");
        assert_eq!(input.last().unwrap()["content"],"change direction");
    }

    #[tokio::test]
    async fn late_interrupt_settles_failed_parent_atomically_and_once() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let template=run.clone();
        assert!(app.store.claim_run(&mut run,"late-interrupt-test",&uuid()).await.unwrap());
        let calls=json!([
            {"type":"function_call","call_id":"late-read","name":"file_read","arguments":"{}"},
            {"type":"function_call","call_id":"late-publish","name":"publish_main_message","arguments":"{}"}
        ]);
        run["input"].as_array_mut().unwrap().extend(calls.as_array().unwrap().iter().cloned());
        run["calls"]=calls.clone();run["turns"]=json!(1);run["call_index"]=json!(0);run["phase"]=json!("tools");
        run["pending"]=json!({"name":"file_read","call_id":"late-read","state":"executing"});
        let policy=workflow::Policy::resolve(None).unwrap();
        workflow::record(&mut run,&policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:"file_read".into(),arguments_digest:"fixture".into(),policy_version:1,
            mode_revision:0,visibility_revision:0,ability_state_revision:0,
        }),workflow::Decision::Pass).unwrap();
        app.store.save_run_observed(&mut run,"model_output",&json!({"output":calls})).await.unwrap();
        let receipt=record_interrupted_tool(&mut run,"file_read").unwrap().unwrap();
        app.store.save_run_observed(&mut run,"tool_result",&json!({"run_id":rid,"name":"file_read","result":receipt})).await.unwrap();
        assert_eq!(run["status"],"failed");
        // Deterministically persist the choice after failure, with the target
        // already bound while the run was running (the narrow commit race).
        let e=json!({"id":"late-interrupt","message_id":"user:late-interrupt","session_id":sid,
            "agent_id":"shared_default","revision":0,"choice":"interrupt","status":"pending",
            "review_status":"acceptable","interrupt_committed":true,"target_run_id":rid,
            "content":"new direction","context_content":"new direction","attachments":[],"template":template});
        app.store.followup_submit(&sid,e,vec![],"late-interrupt","digest",&json!({})).await.unwrap();
        crate::followup_queue::scheduler_tick(&app).await.unwrap();
        crate::followup_queue::scheduler_tick(&app).await.unwrap();
        let q=app.store.followup_queue(&sid).await.unwrap();assert_eq!(q["entries"][0]["status"],"delivered");
        let active=app.store.active_runs().await.unwrap();
        assert_eq!(active.iter().filter(|r|r["session_id"]==sid).count(),1);
        let next=app.run(q["entries"][0]["consuming_run_id"].as_str().unwrap()).await.unwrap();
        let input=next["input"].as_array().unwrap();
        for id in ["late-read","late-publish"] {
            assert_eq!(input.iter().filter(|i|i["type"]=="function_call_output"&&i["call_id"]==id).count(),1);
        }
        let result:Value=serde_json::from_str(input.iter().find(|i|i["type"]=="function_call_output"&&i["call_id"]=="late-publish").unwrap()["output"].as_str().unwrap()).unwrap();
        assert_eq!(result["execution_state"],"not_dispatched");
        assert_eq!(app.run(&rid).await.unwrap()["status"],"cancelled");
        let context=app.store.context(&sid).await.unwrap();
        assert_eq!(context.iter().filter(|i|i["type"]=="function_call_output"&&i["call_id"]=="late-publish").count(),1);
    }

    #[tokio::test]
    async fn terminal_unknown_write_holds_interrupt_without_replacement_or_fake_result() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let mut run=app.run(&rid).await.unwrap();let template=run.clone();
        assert!(app.store.claim_run(&mut run,"terminal-write-test",&uuid()).await.unwrap());
        let call=json!({"type":"function_call","call_id":"unknown-write","name":"file_write","arguments":"{}"});
        run["input"].as_array_mut().unwrap().push(call.clone());run["calls"]=json!([call.clone()]);run["turns"]=json!(1);
        run["pending"]=json!({"name":"file_write","call_id":"unknown-write","state":"executing"});
        app.store.save_run_observed(&mut run,"model_output",&json!({"output":[call]})).await.unwrap();
        // Simulate a terminal checkpoint lacking an acknowledged write result.
        run["status"]=json!("cancelled");run["uncertain"]=json!(false);
        app.store.save_run_observed(&mut run,"run_status",&json!({"run_id":rid})).await.unwrap();
        let e=json!({"id":"write-interrupt","message_id":"user:write-interrupt","session_id":sid,
            "agent_id":"shared_default","revision":0,"choice":"interrupt","status":"pending",
            "review_status":"acceptable","interrupt_committed":true,"target_run_id":rid,
            "content":"check write","context_content":"check write","attachments":[],"template":template});
        app.store.followup_submit(&sid,e,vec![],"write-interrupt","digest",&json!({})).await.unwrap();
        crate::followup_queue::scheduler_tick(&app).await.unwrap();
        crate::followup_queue::scheduler_tick(&app).await.unwrap();
        let q=app.store.followup_queue(&sid).await.unwrap();assert_eq!(q["held"],true);
        assert_eq!(q["entries"][0]["status"],"pending");assert!(q["entries"][0]["consuming_run_id"].is_null());
        assert!(!app.store.active_runs().await.unwrap().iter().any(|r|r["session_id"]==sid));
        let parent=app.run(&rid).await.unwrap();assert_eq!(parent["pending"]["state"],"executing");
        assert!(!parent["input"].as_array().unwrap().iter().any(|i|i["type"]=="function_call_output"&&i["call_id"]=="unknown-write"));
        assert!(!app.store.context(&sid).await.unwrap().iter().any(|i|i["type"]=="function_call_output"&&i["call_id"]=="unknown-write"));
    }

    #[test]
    fn interruption_reason_is_bounded_and_preserves_call() {
        for (reason, expected) in [("timeout","timeout"),("cancelled","cancelled"),
            ("transport_error","transport_error"),("PRIVATE_ERROR","uncertain")] {
            let mut run = pending("shell");
            record_tool_interruption(&mut run, reason);
            assert_eq!(run["pending"]["interruption"]["reason"], expected);
            assert!(run["pending"]["interruption"]["recorded_at"].as_i64().is_some());
            assert_eq!(run["pending"]["call_id"], "call-1");
            assert!(!run.to_string().contains("PRIVATE_ERROR"));
            assert_eq!(run["input"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn interrupted_read_gets_matching_safe_result_and_fails_closed() {
        let mut run=pending("file_read");
        let result=record_interrupted_tool(&mut run,"file_read").unwrap().unwrap();
        assert_eq!(run["status"],"failed");
        assert_eq!(run["error"],"Read tool interrupted");
        assert!(run["pending"].is_null());
        assert_eq!(run["uncertain"],false);
        assert_eq!(result["status"],"error");
        assert!(result["output"].as_str().unwrap().contains("no data was returned"));
        let input=run["input"].as_array().unwrap();
        assert_eq!(input.len(),2);
        assert_eq!(input[0]["type"],"function_call");
        assert_eq!(input[1]["type"],"function_call_output");
        assert_eq!(input[0]["call_id"],input[1]["call_id"]);
        let receipt:Value=serde_json::from_str(input[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt,result);
        let workflow_result=&run["workflow"]["receipts"]["tool_result"];
        assert_eq!(workflow_result["checkpoint"],"tool_result");
        assert!(!workflow_result["input_digest"].as_str().unwrap_or("").is_empty());
    }

    #[test]
    fn interrupted_write_stays_uncertain_without_a_result_or_retry() {
        let mut run=pending("file_write");
        assert!(record_interrupted_tool(&mut run,"file_write").unwrap().is_none());
        assert_eq!(run["status"],"recovery");
        assert_eq!(run["uncertain"],true);
        assert_eq!(run["input"].as_array().unwrap().len(),1);
        assert_eq!(run["pending"]["call_id"],"call-1");
        assert_eq!(run["workflow"]["receipts"]["tool_result"],Value::Null);
    }

    #[tokio::test]
    async fn interrupted_read_persists_a_paired_result_in_next_context() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let sid=crate::runtime_tests::conversation(&app).await;
        let rid=crate::runtime_tests::queued(&app,&sid).await;
        let generation=uuid();
        let mut run=app.run(&rid).await.unwrap();
        assert!(app.store.claim_run(&mut run,"interrupted-read-test",&generation).await.unwrap());

        let call=json!({"type":"function_call","call_id":"call-interrupted-read",
            "name":"file_read","arguments":"{}"});
        run["input"].as_array_mut().unwrap().push(call.clone());
        run["turns"]=json!(1);
        run["pending"]=json!({"name":"file_read","call_id":"call-interrupted-read","state":"executing"});
        let policy=workflow::Policy::resolve(None).unwrap();
        workflow::record(&mut run,&policy,&workflow::Input::PreTool(workflow::PreToolInput {
            name:"file_read".into(),arguments_digest:"fixture".into(),policy_version:1,
            mode_revision:0,visibility_revision:0,ability_state_revision:0,
        }),workflow::Decision::Pass).unwrap();
        app.store.save_run_observed(&mut run,"model_output",&json!({"output":[call]})).await.unwrap();

        let result=record_interrupted_tool(&mut run,"file_read").unwrap().unwrap();
        app.store.save_run_observed(&mut run,"tool_result",
            &json!({"run_id":rid,"name":"file_read","result":result})).await.unwrap();

        let context=app.store.context(&sid).await.unwrap();
        let call_index=context.iter().position(|item|item["call_id"]=="call-interrupted-read").unwrap();
        assert_eq!(context[call_index]["type"],"function_call");
        assert_eq!(context[call_index+1]["type"],"function_call_output");
        assert_eq!(context[call_index+1]["call_id"],"call-interrupted-read");
        assert!(context[call_index+1]["output"].as_str().unwrap().contains("no data was returned"));
        assert!(context_compaction::plan_manual(&context,262144,
            &context_compaction::Config::configured().unwrap()).is_ok());
    }
}

#[cfg(test)]
mod messaging_future_stack_tests {
    use super::*;
    fn future_bytes<A,F:std::future::Future>(_:impl FnOnce(A)->F)->usize {
        std::mem::size_of::<F>()
    }
    #[test]
    fn run_future_has_bounded_inline_storage() {
        let bytes=future_bytes(|app| execute_run_inner(app,String::new(),String::new(),CancellationToken::new()));
        assert!(bytes <= 65536,"execute_run_inner future inline bytes: {bytes}");
    }
}
