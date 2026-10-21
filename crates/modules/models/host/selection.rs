//! Root composition delegates Models domain and persistence to its independently compiled package.
pub use webagent_models::model_selection::*;
use crate::store::Store;
use anyhow::Result;
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
impl Store {
    pub async fn save_model_catalog(&self, catalog: &Catalog, expected_revision: u64) -> Result<Catalog> { self.models_repository().save_model_catalog(catalog, expected_revision).await }
    pub async fn model_catalog(&self) -> Result<Catalog> { self.models_repository().model_catalog().await }
}
#[cfg(test)]
mod tests {
    use super::*;
    const CATALOG_KEY: &str = "model_selection:catalog:v1";


    fn fixture() -> (Assignment, BTreeMap<String, ModelEntry>, BTreeMap<String, Connection>, BTreeMap<String, BackupGroup>) {
        let assignment = Assignment { agent_id: "agent-a".into(), owner_id: "owner".into(), primary_entry_id: "a".into(), backup_group_id: Some("group".into()), revision: 1 };
        let mut entries = BTreeMap::new(); let mut connections = BTreeMap::new();
        for id in ["a", "b", "c"] {
            connections.insert(id.into(), Connection { id: id.into(), owner_id: "owner".into(), transport: Transport::OpenaiSubscription,
                credential_ref: format!("credential:{id}"), allowed_agents: BTreeSet::from(["agent-a".into()]), revision: 1, revoked: false });
            entries.insert(id.into(), ModelEntry { id: id.into(), owner_id: "owner".into(), connection_id: id.into(), upstream_model: "same-upstream".into(),
                display_name: id.into(), capabilities: BTreeSet::from(["text".into()]), allowed_agents: BTreeSet::from(["agent-a".into()]), revision: 1 });
        }
        let groups = BTreeMap::from([("group".into(), BackupGroup { id: "group".into(), owner_id: "owner".into(), entry_ids: vec!["c".into(), "a".into(), "b".into()], revision: 3 })]);
        (assignment, entries, connections, groups)
    }
    fn catalog_fixture() -> Catalog {
        let (assignment, entries, connections, mut groups) = fixture();
        for group in groups.values_mut() { group.revision = 1; }
        Catalog { revision: 0, platform_sources: BTreeMap::new(), entries, connections, groups,
            assignments: BTreeMap::from([(assignment.agent_id.clone(), assignment)]), agent_models: BTreeMap::new() }
    }








    #[tokio::test]
    async fn role_edits_are_atomic_revisioned_and_revocation_does_not_block_clearing() {
        let root = tempfile::tempdir().unwrap(); let store = crate::store::Store::open(root.path()).await.unwrap();
        let mut catalog = store.save_model_catalog(&catalog_fixture(), 0).await.unwrap();
        let pinned = catalog.resolve("agent-a").unwrap();
        catalog.connections.get_mut("a").unwrap().allowed_agents.clear();
        catalog = store.save_model_catalog(&catalog, 1).await.unwrap();
        assert!(pinned.authorize_candidate(0, &catalog.entries, &catalog.connections).is_err());
        let mut invalid = catalog.clone();
        invalid.agent_models.get_mut("agent-a").unwrap().roles.insert(ModelRole::Premium,
            RoleAssignment { entry_id: "a".into(), backup_entry_ids: vec![] });
        assert!(store.save_model_catalog(&invalid, 2).await.is_err());
        assert_eq!(store.model_catalog().await.unwrap(), catalog);
        catalog.agent_models.get_mut("agent-a").unwrap().roles.clear();
        let saved = store.save_model_catalog(&catalog, 2).await.unwrap();
        assert_eq!(saved.agent_models["agent-a"].revision, 2);
        assert_eq!(saved.entries["a"].revision, 1);
        assert!(saved.resolve("agent-a").is_err());
        assert!(store.save_model_catalog(&catalog, 2).await.is_err());
        let mut forged = saved.clone(); forged.agent_models.get_mut("agent-a").unwrap().owner_id = "other".into();
        assert!(store.save_model_catalog(&forged, saved.revision).await.is_err());
    }

