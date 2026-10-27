//! Root composition: Access owns the decisions, SQL and revisions.
use crate::store::Store;
use crate::access::{self,Permission,Relationship};
#[cfg(test)] use crate::access::{AccountStatus,Principal,Tier,ToolDecision,ToolFactors};
#[cfg(test)] use serde_json::json;
use anyhow::Result;
use serde_json::Value;
pub use crate::access::authority::{AccessRevision,LiveAccess,permission_map,caller_map};
impl Store {
    pub async fn live_access(&self, user_id: &str, agent_id: &str) -> Result<LiveAccess> { crate::access::authority::validate_live_access(user_id, agent_id)?; let view=self.for_agent(agent_id).await?; view.access_repository().live_access(user_id, agent_id).await }
    pub async fn set_agent_permissions(&self,agent:&str,expected:u64,abilities:Value,tools:Value)->Result<Value> { crate::access::authority::validate_set_agent_permissions(agent, expected, &abilities, &tools)?; let view=self.for_agent(agent).await?; view.access_repository().set_agent_permissions(agent, expected, abilities, tools).await }
    pub async fn agent_tool_permissions(&self, agent_id: &str) -> Result<Value> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_tool_permissions(agent_id).await }
    pub async fn set_agent_tool_permissions(&self, agent_id: &str,
        expected_revision: u64,
        permissions: std::collections::BTreeMap<String, Permission>) -> Result<Value> { crate::access::authority::validate_set_agent_tool_permissions(agent_id, expected_revision, &permissions)?; let view=self.for_agent(agent_id).await?; view.access_repository().set_agent_tool_permissions(agent_id, expected_revision, permissions).await }
    pub async fn agent_ability_permissions(&self,agent_id:&str) -> Result<Value> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_ability_permissions(agent_id).await }
    pub async fn set_agent_ability_permissions(&self,agent_id:&str,
        expected_revision:u64,
        permissions:std::collections::BTreeMap<String,Permission>) -> Result<Value> { crate::access::authority::validate_set_agent_ability_permissions(agent_id, expected_revision, &permissions)?; let view=self.for_agent(agent_id).await?; view.access_repository().set_agent_ability_permissions(agent_id, expected_revision, permissions).await }
    pub async fn agent_tool_callers(&self, agent_id: &str) -> Result<Value> { let view=self.for_agent(agent_id).await?; view.access_repository().agent_tool_callers(agent_id).await }
    pub async fn set_agent_tool_callers(&self,agent_id:&str,expected_revision:u64,
        callers:std::collections::BTreeMap<String,Vec<Relationship>>) -> Result<Value> { crate::access::authority::validate_set_agent_tool_callers(agent_id, expected_revision, &callers)?; let view=self.for_agent(agent_id).await?; view.access_repository().set_agent_tool_callers(agent_id, expected_revision, callers).await }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{AccountStatus, Reason, Role};

    #[test]
    fn malformed_permission_and_caller_maps_fail_closed() {
        assert!(permission_map(&serde_json::json!({"file_read":true})).is_err());
        assert!(permission_map(&serde_json::json!({"../file_read":"auto"})).is_err());
        assert!(caller_map(&serde_json::json!({"file_read":["owner","owner"]})).is_err());
        assert!(caller_map(&serde_json::json!({"file_read":["revoked"]})).is_err());
    }

    #[tokio::test]
    async fn new_ability_is_not_granted_by_legacy_tier_or_membership() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        // Unsaved capability levels grant nothing, even to the original administrator.
        let unset=store.live_access("admin","shared_default").await.unwrap();
        for id in ["codebase_admin","web_access","new_plugin"] {assert_eq!(unset.ability_permission(id),Permission::Deny);}
        crate::runtime_tests::grant_fixture_levels(&store,"shared_default").await;
        let access=store.live_access("admin","shared_default").await.unwrap();
        assert_eq!(access.ability_permission("codebase_admin"),Permission::Auto);
        assert_eq!(access.ability_permission("web_access"),Permission::Auto);
        assert_eq!(access.ability_permission("new_plugin"),Permission::Deny);
    }

    #[tokio::test]
    async fn malformed_agent_metadata_fails_closed() {
        let root=tempfile::tempdir().unwrap();
        let store=Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(store.seal(&serde_json::json!("malformed")).unwrap())
            .execute(&store.agent_pool).await.unwrap();
        assert!(store.live_access("admin","shared_default").await.is_err());
    }

    #[tokio::test]
    async fn live_access_restricts_pending_and_rechecks_revisions() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store.create_admin_account("admin","hash","generation").await.unwrap();
        crate::runtime_tests::grant_fixture_levels(&store,"shared_default").await;
        let admin = store.live_access("admin","shared_default").await.unwrap();
        // CONTEXT-FINGERPRINT-REGRESSION: grants invalidate even without revision changes.
        let original:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
            .fetch_one(&store.agent_pool).await.unwrap();
        let mut metadata=store.unseal(&original).unwrap();
        metadata["tool_permissions"]=serde_json::json!({"file_read":"deny"});
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(store.seal_agent(&metadata).unwrap()).execute(&store.agent_pool).await.unwrap();
        let changed=store.live_access("admin","shared_default").await.unwrap();
        sqlx::query("UPDATE agents SET metadata=? WHERE id='shared_default'")
            .bind(original).execute(&store.agent_pool).await.unwrap();
        assert_ne!(admin.context_fingerprint(),changed.context_fingerprint());
        assert_eq!(admin.revision,changed.revision);
        assert!(admin.admitted());
        assert!(admin.model_allowed("gpt-6-luna"));
        assert_eq!(admin.tool_decision("file_write",Permission::Ask,
            true,true,false,false).permission,Permission::Ask);
        let original = store.agent_tool_permissions("shared_default").await.unwrap();
        let agent_revision = original["revision"].as_u64().unwrap();
        let permissions = [("file_write".to_owned(),Permission::Deny)].into();
        store.set_agent_tool_permissions("shared_default",agent_revision,permissions)
            .await.unwrap();
        let tightened = store.live_access("admin","shared_default").await.unwrap();
        assert_ne!(admin.revision,tightened.revision);
        assert_eq!(tightened.tool_decision("file_write",Permission::Ask,
            true,true,false,false).reason,Reason::AgentDenied);
        assert!(store.set_agent_tool_permissions("shared_default",agent_revision,
            Default::default()).await.is_err());
        sqlx::query("UPDATE agents SET status='disabled',revision=revision+1 WHERE id='shared_default'")
            .execute(&store.agent_pool).await.unwrap();
        assert!(!store.live_access("admin","shared_default").await.unwrap().admitted());
        sqlx::query("UPDATE agents SET status='active',revision=revision+1 WHERE id='shared_default'")
            .execute(&store.agent_pool).await.unwrap();
        let agent_revision=store.agent_ability_permissions("shared_default").await.unwrap()
            ["revision"].as_u64().unwrap();
        store.set_agent_ability_permissions("shared_default",agent_revision,
            [("codebase_admin".to_owned(),Permission::Deny)].into())
            .await.unwrap();
        let denied=store.live_access("admin","shared_default").await.unwrap();
        assert_eq!(denied.tool_decision_for_abilities("file_read",
            &["codebase_admin".to_owned()],Permission::Auto,true,true,false,true)
            .reason,Reason::CapabilityDenied);
        store.set_agent_ability_permissions("shared_default",agent_revision+1,
            [("codebase_admin".to_owned(),Permission::Ask)].into())
            .await.unwrap();
        let ask=store.live_access("admin","shared_default").await.unwrap();
        assert_eq!(ask.tool_decision_for_abilities("file_read",
            &["codebase_admin".to_owned()],Permission::Auto,true,true,false,true)
            .reason,Reason::ApprovalRequired);
        store.set_agent_ability_permissions("shared_default",agent_revision+2,
            Default::default()).await.unwrap();
        let user = store.create_user_account("second","x".repeat(24).as_str())
            .await.unwrap();
        let id = user["user_id"].as_str().unwrap();
        store.set_agent_membership(id,"shared_default",Relationship::Caller,0)
            .await.unwrap();
        let pending = store.live_access(id,"shared_default").await.unwrap();
        assert!(!pending.admitted());
        assert_eq!(pending.tool_decision("file_read",Permission::Auto,
            true,true,false,true).reason,Reason::TierDenied);
        store.update_account_access(id,1,AccountStatus::Active,Role::User,"basic")
            .await.unwrap();
        let active = store.live_access(id,"shared_default").await.unwrap();
        assert!(active.admitted());
        assert!(!active.model_allowed("gpt-6-luna"));
        assert_ne!(pending.revision,active.revision);
        let mut tier = store.entitlement_tier("basic").await.unwrap().unwrap();
        tier.allowed_models.push("gpt-6-luna".into());
        tier.tools.insert("file_read".into(),Permission::Auto);
        store.save_entitlement_tier(tier,1).await.unwrap();
        let granted = store.live_access(id,"shared_default").await.unwrap();
        assert!(granted.model_allowed("gpt-6-luna"));
        assert_eq!(granted.tool_decision("file_read",Permission::Auto,
            true,true,false,true).permission,Permission::Auto);
        assert_ne!(active.revision,granted.revision);
        let current=store.agent_tool_callers("shared_default").await.unwrap();
        let agent_revision=current["revision"].as_u64().unwrap();
        let callers=[("file_read".to_owned(),
            vec![Relationship::Owner,Relationship::Member])].into();
        store.set_agent_tool_callers("shared_default",agent_revision,callers)
            .await.unwrap();
        let caller=store.live_access(id,"shared_default").await.unwrap();
        assert_eq!(caller.tool_decision("file_read",Permission::Auto,
            true,true,false,true).reason,Reason::CallerDenied);
        store.set_agent_membership(id,"shared_default",Relationship::Member,1)
            .await.unwrap();
        let member=store.live_access(id,"shared_default").await.unwrap();
        assert_eq!(member.tool_decision("file_read",Permission::Auto,
            true,true,false,true).permission,Permission::Auto);
        store.set_agent_membership(id,"shared_default",Relationship::Revoked,2)
            .await.unwrap();
        let revoked = store.live_access(id,"shared_default").await.unwrap();
        assert!(!revoked.admitted());
        assert_ne!(granted.revision,revoked.revision);
        assert_eq!(revoked.tool_decision("file_read",Permission::Auto,
            true,true,false,true).reason,Reason::CallerDenied);
    }
}

#[cfg(test)]
mod scope_validation_regression {
    use super::*;
    #[tokio::test]
    async fn owner_validation_precedes_agent_scope_io() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        assert_eq!(store.live_access("invalid/user","missing_agent").await.unwrap_err().to_string(),"Invalid access identity");
        assert_eq!(store.set_agent_permissions("missing_agent",0,serde_json::json!([]),serde_json::json!({})).await.unwrap_err().to_string(),"Invalid permission map");
        assert_eq!(store.set_agent_membership("admin","shared_default",Relationship::Revoked,0).await.unwrap_err().to_string(),"The original administrator must retain its agent");
        assert_eq!(store.assign_agent_membership("invalid/actor","admin","missing_agent",Relationship::Member,false,0).await.unwrap_err().to_string(),"Invalid actor identity");
    }
}
