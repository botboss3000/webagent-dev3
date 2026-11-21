//! Root composition delegates Models domain and persistence to its independently compiled package.
use crate::store::Store;
use anyhow::Result;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
pub use webagent_models::model_selection::*;
impl Store {
    pub async fn save_model_catalog(
        &self,
        catalog: &Catalog,
        expected_revision: u64,
    ) -> Result<Catalog> {
        self.models_repository()
            .save_model_catalog(catalog, expected_revision)
            .await
    }
    pub async fn model_catalog(&self) -> Result<Catalog> {
        self.models_repository().model_catalog().await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    const CATALOG_KEY: &str = "model_selection:catalog:v1";
    async fn prepared(store: &Store) -> &Store {
        let owner: Option<String> =
            sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id='agent-a'")
                .fetch_optional(&store.app_pool)
                .await
                .unwrap();
        let owner = owner.unwrap_or_else(|| "owner".into());
        let pool = crate::agent_storage::open_agent_pool(&store.root, "agent-a")
            .await
            .unwrap();
        sqlx::query("INSERT OR IGNORE INTO agents VALUES('agent-a','Agent','','openai_subscription','','active',?,1,1,1)")
            .bind(store.seal(&json!({"owner_id":owner})).unwrap()).execute(&pool).await.unwrap();
        sqlx::query("INSERT OR IGNORE INTO agent_directory VALUES('agent-a',?)")
            .bind(owner)
            .execute(&store.app_pool)
            .await
            .unwrap();
        pool.close().await;
        store.for_agent("agent-a").await.unwrap();
        store
    }

    /// Roster a, b, c; Standard is a with aliases c then b.
    fn catalog_fixture() -> Catalog {
        let mut catalog = Catalog::default();
        for id in ["a", "b", "c"] {
            catalog.connections.insert(
                id.into(),
                Connection {
                    id: id.into(),
                    owner_id: "owner".into(),
                    transport: Transport::OpenaiSubscription,
                    credential_ref: format!("credential:{id}"),
                    allowed_agents: BTreeSet::from(["agent-a".into()]),
                    revision: 1,
                    revoked: false,
                },
            );
            catalog.entries.insert(
                id.into(),
                ModelEntry {
                    id: id.into(),
                    owner_id: "owner".into(),
                    connection_id: id.into(),
                    upstream_model: "same-upstream".into(),
                    display_name: id.into(),
                    capabilities: BTreeSet::from(["text".into()]),
                    allowed_agents: BTreeSet::from(["agent-a".into()]),
                    revision: 1,
                },
            );
        }
        catalog.agent_models.insert(
            "agent-a".into(),
            AgentModels {
                retired_limits: Default::default(),
                efforts: BTreeMap::new(),
                agent_id: "agent-a".into(),
                owner_id: "owner".into(),
                entry_ids: vec!["a".into(), "b".into(), "c".into()],
                roles: BTreeMap::from([(ModelRole::Standard, RoleAssignment::new("a"))]),
                aliases: BTreeMap::from([("a".into(), vec!["c".into(), "b".into()])]),
                revision: 1,
            },
        );
        catalog
    }

    #[tokio::test]
    async fn role_edits_are_atomic_revisioned_and_revocation_does_not_block_clearing() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        let mut catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog_fixture(), 0)
            .await
            .unwrap();
        let pinned = catalog.resolve("agent-a").unwrap();
        catalog
            .connections
            .get_mut("a")
            .unwrap()
            .allowed_agents
            .clear();
        catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog, 1)
            .await
            .unwrap();
        assert!(pinned
            .authorize_candidate(0, &catalog.entries, &catalog.connections)
            .is_err());
        let mut invalid = catalog.clone();
        invalid
            .agent_models
            .get_mut("agent-a")
            .unwrap()
            .roles
            .insert(ModelRole::Premium, RoleAssignment::new("a"));
        assert!(prepared(&store)
            .await
            .save_model_catalog(&invalid, 2)
            .await
            .is_err());
        assert_eq!(
            prepared(&store).await.model_catalog().await.unwrap(),
            catalog
        );
        catalog
            .agent_models
            .get_mut("agent-a")
            .unwrap()
            .roles
            .clear();
        let saved = prepared(&store)
            .await
            .save_model_catalog(&catalog, 2)
            .await
            .unwrap();
        assert_eq!(saved.agent_models["agent-a"].revision, 2);
        assert_eq!(saved.entries["a"].revision, 1);
        assert!(saved.resolve("agent-a").is_err());
        assert!(prepared(&store)
            .await
            .save_model_catalog(&catalog, 2)
            .await
            .is_err());
        let mut forged = saved.clone();
        forged.agent_models.get_mut("agent-a").unwrap().owner_id = "other".into();
        assert!(prepared(&store)
            .await
            .save_model_catalog(&forged, saved.revision)
            .await
            .is_err());
    }

    // Saving a roster without one of its models also drops that model from its aliases.
    #[tokio::test]
    async fn saved_aliases_follow_the_roster() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        let mut catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog_fixture(), 0)
            .await
            .unwrap();
        let ids = |c: &Catalog| {
            c.resolve("agent-a")
                .unwrap()
                .candidates
                .iter()
                .map(|x| x.entry_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&catalog), vec!["a", "c", "b"]);
        let roster = catalog.agent_models.get_mut("agent-a").unwrap();
        roster
            .aliases
            .insert("a".into(), vec!["b".into(), "c".into()]);
        catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog, 1)
            .await
            .unwrap();
        assert_eq!(ids(&catalog), vec!["a", "b", "c"]);
        catalog
            .agent_models
            .get_mut("agent-a")
            .unwrap()
            .entry_ids
            .retain(|id| id != "b");
        catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog, 2)
            .await
            .unwrap();
        assert_eq!(catalog.agent_models["agent-a"].aliases["a"], vec!["c"]);
        assert_eq!(
            prepared(&store).await.model_catalog().await.unwrap(),
            catalog
        );
    }

    #[tokio::test]
    async fn encrypted_catalog_round_trip_and_revision_conflict() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        assert_eq!(
            prepared(&store).await.model_catalog().await.unwrap(),
            Catalog::default()
        );
        let saved = prepared(&store)
            .await
            .save_model_catalog(&catalog_fixture(), 0)
            .await
            .unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(prepared(&store).await.model_catalog().await.unwrap(), saved);
        assert!(prepared(&store)
            .await
            .save_model_catalog(&catalog_fixture(), 0)
            .await
            .is_err());
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(CATALOG_KEY)
            .fetch_one(&store.app_pool)
            .await
            .unwrap();
        assert!(!raw
            .windows(b"same-upstream".len())
            .any(|x| x == b"same-upstream"));
        let mut changed = saved.clone();
        changed.entries.get_mut("a").unwrap().display_name = "Renamed".into();
        let updated = prepared(&store)
            .await
            .save_model_catalog(&changed, 1)
            .await
            .unwrap();
        assert_eq!(updated.entries["a"].revision, 2);
        assert_eq!(updated.entries["b"].revision, 1);
        store.app_pool.close().await;
        store.pool.close().await;
        store.agent_pool.close().await;
        let reopened = crate::store::Store::open(root.path()).await.unwrap();
        assert_eq!(
            prepared(&reopened).await.model_catalog().await.unwrap(),
            updated
        );
    }
    #[tokio::test]
    async fn revocation_allowed_but_new_assignment_and_owner_transfer_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        let mut catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog_fixture(), 0)
            .await
            .unwrap();
        catalog.connections.get_mut("a").unwrap().revoked = true;
        catalog = prepared(&store)
            .await
            .save_model_catalog(&catalog, 1)
            .await
            .unwrap();
        assert!(catalog.resolve("agent-a").is_err());
        let mut transfer = catalog.clone();
        transfer.entries.get_mut("a").unwrap().owner_id = "other-owner".into();
        assert!(prepared(&store)
            .await
            .save_model_catalog(&transfer, 2)
            .await
            .is_err());
        // A new roster may not use models that were never shared with its agent.
        let mut roster = catalog.agent_models["agent-a"].clone();
        roster.agent_id = "agent-b".into();
        roster.revision = 1;
        catalog.agent_models.insert("agent-b".into(), roster);
        assert!(prepared(&store)
            .await
            .save_model_catalog(&catalog, 2)
            .await
            .is_err());
        assert_eq!(
            prepared(&store)
                .await
                .model_catalog()
                .await
                .unwrap()
                .revision,
            2
        );
    }
}
