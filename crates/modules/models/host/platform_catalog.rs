//! Root composition delegates Models domain and persistence to its independently compiled package.
pub use webagent_models::platform_catalog::*;
use crate::store::Store;
use anyhow::Result;
use serde_json::{json,Value};
use std::collections::{BTreeMap,BTreeSet};
impl Store {
    pub async fn mutate_platform_roster(&self, id: &str, expected: u64,
        operation: Mutation, actor: &str) -> Result<Roster> { self.models_repository().mutate_platform_roster(id, expected, operation, actor).await }
    pub async fn platform_roster(&self, id: &str) -> Result<Option<Roster>> { self.models_repository().platform_roster(id).await }
    pub async fn platform_rosters(&self) -> Result<Vec<Roster>> { self.models_repository().platform_rosters().await }
    pub async fn platform_credential_configured(&self, roster_id:&str, entry:&Entry)->Result<bool> { self.models_repository().platform_credential_configured(roster_id, entry).await }
    pub(crate) async fn platform_tokens_pinned(&self,roster_id:&str,entry:&Entry,generation:&str)->Result<serde_json::Value> { self.models_repository().platform_tokens_pinned(roster_id, entry, generation).await }
    pub(crate) async fn platform_credential_generation(&self,roster_id:&str,entry:&Entry)->Result<String> { self.models_repository().platform_credential_generation(roster_id, entry).await }
    pub async fn platform_credential_set(&self, roster_id:&str, entry_id:&str,
        expected:u64, api_key:Option<&str>, actor:&str) -> Result<String> { self.models_repository().platform_credential_set(roster_id, entry_id, expected, api_key, actor).await }
}
#[cfg(test)]
mod persistence_tests {
    use super::*;
    #[tokio::test]
    async fn encrypted_lifecycle_reload_and_revision_fences() {
        let root=tempfile::tempdir().unwrap();
        let store=crate::store::Store::open(root.path()).await.unwrap();
        let draft=||Mutation::Draft{name:"Base".into(),entries:BTreeMap::new()};
        let first=store.mutate_platform_roster("base",0,draft(),"admin").await.unwrap();
        assert!(first.published().is_none());
        assert!(store.mutate_platform_roster("base",0,draft(),"admin").await.is_err());
        let published=store.mutate_platform_roster("base",1,Mutation::Publish{rollback:None},"admin").await.unwrap();
        let updated=store.mutate_platform_roster("base",2,draft(),"admin").await.unwrap();
        assert_eq!(updated.published(),published.published());
        let rolled=store.mutate_platform_roster("base",3,Mutation::Publish{rollback:Some(2)},"admin").await.unwrap();
        assert_eq!(rolled.published_revision,Some(4));
        assert!(store.mutate_platform_roster("base",4,Mutation::Retire,"admin").await.is_err());
        let reopened=crate::store::Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.platform_roster("base").await.unwrap(),Some(rolled));
    }
}
#[cfg(test)]
mod credential_tests {
    use super::*;
    use crate::model_selection::Transport;
    #[tokio::test]
    async fn rollback_never_restores_cleared_key_and_rotation_is_fenced() {
        let root=tempfile::tempdir().unwrap();let store=crate::store::Store::open(root.path()).await.unwrap();
        let entry=Entry{id:"model".into(),display_name:"Model".into(),upstream_model:"deepseek-chat".into(),
            transport:Transport::Deepseek,capabilities:BTreeSet::from(["text".into()])};
        store.mutate_platform_roster("base",0,Mutation::Draft{name:"Base".into(),entries:BTreeMap::from([("model".into(),entry.clone())])},"admin").await.unwrap();
        let first=store.platform_credential_set("base","model",1,Some("fixture-key"),"admin").await.unwrap();
        assert!(store.platform_credential_configured("base",&entry).await.unwrap());
        assert!(store.platform_credential_set("base","model",1,None,"admin").await.is_err());
        store.mutate_platform_roster("base",2,Mutation::Publish{rollback:None},"admin").await.unwrap();
        let cleared=store.platform_credential_set("base","model",3,None,"admin").await.unwrap();
        assert_ne!(first,cleared);
        store.mutate_platform_roster("base",4,Mutation::Publish{rollback:Some(3)},"admin").await.unwrap();
        assert!(!store.platform_credential_configured("base",&entry).await.unwrap());
        assert!(!serde_json::to_string(&store.platform_roster("base").await.unwrap()).unwrap().contains("fixture-key"));
    }
}