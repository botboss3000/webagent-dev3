//! The root resolves existing agent pools; Access owns capability authority and persistence.
use crate::store::Store;
use anyhow::Result;
#[cfg(test)] use crate::access::{self,Permission,Relationship};
#[cfg(test)] use serde_json::json;
use serde_json::Value;
pub use crate::access::capabilities::*;
impl Store {
    pub async fn capability_levels(&self,agent:&str)->Result<CapabilityLevels> { let view=self.for_agent(agent).await?; view.access_repository().capability_levels(agent).await }
    pub async fn save_capability_level(&self,agent:&str,level:CapabilityLevel,expected:u64)->Result<CapabilityLevels> { level.validate()?; let view=self.for_agent(agent).await?; view.access_repository().save_capability_level(agent, level, expected).await }
    pub async fn assign_capability_level(&self,actor:&str,user:&str,agent:&str,level:&str,expected:u64)->Result<Value> { crate::access::capabilities::validate_level_id(level)?; let view=self.for_agent(agent).await?; view.access_repository().assign_capability_level(actor, user, agent, level, expected).await }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn legacy_projection_is_bounded_and_does_not_grant_new_plugins() {
        let levels=CapabilityLevels::from_metadata(&json!({})).unwrap();
        assert_eq!(levels.levels.len(),3);
        for level in levels.levels.values() {
            assert_eq!(level.ability_permission("codebase_admin"),Permission::Deny);
            assert_eq!(level.ability_permission("web_access"),Permission::Deny);
            assert_eq!(level.ability_permission("new_plugin"),Permission::Deny);
        }
        assert!(CapabilityLevels::from_metadata(&json!({"capability_levels":{"revision":1,"levels":{}}})).is_err());
    }
    #[tokio::test]
    async fn capability_changes_narrow_access_and_survive_reopen() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        crate::runtime_tests::grant_fixture_levels(&store,"shared_default").await;
        let before=store.live_access("admin","shared_default").await.unwrap();
        let mut level=before.capability_level.clone();
        level.ability_permissions.insert("codebase_admin".into(),Permission::Deny);
        let saved=store.save_capability_level("shared_default",level,3).await.unwrap();
        let after=store.live_access("admin","shared_default").await.unwrap();
        assert_ne!(before.context_fingerprint(),after.context_fingerprint());
        assert_eq!(after.revision.capability_level,saved.revision);
        let decide=|access:&crate::access_policy::LiveAccess|access.tool_decision_for_abilities("file_read",&["codebase_admin".into()],Permission::Auto,true,true,false,true);
        assert_eq!(decide(&before).permission,Permission::Auto);
        assert_eq!(decide(&after).reason,access::Reason::CapabilityDenied);
        let reopened=Store::open(root.path()).await.unwrap();
        assert_eq!(decide(&reopened.live_access("admin","shared_default").await.unwrap()).permission,Permission::Deny);
        let mut level=after.capability_level.clone();level.ability_permissions.insert("codebase_admin".into(),Permission::Ask);
        store.save_capability_level("shared_default",level,4).await.unwrap();
        assert_eq!(decide(&store.live_access("admin","shared_default").await.unwrap()).permission,Permission::Ask);
        let mut tier=store.entitlement_tier("operator").await.unwrap().unwrap();
        tier.ability_permissions.insert("codebase_admin".into(),Permission::Deny);
        let revision=tier.revision;store.save_entitlement_tier(tier,revision).await.unwrap();
        assert_eq!(decide(&store.live_access("admin","shared_default").await.unwrap()).reason,access::Reason::TierDenied);
    }
    #[tokio::test]
    async fn combined_base_permissions_reject_stale_writes_without_partial_replacement() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        crate::runtime_tests::grant_fixture_levels(&store,"shared_default").await;
        let revision=store.agent_ability_permissions("shared_default").await.unwrap()["revision"].as_u64().unwrap();
        store.set_agent_permissions("shared_default",revision,json!({"codebase_admin":"ask"}),json!({"file_read":"deny"})).await.unwrap();
        assert!(store.set_agent_permissions("shared_default",revision,json!({"codebase_admin":"auto"}),json!({"file_read":"auto"})).await.is_err());
        let access=store.live_access("admin","shared_default").await.unwrap();
        assert_eq!(access.ability_permission("codebase_admin"),Permission::Ask);
        assert_eq!(access.tool_decision_for_abilities("file_read",&["codebase_admin".into()],Permission::Auto,true,true,false,true).reason,access::Reason::AgentDenied);
    }
    #[tokio::test]
    async fn level_writes_are_agent_owned_revisioned_and_durable() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        let mut level=store.capability_levels("shared_default").await.unwrap().levels["member"].clone();
        level.ability_permissions.insert("codebase_admin".into(),Permission::Deny);
        let saved=store.save_capability_level("shared_default",level.clone(),0).await.unwrap();
        assert_eq!(saved.revision,1);assert!(store.save_capability_level("shared_default",level,0).await.is_err());
        let reopened=Store::open(root.path()).await.unwrap();assert_eq!(reopened.capability_levels("shared_default").await.unwrap(),saved);
    }
}
