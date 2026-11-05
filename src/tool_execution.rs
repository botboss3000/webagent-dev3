//! Root Action/read-classification composition for Engine-owned settings and one shared limiter.
use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
pub use webagent_engine::tools::execution::{
    background_capable, concurrent_capable, max_supported_concurrency, Permit,
};
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NativeActions;
impl webagent_engine::tools::execution::ActionCatalog for NativeActions {
    type Protection = crate::actions::Protection;
    fn supported(name: &str) -> bool {
        crate::actions::supported(name)
    }
    fn default_protection(name: &str) -> Option<Self::Protection> {
        crate::actions::default_on(name).then_some(crate::actions::Protection::On)
    }
    fn is_read(name: &str) -> bool {
        crate::tools::is_read(name)
    }
    fn retry(name: &str) -> webagent_engine::tools::retry_guard::Retry {
        crate::tools::retry(name)
    }
}
pub type Settings = webagent_engine::tools::execution::Settings<NativeActions>;
pub fn advanced_policies(name: &str) -> Value {
    webagent_engine::tools::execution::advanced_policies::<NativeActions>(name)
}
pub fn resolve(value: &Value, name: &str) -> Result<Settings> {
    webagent_engine::tools::execution::resolve::<NativeActions>(value, name)
}
pub fn guidance(value: &Value, names: &[String]) -> Result<String> {
    webagent_engine::tools::execution::guidance::<NativeActions>(value, names)
}
pub async fn acquire(
    user: &str,
    agent: &str,
    name: &str,
    settings: &Settings,
    cancel: &CancellationToken,
) -> Result<Permit> {
    webagent_engine::tools::execution::acquire(user, agent, name, settings, cancel).await
}
pub async fn acquire_with_budget(
    user: &str,
    agent: &str,
    name: &str,
    settings: &Settings,
    cancel: &CancellationToken,
    max_total: usize,
) -> Result<Permit> {
    webagent_engine::tools::execution::acquire_with_budget(
        user, agent, name, settings, cancel, max_total,
    )
    .await
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn interrupted_parallel_writes_wait_for_review_only_when_high_risk() {
        let mut run = json!({"batch":[
            {"state":"executing","pending":{"name":"shell","call_id":"a","arguments":{"command":"x"}}},
            {"state":"executing","pending":{"name":"commit_and_push","call_id":"b","arguments":{}}}]});
        assert!(crate::parallel_batch::mark_uncertain_writes(&mut run));
        assert_eq!(run["batch"][0]["state"], "done");
        assert_eq!(run["batch"][0]["result"]["error_kind"], "outcome_unknown");
        assert_eq!(run["batch"][1]["state"], "uncertain");
        assert!(run["retry_guard"]["actions"]
            .as_object()
            .unwrap()
            .values()
            .any(|entry| entry["call_id"] == "a" && entry["state"] == "uncertain"));
        let mut low = json!({"batch":[
            {"state":"executing","pending":{"name":"shell","call_id":"a","arguments":{}}}]});
        assert!(!crate::parallel_batch::mark_uncertain_writes(&mut low));
    }
    #[test]
    fn repeat_policy_defaults_follow_tool_class() {
        use webagent_engine::tools::execution::ActionCatalog;
        use webagent_engine::tools::retry_guard::Risk;
        assert_eq!(NativeActions::retry("file_read").risk, Risk::None);
        assert_eq!(NativeActions::retry("set_closer_status").risk, Risk::Exempt);
        assert_eq!(
            NativeActions::retry("publish_main_message").risk,
            Risk::Exempt
        );
        assert_eq!(NativeActions::retry("edit_own_prompt").risk, Risk::High);
        assert_eq!(NativeActions::retry("shell").risk, Risk::Low);
        assert_eq!(NativeActions::retry("python").risk, Risk::Low);
        assert_eq!(NativeActions::retry("commit_and_push").risk, Risk::High);
        assert_eq!(NativeActions::retry("unknown_write").risk, Risk::High);
    }
    #[test]
    fn advanced_policies_follow_dispatch_classification() {
        for schema in crate::tools::schemas() {
            let name = schema["name"].as_str().unwrap();
            let policies = advanced_policies(name);
            assert_eq!(policies.is_null(), crate::tools::is_read(name), "{name}");
            if !policies.is_null() {
                assert_eq!(policies["applies_in_auto"], true);
                assert_eq!(policies["retry"], "repeat_guard");
                assert_eq!(policies["read_only_session"], "blocked");
            }
        }
        assert!(advanced_policies("file_read").is_null());
        assert_eq!(advanced_policies("shell")["classification"], "tool_name");
        assert_eq!(
            advanced_policies("unknown")["uncertain_outcome"],
            "returned_to_agent"
        );
        for name in ["build_work", "v3_build_work"] {
            assert_eq!(
                advanced_policies(name)["build_finish"],
                "reconcile_existing_job"
            );
        }
        assert!(advanced_policies("shell")["build_finish"].is_null());
    }
}

#[cfg(test)]
mod action_settings_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn retired_ledger_settings_are_ignored_not_fatal() {
        // These tools now use the repeat guard; settings saved for the old ledger are dropped.
        let off = json!({"tool_execution":{"edit_own_prompt":{"mode":"foreground",
            "max_concurrency":1,"on_run_end":"stop","duplicate_protection":"off"}}});
        assert!(resolve(&off, "edit_own_prompt")
            .unwrap()
            .duplicate_protection
            .is_none());
        assert_eq!(
            resolve(&json!({}), "test_owner_purchase")
                .unwrap()
                .duplicate_protection,
            Some(crate::actions::Protection::On)
        );
        let mut bad = off;
        bad["tool_execution"]["edit_own_prompt"]["duplicate_protection"] = json!("ignore");
        assert!(resolve(&bad, "edit_own_prompt").is_err());
        assert!(resolve(&json!({}), "shell")
            .unwrap()
            .duplicate_protection
            .is_none());
    }
}