    // The old installer could configure a different owner's agent. Migration keeps
    // its grant authority but uses the canonical tenant owner for future role edits.
    #[tokio::test]
    async fn stored_legacy_roles_use_directory_owner_without_granting_its_private_connections() {
        let root = tempfile::tempdir().unwrap(); let store = crate::store::Store::open(root.path()).await.unwrap();
        sqlx::query("INSERT INTO agent_directory(id,owner_id) VALUES('agent-a','tenant-owner')")
            .execute(&store.app_pool).await.unwrap();
        let mut legacy = catalog_fixture();
        legacy.assignments.get_mut("agent-a").unwrap().owner_id = "admin".into();
        legacy.groups.get_mut("group").unwrap().owner_id = "admin".into();
        for entry in legacy.entries.values_mut() { entry.owner_id = "admin".into(); entry.allowed_agents.clear(); }
        for connection in legacy.connections.values_mut() { connection.owner_id = "admin".into(); connection.allowed_agents.clear(); }
        // An originally ungranted tenant-owned backup must not gain authorization
        // merely because the roster's owner is corrected to that tenant.
        legacy.connections.get_mut("b").unwrap().owner_id = "tenant-owner".into();
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(CATALOG_KEY).bind(store.seal(&serde_json::to_value(&legacy).unwrap()).unwrap())
            .execute(&store.app_pool).await.unwrap();
        let projected = store.model_catalog().await.unwrap();
        assert_eq!(projected.agent_models["agent-a"].owner_id, "tenant-owner");
        assert_eq!(projected.assignments["agent-a"].owner_id, "admin");
        assert!(projected.entries["a"].allowed_agents.contains("agent-a"));
        assert!(projected.connections["a"].allowed_agents.contains("agent-a"));
        assert!(projected.connections["b"].allowed_agents.is_empty());
        assert!(projected.resolve("agent-a").is_err());
        // An authorized administrator and the owner submit the same canonical owner;
        // storage advances revisions without turning the editor into the owner.
        let mut owner_edit = projected;
        owner_edit.agent_models.get_mut("agent-a").unwrap().roles.get_mut(&ModelRole::Standard).unwrap().backup_entry_ids = vec!["c".into()];
        let saved = store.save_model_catalog(&owner_edit,0).await.unwrap();
        assert_eq!(saved.agent_models["agent-a"].owner_id, "tenant-owner");
        assert_eq!(saved.agent_models["agent-a"].revision, 2);
        let mut oversight_edit = store.model_catalog().await.unwrap();
        oversight_edit.agent_models.get_mut("agent-a").unwrap().roles.insert(ModelRole::System,
            RoleAssignment { entry_id:"c".into(), backup_entry_ids:vec![] });
        let saved = store.save_model_catalog(&oversight_edit,1).await.unwrap();
        assert_eq!(saved.agent_models["agent-a"].owner_id, "tenant-owner");
        assert_eq!(saved.agent_models["agent-a"].revision, 3);
        assert!(saved.connections["b"].allowed_agents.is_empty());
        assert_eq!(store.model_catalog().await.unwrap(), saved);
        let mut transfer = saved.clone(); transfer.agent_models.get_mut("agent-a").unwrap().owner_id = "admin".into();
        assert!(store.save_model_catalog(&transfer,2).await.is_err());
    }

    // Newly accepted legacy-form writes use the same directory projection, while
    // an already explicit record is never rewritten as an implicit ownership transfer.
    #[tokio::test]
    async fn legacy_save_projects_owner_but_explicit_roles_keep_their_record_owner() {
        let root = tempfile::tempdir().unwrap(); let store = crate::store::Store::open(root.path()).await.unwrap();
        sqlx::query("INSERT INTO agent_directory(id,owner_id) VALUES('agent-a','tenant-owner')")
            .execute(&store.app_pool).await.unwrap();
        let saved = store.save_model_catalog(&catalog_fixture(),0).await.unwrap();
        assert_eq!(saved.agent_models["agent-a"].owner_id, "tenant-owner");
        assert_eq!(saved.resolve("agent-a").unwrap().owner_id, "tenant-owner");
        // Simulate an existing explicit snapshot from before directory reconciliation.
        let mut explicit = saved.clone(); explicit.agent_models.get_mut("agent-a").unwrap().owner_id = "previous-explicit-owner".into();
        sqlx::query("UPDATE docs SET value=? WHERE key=?")
            .bind(store.seal(&serde_json::to_value(&explicit).unwrap()).unwrap()).bind(CATALOG_KEY)
            .execute(&store.app_pool).await.unwrap();
        assert_eq!(store.model_catalog().await.unwrap().agent_models["agent-a"].owner_id,"previous-explicit-owner");
    }

