//! The root resolves existing agent pools; Access owns capability authority and persistence.
pub use crate::access::capabilities::*;
#[cfg(test)]
use crate::access::{self, Permission, Relationship};
use crate::store::Store;
use anyhow::Result;
#[cfg(test)]
use serde_json::json;
use serde_json::Value;
impl Store {
    pub async fn capability_levels(&self, agent: &str) -> Result<CapabilityLevels> {
        let view = self.for_agent(agent).await?;
        view.access_repository().capability_levels(agent).await
    }
    pub async fn save_capability_level(
        &self,
        agent: &str,
        level: CapabilityLevel,
        expected: u64,
    ) -> Result<CapabilityLevels> {
        level.validate()?;
        let view = self.for_agent(agent).await?;
        view.access_repository()
            .save_capability_level(agent, level, expected)
            .await
    }
    pub async fn assign_capability_level(
        &self,
        actor: &str,
        user: &str,
        agent: &str,
        level: &str,
        expected: u64,
    ) -> Result<Value> {
        crate::access::capabilities::validate_level_id(level)?;
        let view = self.for_agent(agent).await?;
        view.access_repository()
            .assign_capability_level(actor, user, agent, level, expected)
            .await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use webagent_accounts_host::AccountsStore;
    #[test]
    fn unsaved_levels_are_bounded_and_follow_tool_json() {
        let levels = CapabilityLevels::from_metadata(&json!({})).unwrap();
        assert_eq!(levels.levels.len(), 3);
        for level in levels.levels.values() {
            assert_eq!(level.ability_permission("codebase_admin"), Permission::Auto);
            assert_eq!(level.ability_permission("web_access"), Permission::Auto);
            assert_eq!(level.ability_permission("new_plugin"), Permission::Auto);
        }
        assert!(CapabilityLevels::from_metadata(
            &json!({"capability_levels":{"revision":1,"levels":{}}})
        )
        .is_err());
    }
    #[tokio::test]
    async fn capability_changes_narrow_access_and_survive_reopen() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .create_admin_account("admin", "hash", "generation")
            .await
            .unwrap();
        crate::runtime_tests::grant_fixture_levels(&store, "shared_default").await;
        let before = store.live_access("admin", "shared_default").await.unwrap();
        // Narrow from the bottom up: a save never leaves a lower level above a higher one.
        let mut saved = store.capability_levels("shared_default").await.unwrap();
        for id in ["visitor", "member", "operator"] {
            let mut level = saved.levels[id].clone();
            // A level narrows an ability by switching it off.
            level.ability_exposure.insert(
                "codebase_admin".into(),
                crate::access::capabilities::Exposure::Off,
            );
            saved = store
                .save_capability_level("shared_default", level, saved.revision)
                .await
                .unwrap();
        }
        let after = store.live_access("admin", "shared_default").await.unwrap();
        assert_ne!(before.context_fingerprint(), after.context_fingerprint());
        assert_eq!(after.revision.capability_level, saved.revision);
        let decide = |access: &crate::access_policy::LiveAccess| {
            access.tool_decision_for_abilities("file_read", &["codebase_admin".into()], true, true)
        };
        assert_eq!(decide(&before).permission, Permission::Auto);
        assert_eq!(decide(&after).permission, Permission::Deny);
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            decide(
                &reopened
                    .live_access("admin", "shared_default")
                    .await
                    .unwrap()
            )
            .permission,
            Permission::Deny
        );
        // Administrator abilities also need an administrator caller.
        let user = store
            .create_user_account("member_x", "x".repeat(24).as_str())
            .await
            .unwrap();
        let id = user["user_id"].as_str().unwrap();
        store
            .update_account_access(id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        store
            .set_agent_membership(id, "shared_default", crate::access::Relationship::Member, 0)
            .await
            .unwrap();
        assert_eq!(
            decide(&store.live_access(id, "shared_default").await.unwrap()).reason,
            access::Reason::CallerDenied
        );
    }
    #[tokio::test]
    async fn combined_base_permissions_reject_stale_writes_without_partial_replacement() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .create_admin_account("admin", "hash", "generation")
            .await
            .unwrap();
        crate::runtime_tests::grant_fixture_levels(&store, "shared_default").await;
        let revision = store
            .agent_ability_permissions("shared_default")
            .await
            .unwrap()["revision"]
            .as_u64()
            .unwrap();
        store
            .set_agent_permissions(
                "shared_default",
                revision,
                json!({"codebase_admin":"ask"}),
                json!({"file_read":"deny"}),
            )
            .await
            .unwrap();
        assert!(store
            .set_agent_permissions(
                "shared_default",
                revision,
                json!({"codebase_admin":"auto"}),
                json!({"file_read":"auto"})
            )
            .await
            .is_err());
        let access = store.live_access("admin", "shared_default").await.unwrap();
        assert_eq!(access.ability_permission("codebase_admin"), Permission::Ask);
        assert_eq!(
            access
                .tool_decision_for_abilities("file_read", &["codebase_admin".into()], true, true)
                .reason,
            access::Reason::AgentDenied
        );
    }
    #[tokio::test]
    async fn level_writes_are_agent_owned_revisioned_and_durable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .create_admin_account("admin", "hash", "generation")
            .await
            .unwrap();
        let mut level = store
            .capability_levels("shared_default")
            .await
            .unwrap()
            .levels["member"]
            .clone();
        level
            .ability_permissions
            .insert("codebase_admin".into(), Permission::Deny);
        let saved = store
            .save_capability_level("shared_default", level.clone(), 0)
            .await
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert!(store
            .save_capability_level("shared_default", level, 0)
            .await
            .is_err());
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.capability_levels("shared_default").await.unwrap(),
            saved
        );
    }
}
