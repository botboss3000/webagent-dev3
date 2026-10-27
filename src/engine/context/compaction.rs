//! Root compatibility names for the Engine-owned compaction adapter.
pub use webagent_engine::context::compaction::*;
#[cfg(test)] use anyhow::Result;
#[cfg(test)] use serde_json::{json,Value};
#[cfg(test)] use sha2::{Digest,Sha256};

#[cfg(test)]
/// Keep legacy byte-budget characterization on the same shared planner as production policy.
fn plan(input: &[Value], max_bytes: usize, reserve_bytes: usize) -> Result<Option<Plan>> {
    let mut config = Config::configured()?;
    config.summary_reserve_bytes = reserve_bytes;
    plan_with_config(input, max_bytes, &config, false, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_tool_result_is_bounded_once_and_keeps_provenance() {
        let full = json!({"status":"success","output":"\\n".repeat(500_000)}).to_string();
        let hash = hex::encode(Sha256::digest(full.as_bytes()));
        let mut input = vec![json!({"type":"function_call_output","call_id":"big-search","output":full})];
        assert!(project_tool_results(&mut input, 8192).unwrap());
        let receipt = input[0]["output"].as_str().unwrap();
        assert!(receipt.len() <= 8192);
        let parsed: Value = serde_json::from_str(receipt).unwrap();
        assert_eq!(parsed["status"], "success");
        assert_eq!(parsed["output"]["sha256"], hash);
        assert_eq!(parsed["output"]["call_id"], "big-search");
        assert!(parsed["output"]["context_truncated"].as_bool().unwrap());
        let once = input.clone();
        assert!(!project_tool_results(&mut input, 8192).unwrap());
        assert_eq!(input, once);
    }

    #[test]
    fn loader_acknowledgments_pass_unchanged_to_replay_and_summary() {
        let config=Config::configured().unwrap();
        for name in ["load_ability","load_tool","load_skill"] {
            let output=json!({"status":"success","output":{"status":"loaded",
                "scope":"run","context_source":"live_skill_catalog"}}).to_string();
            let mut input=vec![
                json!({"type":"function_call","call_id":"load","name":name,"arguments":"{}"}),
                json!({"type":"function_call_output","call_id":"load","output":output})];
            let original=input.clone();
            assert!(!project_tool_results(&mut input,8192).unwrap());
            assert_eq!(input,original);
            assert!(config.prompt_source(&input,true).unwrap().contains("live_skill_catalog"));
        }
    }

    #[test]
    fn large_history_folds_in_stages_with_bounded_sources() {
        let mut config = rolling_config();
        config.max_source_bytes = 6000;
        let mut input = vec![user("long task"), call("first"),
            json!({"type":"function_call_output","call_id":"first","output":"x".repeat(3500)}),
            call("second"),
            json!({"type":"function_call_output","call_id":"second","output":"y".repeat(3500)}),
            user("finish")];
        let first = plan_rolling(&input, 4000, &config).unwrap().unwrap();
        assert!(serde_json::to_vec(&first.source).unwrap().len() <= config.max_source_bytes);
        input = apply_summary(&input, &first, "first search done").unwrap();
        let second = plan_rolling(&input, 4000, &config).unwrap().unwrap();
        assert!(serde_json::to_vec(&second.source).unwrap().len() <= config.max_source_bytes);
        input = apply_summary(&input, &second, "both searches done").unwrap();
        assert!(serde_json::to_vec(&input).unwrap().len() <= 4000);
    }

    #[test]
    fn reasoning_projection_is_idempotent_and_keeps_visible_content() {
        let source=vec![json!({"type":"reasoning","encrypted_content":"opaque"}),
            json!({"type":"message","role":"assistant","content":"answer",
                "deepseek_reasoning":"hidden"}),json!({"role":"user","content":"next"})];
        let once=recall_source(&source);
        assert_eq!(once.len(),2);
        assert_eq!(once[0]["content"],"answer");
        assert!(once[0].get("deepseek_reasoning").is_none());
        assert_eq!(recall_source(&once),once);
        assert_eq!(source[0]["encrypted_content"],"opaque");
    }

    fn user(text: &str) -> Value {
        json!({"role":"user","content":text})
    }
    fn assistant(text: &str) -> Value {
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":text}]})
    }
    fn call(id: &str) -> Value {
        json!({"type":"function_call","call_id":id,"name":"file_read","arguments":"{}"})
    }
    fn output(id: &str) -> Value {
        json!({"type":"function_call_output","call_id":id,"output":"{}"})
    }

    #[test]
    fn fitting_input_needs_no_compaction() {
        assert!(plan(&[user("hello")], 1024, 100).unwrap().is_none());
    }

    #[test]
    fn retains_complete_latest_tool_exchange() {
        let input = vec![
            user(&"old".repeat(120)),
            assistant("old answer"),
            user("new task"),
            call("c1"),
            output("c1"),
            assistant("new answer"),
        ];
        let latest = serde_json::to_vec(&input[2..]).unwrap().len();
        let max = latest + 160;
        let planned = plan(&input, max, 65).unwrap().unwrap();
        assert_eq!(planned.drop_until, 2);
        assert_eq!(planned.source, input[..2]);
        let compacted = apply_summary(&input, &planned, "Old request was completed.").unwrap();
        assert_eq!(&compacted[1..], &input[2..]);
        assert_eq!(compacted[0]["role"], "user");
        assert!(plan(&compacted, max, 65).unwrap().is_none());
    }

    #[test]
    fn does_not_split_unanswered_tool_call() {
        let input = vec![
            user(&"old".repeat(120)),
            call("c1"),
            user("new task"),
            assistant("answer"),
        ];
        assert!(plan(&input, 140, 40).is_err());
    }

    #[test]
    fn stale_plan_and_oversized_summary_are_rejected() {
        let input = vec![
            user(&"old".repeat(120)),
            assistant("done"),
            user("new task"),
        ];
        let planned = plan(&input, 180, 80).unwrap().unwrap();
        let mut changed = input.clone();
        changed[0] = user("changed");
        assert!(apply_summary(&changed, &planned, "memory").is_err());
        assert!(apply_summary(&input, &planned, "").is_err());
        assert!(apply_summary(&input, &planned, &"x".repeat(81)).is_err());
    }

    #[test]
    fn cannot_discard_the_only_user_turn() {
        assert!(plan(&[user(&"x".repeat(500))], 200, 50).is_err());
    }

    #[test]
    fn compacts_completed_work_within_one_user_task() {
        let input = vec![
            user("Investigate the issue and finish the repair"),
            call("first"),
            json!({"type":"function_call_output","call_id":"first","output":"x".repeat(2000)}),
            call("second"),
            output("second"),
        ];
        let planned = plan(&input, 700, 250).unwrap().unwrap();
        assert_eq!(planned.boundary, Boundary::CompletedToolExchange);
        assert_eq!(planned.drop_until, 3);
        let compacted = apply_summary(&input, &planned,
            "The issue is under investigation. First read completed; repair remains pending.").unwrap();
        assert_eq!(&compacted[1..], &input[3..]);
        assert!(compacted[0]["content"].as_str().unwrap().contains("Active task progress"));
        assert!(plan(&compacted, 700, 250).unwrap().is_none());
    }

    #[test]
    fn active_compaction_rejects_mismatched_tool_results() {
        let input = vec![user("Do work"), call("first"),
            json!({"type":"function_call_output","call_id":"first","output":"x".repeat(2000)}),
            call("second"),
            json!({"type":"function_call_output","call_id":"unknown","output":"bad"})];
        assert!(plan(&input, 700, 250).is_err());
    }
    fn rolling_config() -> Config {
        let mut c = Config::configured().unwrap();
        c.trigger_tokens = 150;
        c.tail_tokens = 40;
        c.summary_tokens = 20;
        c.summary_reserve_bytes = 300;
        c
    }

    #[test]
    fn system_budget_triggers_before_configured_history_threshold() {
        let mut c = Config::configured().unwrap();
        c.trigger_tokens = 150_000;
        c.tail_tokens = 40_000;
        c.summary_reserve_bytes = 300;
        let input = vec![user(&"old".repeat(500)), assistant("done"),
            user(&"recent".repeat(500)), assistant("done"), user("continue")];
        let budget = c.summary_envelope_bytes(&input[..2]).unwrap() + 100;
        assert!(plan_rolling(&input, 262144, &c).unwrap().is_none());
        c.system_input_budget = Some(budget);
        let plan = plan_rolling(&input, 262144, &c).unwrap().unwrap();
        assert_eq!(plan.drop_until, 2);
        assert!(c.summary_envelope_bytes(&plan.source).unwrap() <= budget);
        assert_eq!(c.trigger_tokens, 150_000);
        assert!(serde_json::to_value(&c).unwrap().get("system_input_budget").is_none());
    }

    #[test]
    fn system_budget_counts_json_escaping_and_manual_tool_boundaries() {
        let mut c = Config::configured().unwrap();
        c.summary_reserve_bytes = 300;
        let input = vec![user("work"), call("one"),
            json!({"type":"function_call_output","call_id":"one","output":"\"\\".repeat(300)}),
            call("two"),
            json!({"type":"function_call_output","call_id":"two","output":"\"\\".repeat(300)}),
            user("continue")];
        let budget = c.summary_envelope_bytes(&input[..3]).unwrap();
        c.system_input_budget = Some(budget);
        let plan = plan_manual(&input, 262144, &c).unwrap().unwrap();
        assert_eq!(plan.drop_until, 3);
        assert!(c.summary_envelope_bytes(&plan.source).unwrap() <= budget);
        c.system_input_budget = Some(budget - 1);
        assert!(plan_manual(&input, 262144, &c).is_err());
    }

    #[test]
    fn manual_fold_waits_for_a_safe_completed_exchange() {
        let c = rolling_config();
        let input = vec![user("old request"), assistant("old answer"), user("current request")];
        assert!(plan_rolling(&input, 4096, &c).unwrap().is_none());
        let requested = plan_manual(&input, 4096, &c).unwrap().unwrap();
        assert_eq!(requested.drop_until, 2);
        assert_eq!(&apply_summary(&input, &requested, "prior answer").unwrap()[1..], &input[2..]);
        assert!(plan_manual(&[user("only request")], 4096, &c).is_err());
    }
    #[test]
    fn rolling_trigger_is_independent_of_byte_ceiling_and_keeps_tail() {
        let c = rolling_config();
        let input = vec![user(&"old".repeat(250)), assistant("done"), user("latest")];
        assert!(plan(&input, 4096, 300).unwrap().is_none());
        let p = plan_rolling(&input, 4096, &c).unwrap().unwrap();
        assert_eq!(p.drop_until, 2);
        let result = apply_summary(&input, &p, "remember original goal").unwrap();
        assert_eq!(&result[1..], &input[2..]);
        let mut raised = c.clone(); raised.trigger_tokens = 500;
        assert!(plan_rolling(&input, 4096, &raised).unwrap().is_none());
    }
    #[test]
    fn second_fold_includes_previous_checkpoint_without_stacking_summaries() {
        let c = rolling_config();
        let input = vec![user(&"old".repeat(250)), assistant("done"), user("latest")];
        let p = plan_rolling(&input, 4096, &c).unwrap().unwrap();
        let mut first = apply_summary(&input, &p, "checkpoint one").unwrap();
        first.extend([assistant(&"aged".repeat(250)), user("newest")]);
        let p2 = plan_rolling(&first, 4096, &c).unwrap().unwrap();
        assert_eq!(p2.source[0], first[0]);
        assert!(p2.drop_until > 1);
        let second = apply_summary(&first, &p2, "checkpoint two").unwrap();
        assert_eq!(second.len(), 2);
        assert_eq!(second[1], user("newest"));
        assert!(!second[0].to_string().contains("checkpoint one"));
    }
    #[test]
    fn single_file_is_required_live_and_rejects_invalid_or_provider_fields() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("context-compaction.json");
        assert!(Config::read(&path).is_err());
        assert!(!path.exists());
        let mut config = Config::configured().unwrap();
        config.persist(&path).unwrap();
        config.summary_reserve_bytes = 20000;
        config.persist(&path).unwrap();
        assert_eq!(Config::read(&path).unwrap().summary_reserve_bytes, 20000);
        let mut value = serde_json::to_value(config).unwrap();
        value["provider"] = json!("not permitted");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(Config::read(&path).is_err());
        std::fs::write(&path, "{").unwrap();
        assert!(Config::read(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{");
    }
    #[test]
    fn snapshot_prompt_and_validation_limits_are_stable() {
        let mut c = rolling_config();
        let input = vec![user(&"old".repeat(250)), user("latest")];
        let p = plan_rolling(&input, 4096, &c).unwrap().unwrap();
        let prompt = c.prompt(&p).unwrap();
        assert!(prompt.contains("under 80 characters"));
        assert!(prompt.contains("Conversation JSON:"));
        c.summary_reserve_bytes = 1;
        // The plan captures its own immutable settings, not this later edit.
        assert!(apply_summary(&input, &p, "remember goal").is_ok());
        assert!(apply_summary(&input, &p, "").is_err());
        assert!(apply_summary(&input, &p, &"x".repeat(301)).is_err());
        c.max_source_bytes = 1;
        assert!(c.prompt(&p).is_err());
    }
    #[test]
    fn completed_exchange_remains_an_available_rolling_boundary() {
        let c = rolling_config();
        let input = vec![user(&"task".repeat(250)), call("first"), output("first"),
            call("last"), output("last")];
        let p = plan_rolling(&input, 4096, &c).unwrap().unwrap();
        assert_eq!(p.boundary, Boundary::CompletedToolExchange);
        let result = apply_summary(&input, &p, "completed first exchange").unwrap();
        assert_eq!(&result[1..], &input[p.drop_until..]);
    }

    #[test]
    fn cached_planner_matches_reference_with_unicode_and_append() {
        let mut config = rolling_config();
        config.tail_tokens = 100;
        let mut history = vec![user(&"é🌍".repeat(600)), assistant("done"), user("latest")];
        let mut state = crate::context_accounting::ContextAccounting::default();
        for _ in 0..3 {
            state.sync(&history).unwrap();
            let (sizes, characters) = state.measurements();
            let cached = plan_cached(&history, 4096, &config, sizes, characters).unwrap().unwrap();
            let reference = plan_rolling(&history, 4096, &config).unwrap().unwrap();
            assert_eq!(cached.drop_until, reference.drop_until);
            assert_eq!(cached.source_digest, reference.source_digest);
            let measured = state.measured_items;
            state.sync(&history).unwrap();
            assert_eq!(state.measured_items, measured);
            history.push(assistant("next"));
        }
    }

}

#[cfg(test)]
mod model_limit_regression {
 use super::*;
 #[test]
 fn summary_chunk_cap_does_not_replace_history_trigger() {
  let mut c=Config::configured().unwrap();
  c.system_input_budget=Some(3_688_000);
  let input=vec![json!({"role":"user","content":"x".repeat(270_000)}),
      json!({"role":"user","content":"continue"})];
  assert!(plan_rolling(&input,4_194_304,&c).unwrap().is_none());
  c.trigger_tokens=50_000;c.tail_tokens=10_000;
  let plan=plan_rolling(&input,4_194_304,&c);
  // A single oversized turn cannot be safely split; it must be reported rather than dropped.
  assert!(plan.is_err());
 }
}
