//! Root Action/read-classification composition for Engine-owned settings and one shared limiter.
use anyhow::Result;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
pub use webagent_engine::tools::execution::{background_capable,concurrent_capable,max_supported_concurrency,Permit};
#[derive(Clone,Debug,PartialEq,Eq)] pub(crate) struct NativeActions;
impl webagent_engine::tools::execution::ActionCatalog for NativeActions {
 type Protection=crate::actions::Protection;
 fn supported(name:&str)->bool {crate::actions::supported(name)}
 fn default_protection(name:&str)->Option<Self::Protection> {crate::actions::default_on(name).then_some(crate::actions::Protection::On)}
 fn is_read(name:&str)->bool {crate::tools::is_read(name)}
}
pub type Settings=webagent_engine::tools::execution::Settings<NativeActions>;
pub fn advanced_policies(name:&str)->Value {webagent_engine::tools::execution::advanced_policies::<NativeActions>(name)}
pub fn resolve(value:&Value,name:&str)->Result<Settings> {webagent_engine::tools::execution::resolve::<NativeActions>(value,name)}
pub fn guidance(value:&Value,names:&[String])->Result<String> {webagent_engine::tools::execution::guidance::<NativeActions>(value,names)}
pub async fn acquire(user:&str,agent:&str,name:&str,settings:&Settings,cancel:&CancellationToken)->Result<Permit> {
 webagent_engine::tools::execution::acquire(user,agent,name,settings,cancel).await
}
pub async fn acquire_with_budget(user:&str,agent:&str,name:&str,settings:&Settings,cancel:&CancellationToken,max_total:usize)->Result<Permit> {
 webagent_engine::tools::execution::acquire_with_budget(user,agent,name,settings,cancel,max_total).await
}
#[cfg(test)] mod tests {
 use serde_json::json;
 use super::*;
    #[test]
    fn advanced_policies_follow_dispatch_classification() {
        for schema in crate::tools::schemas() {
            let name = schema["name"].as_str().unwrap();
            let policies = advanced_policies(name);
            assert_eq!(policies.is_null(), crate::tools::is_read(name), "{name}");
            if !policies.is_null() {
                assert_eq!(policies["applies_in_auto"], true);
                assert_eq!(policies["retry"], "explicit_authorization");
                assert_eq!(policies["read_only_session"], "blocked");
            }
        }
        assert!(advanced_policies("file_read").is_null());
        assert_eq!(advanced_policies("shell")["classification"], "tool_name");
        assert_eq!(advanced_policies("unknown")["uncertain_outcome"], "review");
        for name in ["build_work", "v3_build_work"] {
            assert_eq!(advanced_policies(name)["build_finish"], "reconcile_existing_job");
        }
        assert!(advanced_policies("shell")["build_finish"].is_null());
    }

}

#[cfg(test)]
mod action_settings_tests {
    use serde_json::json;
    use super::*;
       #[test]
    fn protected_defaults_old_clients_and_strict_settings() {
        assert_eq!(resolve(&json!({}),"edit_own_prompt").unwrap().duplicate_protection,Some(crate::actions::Protection::On));
        let old=json!({"tool_execution":{"edit_own_prompt":{"mode":"foreground","max_concurrency":1,"on_run_end":"stop"}}});
        assert_eq!(resolve(&old,"edit_own_prompt").unwrap().duplicate_protection,Some(crate::actions::Protection::On));
        let mut off=old;off["tool_execution"]["edit_own_prompt"]["duplicate_protection"]=json!("off");
        assert_eq!(resolve(&off,"edit_own_prompt").unwrap().duplicate_protection,Some(crate::actions::Protection::Off));
        off["tool_execution"]["edit_own_prompt"]["duplicate_protection"]=json!("ignore");
        assert!(resolve(&off,"edit_own_prompt").is_err());
        assert!(resolve(&json!({"tool_execution":{"shell":{"mode":"foreground","max_concurrency":1,"on_run_end":"stop","duplicate_protection":"on"}}}),"shell").is_err());
        assert!(resolve(&json!({}),"shell").unwrap().duplicate_protection.is_none());
    }
}
