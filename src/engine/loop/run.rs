//! Thin role composition for the independently compiled Engine execution.
use super::*;
use crate::engine_runtime::Engine;
pub(crate) async fn execute_run(
    app: App,
    id: String,
    generation: String,
    cancel: CancellationToken,
) -> Result<()> {
    Engine::execute_run(app.engine(), id, generation, cancel).await
}
fn record_tool_interruption(run: &mut Value, reason: &str) {
    Engine::record_tool_interruption(run, reason)
}
fn record_interrupted_tool(run: &mut Value, name: &str) -> Result<Option<Value>> {
    Engine::record_interrupted_tool(run, name)
}
async fn preserve_restart_interruption(app: &App, run: &mut Value) -> Result<bool> {
    Engine::preserve_restart_interruption(&app.engine(), run).await
}
async fn commit_tool_transport_failure(app: &App, run: &mut Value, name: &str) -> Result<()> {
    Engine::commit_tool_transport_failure(&app.engine(), run, name).await
}
#[cfg(test)]
fn execute_run_inner(
    app: App,
    id: String,
    generation: String,
    cancel: CancellationToken,
) -> impl std::future::Future<Output = Result<()>> {
    Engine::fixture_execute_run_inner(app.engine(), id, generation, cancel)
}
#[cfg(test)]
mod interrupted_tool_tests {
    use super::*;

    fn pending(name: &str) -> Value {
        json!({"id":"run-1","session_id":"session-1","generation":"generation-1",
            "status":"running","turns":1,"call_index":0,"uncertain":false,
            "input":[{"type":"function_call","call_id":"call-1","name":name,"arguments":"{}"}],
            "pending":{"name":name,"call_id":"call-1","state":"executing"},
            "workflow":{"receipts":{"pre_tool":{"checkpoint":"pre_tool",
                "generation":"generation-1","policy_revision":"policy-1","input_digest":"digest",
                "turn":1,"call_index":0,"result":{"decision":"pass"}}}}})
    }

    #[test]
    fn interruption_reason_is_bounded_and_preserves_call() {
        for (reason, expected) in [
            ("timeout", "timeout"),
            ("cancelled", "cancelled"),
            ("transport_error", "transport_error"),
            ("PRIVATE_ERROR", "uncertain"),
        ] {
            let mut run = pending("shell");
            record_tool_interruption(&mut run, reason);
            assert_eq!(run["pending"]["interruption"]["reason"], expected);
            assert!(run["pending"]["interruption"]["recorded_at"]
                .as_i64()
                .is_some());
            assert_eq!(run["pending"]["call_id"], "call-1");
            assert!(!run.to_string().contains("PRIVATE_ERROR"));
            assert_eq!(run["input"].as_array().unwrap().len(), 1);
        }
    }
    #[test]
    fn interrupted_read_gets_matching_safe_result_and_fails_closed() {
        let mut run = pending("file_read");
        let result = record_interrupted_tool(&mut run, "file_read")
            .unwrap()
            .unwrap();
        assert_eq!(run["status"], "failed");
        assert_eq!(run["error"], "Read tool interrupted");
        assert!(run["pending"].is_null());
        assert_eq!(run["uncertain"], false);
        assert_eq!(result["status"], "error");
        assert!(result["output"]
            .as_str()
            .unwrap()
            .contains("no data was returned"));
        let input = run["input"].as_array().unwrap();
        assert_eq!(input.len(), 2);
        assert_eq!(input[0]["type"], "function_call");
        assert_eq!(input[1]["type"], "function_call_output");
        assert_eq!(input[0]["call_id"], input[1]["call_id"]);
        let receipt: Value = serde_json::from_str(input[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(receipt, result);
        let workflow_result = &run["workflow"]["receipts"]["tool_result"];
        assert_eq!(workflow_result["checkpoint"], "tool_result");
        assert!(!workflow_result["input_digest"]
            .as_str()
            .unwrap_or("")
            .is_empty());
    }

    #[test]
    fn interrupted_write_stays_uncertain_without_a_result_or_retry() {
        let mut run = pending("file_write");
        assert!(record_interrupted_tool(&mut run, "file_write")
            .unwrap()
            .is_none());
        assert_eq!(run["status"], "recovery");
        assert_eq!(run["uncertain"], true);
        assert_eq!(run["input"].as_array().unwrap().len(), 1);
        assert_eq!(run["pending"]["call_id"], "call-1");
        assert_eq!(run["workflow"]["receipts"]["tool_result"], Value::Null);
    }

    #[tokio::test]
    async fn interrupted_read_persists_a_paired_result_in_next_context() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let generation = uuid();
        let mut run = app.run(&rid).await.unwrap();
        assert!(app
            .store
            .claim_run(&mut run, "interrupted-read-test", &generation)
            .await
            .unwrap());

        let call = json!({"type":"function_call","call_id":"call-interrupted-read",
            "name":"file_read","arguments":"{}"});
        run["input"].as_array_mut().unwrap().push(call.clone());
        run["turns"] = json!(1);
        run["pending"] =
            json!({"name":"file_read","call_id":"call-interrupted-read","state":"executing"});
        let policy = workflow::Policy::resolve(None).unwrap();
        workflow::record(
            &mut run,
            &policy,
            &workflow::Input::PreTool(workflow::PreToolInput {
                name: "file_read".into(),
                arguments_digest: "fixture".into(),
                policy_version: 1,
                mode_revision: 0,
                visibility_revision: 0,
                ability_state_revision: 0,
            }),
            workflow::Decision::Pass,
        )
        .unwrap();
        app.store
            .save_run_observed(&mut run, "model_output", &json!({"output":[call]}))
            .await
            .unwrap();

        let result = record_interrupted_tool(&mut run, "file_read")
            .unwrap()
            .unwrap();
        app.store
            .save_run_observed(
                &mut run,
                "tool_result",
                &json!({"run_id":rid,"name":"file_read","result":result}),
            )
            .await
            .unwrap();

        let context = app.store.context(&sid).await.unwrap();
        let call_index = context
            .iter()
            .position(|item| item["call_id"] == "call-interrupted-read")
            .unwrap();
        assert_eq!(context[call_index]["type"], "function_call");
        assert_eq!(context[call_index + 1]["type"], "function_call_output");
        assert_eq!(context[call_index + 1]["call_id"], "call-interrupted-read");
        assert!(context[call_index + 1]["output"]
            .as_str()
            .unwrap()
            .contains("no data was returned"));
        assert!(context_compaction::plan_manual(
            &context,
            262144,
            &context_compaction::Config::configured().unwrap()
        )
        .is_ok());
    }
}

#[cfg(test)]
mod messaging_future_stack_tests {
    use super::*;
    fn future_bytes<A, F: std::future::Future>(_: impl FnOnce(A) -> F) -> usize {
        std::mem::size_of::<F>()
    }
    #[test]
    fn run_future_has_bounded_inline_storage() {
        let bytes = future_bytes(|app| {
            execute_run_inner(app, String::new(), String::new(), CancellationToken::new())
        });
        assert!(
            bytes <= 65536,
            "execute_run_inner future inline bytes: {bytes}"
        );
    }
}
