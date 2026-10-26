//! Concrete existing application records and owner-supplied codec/resources.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use sqlx::SqlitePool;
use std::{path::Path,future::Future};
use webagent_access::{authority::LiveAccess,storage::{RecordCodec,Repository as AccessRepository}};
pub trait RuntimeStorage: RecordCodec+Clone+Send+Sync+'static {
    fn credential_get<'a>(&'a self,key:&'a str)->impl Future<Output=Result<Option<Value>>>+Send+'a;
    fn credential_put<'a>(&'a self,key:&'a str,value:&'a Value)->impl Future<Output=Result<()>>+Send+'a;
    fn agent_pool<'a>(&'a self,agent:&'a str)->impl Future<Output=Result<SqlitePool>>+Send+'a;
}
pub struct Repository<'a,C:RuntimeStorage> {
    pub(crate) root:&'a Path,
    pub(crate) app_pool:&'a SqlitePool,
    pub(crate) agent_pool:&'a SqlitePool,
    pub(crate) agent_id:&'a str,
    pub(crate) actor:&'a str,
    pub(crate) storage:&'a C,
}
impl<'a,C:RuntimeStorage> Repository<'a,C> {
    pub fn new(root:&'a Path,app_pool:&'a SqlitePool,agent_pool:&'a SqlitePool,
        agent_id:&'a str,actor:&'a str,storage:&'a C)->Self {
        Self {root,app_pool,agent_pool,agent_id,actor,storage}
    }
    pub(crate) fn seal(&self,value:&Value)->Result<Vec<u8>> {self.storage.seal(value)}
    pub(crate) fn unseal(&self,value:&[u8])->Result<Value> {self.storage.unseal(value)}
    pub(crate) fn access(&self)->AccessRepository<'_,C> {
        AccessRepository::new(self.root,self.app_pool,self.agent_pool,self.agent_id,self.actor,self.storage)
    }
    pub(crate) async fn live_access(&self,user:&str,agent:&str)->Result<LiveAccess> {
        webagent_access::authority::validate_live_access(user,agent)?;
        let pool=self.storage.agent_pool(agent).await?;
        AccessRepository::new(self.root,self.app_pool,&pool,agent,self.actor,self.storage).live_access(user,agent).await
    }
}

impl<C:RuntimeStorage> Repository<'_,C> {
    pub(crate) fn current_user_id(&self)->&str {self.actor}
    pub(crate) async fn get(&self,key:&str)->Result<Option<Value>> {self.storage.credential_get(key).await}
    pub(crate) async fn put(&self,key:&str,value:&Value)->Result<()> {self.storage.credential_put(key,value).await}
    pub(crate) async fn for_agent(&self,agent:&str)->Result<SqlitePool> {self.storage.agent_pool(agent).await}
    pub(crate) async fn account_by_id(&self,user:&str)->Result<Option<Value>> {self.access().account_by_id(user).await}
    pub(crate) async fn entitlement_tier(&self,id:&str)->Result<Option<webagent_access::Tier>> {self.access().entitlement_tier(id).await}
    pub(crate) async fn entitlement_tiers(&self)->Result<Vec<webagent_access::Tier>> {self.access().entitlement_tiers().await}
    pub(crate) async fn agent_authority(&self,user:&str,agent:&str)->Result<Value> {
        let pool=self.storage.agent_pool(agent).await?;
        AccessRepository::new(self.root,self.app_pool,&pool,agent,self.actor,self.storage).agent_authority(user,agent).await
    }
}

impl<C:RuntimeStorage> Repository<'_,C> {
    pub async fn migrate_legacy_default<F,Fut>(&self,load_legacy:F,default_model:&str)->anyhow::Result<()>
    where F:FnOnce()->Fut,Fut:std::future::Future<Output=anyhow::Result<Value>> {

    use crate::model_selection::*;
    const MARKER:&str="migration:model_roles_default_v1";
    if self.get(MARKER).await?.is_some() {return Ok(());}
    let mut catalog=self.model_catalog().await?;
    if !catalog.agent_models.contains_key("shared_default") && self.get("provider_tokens").await?.is_some() {
        let selection=crate::provider::ProviderSelection::parse(&load_legacy().await?,default_model)?;
        if selection.provider != crate::provider::ProviderId::OpenaiSubscription {
            anyhow::bail!("Legacy default provider requires explicit model configuration");
        }
        let grants=std::collections::BTreeSet::from(["shared_default".to_owned()]);
        let id="legacy-default-subscription".to_owned();
        if catalog.connections.contains_key(&id) || catalog.entries.contains_key(&id) {anyhow::bail!("Legacy migration identity conflict");}
        catalog.connections.insert(id.clone(),Connection {id:id.clone(),owner_id:"admin".into(),
            transport:Transport::OpenaiSubscription,credential_ref:"provider_tokens".into(),
            allowed_agents:grants.clone(),revision:1,revoked:false});
        catalog.entries.insert(id.clone(),ModelEntry {id:id.clone(),owner_id:"admin".into(),
            connection_id:id.clone(),upstream_model:selection.model,display_name:"Existing default subscription".into(),
            capabilities:std::collections::BTreeSet::from(["text".into()]),allowed_agents:grants,revision:1});
        catalog.agent_models.insert("shared_default".into(),AgentModels { limits: std::collections::BTreeMap::new(), efforts: std::collections::BTreeMap::new(),agent_id:"shared_default".into(),owner_id:"admin".into(),
            entry_ids:vec![id.clone()],roles:std::collections::BTreeMap::from([(ModelRole::Standard,
                RoleAssignment {entry_id:id,backup_entry_ids:vec![]})]),revision:1});
        self.save_model_catalog(&catalog,catalog.revision).await?;
    } else if !catalog.agent_models.is_empty() {
        // Persist projected old primary assignments without changing their ordered backups.
        self.save_model_catalog(&catalog,catalog.revision).await?;
    }
    self.put(MARKER,&json!({"complete":true})).await?;
    Ok(())

    }
}

#[cfg(test)] pub(crate) mod fixtures;