    #[tokio::test]
    async fn encrypted_catalog_round_trip_and_revision_conflict() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        assert_eq!(store.model_catalog().await.unwrap(), Catalog::default());
        let saved = store.save_model_catalog(&catalog_fixture(), 0).await.unwrap();
        assert_eq!(saved.revision, 1);
        assert_eq!(store.model_catalog().await.unwrap(), saved);
        assert!(store.save_model_catalog(&catalog_fixture(), 0).await.is_err());
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(CATALOG_KEY).fetch_one(&store.app_pool).await.unwrap();
        assert!(!raw.windows(b"same-upstream".len()).any(|x| x == b"same-upstream"));
        let mut changed = saved.clone();
        changed.entries.get_mut("a").unwrap().display_name = "Renamed".into();
        let updated = store.save_model_catalog(&changed, 1).await.unwrap();
        assert_eq!(updated.entries["a"].revision, 2);
        assert_eq!(updated.entries["b"].revision, 1);
        store.app_pool.close().await; store.pool.close().await; store.agent_pool.close().await;
        let reopened = crate::store::Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.model_catalog().await.unwrap(), updated);
    }
    #[tokio::test]
    async fn revocation_allowed_but_new_assignment_and_owner_transfer_rejected() {
        let root = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(root.path()).await.unwrap();
        let mut catalog = store.save_model_catalog(&catalog_fixture(), 0).await.unwrap();
        catalog.connections.get_mut("a").unwrap().revoked = true;
        catalog = store.save_model_catalog(&catalog, 1).await.unwrap();
        assert!(catalog.resolve("agent-a").is_err());
        let mut transfer = catalog.clone();
        transfer.entries.get_mut("a").unwrap().owner_id = "other-owner".into();
        assert!(store.save_model_catalog(&transfer, 2).await.is_err());
        let mut assignment = catalog.assignments["agent-a"].clone();
        assignment.agent_id = "agent-b".into();
        catalog.assignments.insert("agent-b".into(), assignment);
        assert!(store.save_model_catalog(&catalog, 2).await.is_err());
        assert_eq!(store.model_catalog().await.unwrap().revision, 2);
    }
    #[test] fn explicit_order_and_distinct_connections() {
        let (a,e,c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        assert_eq!(p.candidates.iter().map(|x| x.entry_id.as_str()).collect::<Vec<_>>(), vec!["a","c","b"]);
        assert_ne!(p.candidates[0].credential_ref,p.candidates[1].credential_ref);
        assert_eq!(p.group_revision,Some(3));
    }
    #[test] fn editing_assignment_does_not_reroute_snapshot() {
        let (mut a,mut e,c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        a.primary_entry_id = "b".into(); a.revision += 1; e.get_mut("a").unwrap().upstream_model = "changed".into();
        assert_eq!(p.authorize_candidate(0,&e,&c).unwrap().upstream_model,"same-upstream");
        assert_eq!(SelectionPlan::resolve(&a,&e,&c,&g).unwrap().candidates[0].entry_id,"b");
    }
    #[test] fn every_backup_requires_model_and_connection_grants() {
        let (a,mut e,mut c,g) = fixture(); e.get_mut("c").unwrap().owner_id = "private".into(); e.get_mut("c").unwrap().allowed_agents.clear();
        assert!(SelectionPlan::resolve(&a,&e,&c,&g).is_err());
        e.get_mut("c").unwrap().allowed_agents.insert(a.agent_id.clone()); c.get_mut("c").unwrap().owner_id = "private".into(); c.get_mut("c").unwrap().allowed_agents.clear();
        assert!(SelectionPlan::resolve(&a,&e,&c,&g).is_err());
        c.get_mut("c").unwrap().allowed_agents.insert(a.agent_id.clone());
        let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        c.get_mut("c").unwrap().revoked = true; assert!(p.authorize_candidate(1,&e,&c).is_err());
    }
    #[test] fn replaced_credentials_are_not_silently_used() {
        let (a,e,mut c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        c.get_mut("a").unwrap().credential_ref = "new-credential".into();
        assert!(p.authorize_candidate(0,&e,&c).is_err());
    }
    #[test] fn duplicate_or_missing_group_rejected() {
        let (a,e,c,mut g) = fixture(); g.get_mut("group").unwrap().entry_ids.push("c".into());
        assert!(SelectionPlan::resolve(&a,&e,&c,&g).is_err());
        g.clear(); assert!(SelectionPlan::resolve(&a,&e,&c,&g).is_err());
    }
    #[test] fn bounded_retry_then_ordered_failover_then_terminal() {
        let (a,e,c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap(); let mut s = FailoverState::default();
        assert_eq!(s.failed(&p,FailureKind::Transient),NextAction::Retry);
        assert_eq!(s.failed(&p,FailureKind::Transient),NextAction::Backup);
        assert_eq!(s.history[0].to_entry_id,"c");
        assert_eq!(s.failed(&p,FailureKind::QuotaExhausted),NextAction::Backup);
        assert_eq!(s.history[1].to_entry_id,"b");
        assert_eq!(s.failed(&p,FailureKind::InvalidCredentials),NextAction::Terminal);
    }
    #[test] fn unsafe_failures_never_failover() {
        let (a,e,c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        for kind in [FailureKind::MalformedRequest,FailureKind::Cancelled,FailureKind::PolicyRejected,FailureKind::Protocol] {
            assert_eq!(FailoverState::default().failed(&p,kind),NextAction::Terminal);
        }
        let mut s = FailoverState { response_started: true, ..Default::default() };
        assert_eq!(s.failed(&p,FailureKind::Transient),NextAction::Terminal);
        let mut s = FailoverState { uncertain_tool_effect: true, ..Default::default() };
        assert_eq!(s.failed(&p,FailureKind::ModelUnavailable),NextAction::Terminal);
    }
    #[test] fn snapshot_and_history_round_trip() {
        let (a,e,c,g) = fixture(); let p = SelectionPlan::resolve(&a,&e,&c,&g).unwrap();
        assert_eq!(p,serde_json::from_str::<SelectionPlan>(&serde_json::to_string(&p).unwrap()).unwrap());
        let mut s = FailoverState::default(); s.failed(&p,FailureKind::ModelUnavailable);
        assert_eq!(s,serde_json::from_str::<FailoverState>(&serde_json::to_string(&s).unwrap()).unwrap());
    }
}
