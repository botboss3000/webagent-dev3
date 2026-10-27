//! Authenticated Agents operations. Neighbor modules retain their own authority and side effects.
use crate::{
    repository::{RecordCodec, Repository},
    IdentityUpdate, NewAgent,
};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::future::Future;
use tokio::sync::Mutex;
use webagent_access::{Principal, Relationship, Role};

/// Account reload, authorization and membership remain Access-owned operations.
pub trait Access: Sync {
    fn principal(&self) -> impl Future<Output = Result<Principal>> + Send;
    fn authority<'a>(
        &'a self,
        agent: &'a str,
        write: bool,
    ) -> impl Future<Output = Result<Value>> + Send + 'a;
    fn ensure_owner<'a>(
        &'a self,
        owner: &'a str,
        agent: &'a str,
        relationship: Relationship,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn status_authority<'a>(
        &'a self,
        agent: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}
/// Storage owns routing, pool lifecycle, registration, receipts and codec implementation.
pub trait Storage: RecordCodec {
    fn agent_ids(&self) -> impl Future<Output = Result<Vec<String>>> + Send;
    fn agent_pool<'a>(
        &'a self,
        agent: &'a str,
    ) -> impl Future<Output = Result<SqlitePool>> + Send + 'a;
    fn creation_pool<'a>(
        &'a self,
        agent: &'a str,
    ) -> impl Future<Output = Result<SqlitePool>> + Send + 'a;
    fn registered<'a>(&'a self, agent: &'a str) -> impl Future<Output = Result<bool>> + Send + 'a;
    fn register<'a>(
        &'a self,
        agent: &'a str,
        owner: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn receipt<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
    fn save_receipt<'a>(
        &'a self,
        key: &'a str,
        value: &'a Value,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}
/// Model assignment and Engine quiescence stay with their production owners.
pub trait Effects: Sync {
    fn seed_model<'a>(
        &'a self,
        owner: &'a str,
        agent: &'a str,
    ) -> impl Future<Output = Result<bool>> + Send + 'a;
    fn status_event<'a>(
        &'a self,
        status: &'a Value,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn quiesce<'a>(&'a self, agent: &'a str) -> impl Future<Output = Result<()>> + Send + 'a;
}

pub struct Service<'a, S: Storage, A: Access, E: Effects> {
    pub storage: &'a S,
    pub access: &'a A,
    pub effects: &'a E,
    /// The existing application mutation gate, shared with all other metadata writers.
    pub gate: &'a Mutex<()>,
}
impl<S: Storage, A: Access, E: Effects> Service<'_, S, A, E> {
    async fn principal(&self) -> Result<Principal> {
        let caller = self.access.principal().await?;
        if !caller.active() || !webagent_access::valid_id(&caller.user_id) {
            bail!("Agent administration unavailable");
        }
        Ok(caller)
    }
    async fn authorize(&self, agent: &str, write: bool) -> Result<Value> {
        self.principal().await?;
        if !webagent_access::valid_agent_id(agent) {
            bail!("Invalid agent identity");
        }
        // Access must apply both manage and live-use admission, as the current endpoint does.
        let rights = self.access.authority(agent, write).await?;
        if write && rights["can_manage"] != true {
            bail!("Agent administration unavailable");
        }
        Ok(rights)
    }
    pub async fn list(&self) -> Result<Value> {
        let caller = self.principal().await?;
        let mut agents = Vec::new();
        for id in self.storage.agent_ids().await? {
            let Ok(rights) = self.authorize(&id, false).await else {
                continue;
            };
            let pool = self.storage.agent_pool(&id).await?;
            let mut identity = Repository::new(&pool, &id, self.storage)?.summary().await?;
            if crate::binned(&identity) {
                continue;
            }
            identity["can_manage"] = rights["can_manage"].clone();
            agents.push(identity);
        }
        Ok(
            json!({"agents":agents,"caller_id":caller.user_id,"can_create_agent":caller.role!=Role::Guest}),
        )
    }
    pub async fn identity(&self, agent: &str) -> Result<Value> {
        let rights = self.authorize(agent, true).await?;
        let pool = self.storage.agent_pool(agent).await?;
        let mut value = Repository::new(&pool, agent, self.storage)?
            .identity()
            .await?;
        value["can_manage"] = rights["can_manage"].clone();
        Ok(value)
    }
    pub async fn update(&self, agent: &str, update: &IdentityUpdate, now: i64) -> Result<Value> {
        update.validate()?;
        let _guard = self.gate.lock().await;
        self.authorize(agent, true).await?;
        let pool = self.storage.agent_pool(agent).await?;
        Repository::new(&pool, agent, self.storage)?
            .update(update, now)
            .await?;
        self.identity(agent).await
    }
    pub async fn create(&self, request: &NewAgent, now: i64) -> Result<Value> {
        let caller = self.principal().await?;
        if caller.role == Role::Guest {
            bail!("Agent administration unavailable");
        }
        request.validate()?;
        let owner = &caller.user_id;
        let receipt = request.receipt_key(owner);
        let digest = request.request_digest()?;
        let _guard = self.gate.lock().await;
        // Recheck account state after waiting behind another mutation.
        let current = self.principal().await?;
        if current.user_id != *owner || current.role == Role::Guest {
            bail!("Agent administration unavailable");
        }
        if let Some(saved) = self.storage.receipt(&receipt).await? {
            if saved["digest"] != digest {
                bail!("Request identifier reused with different input");
            }
            return Ok(saved["result"].clone());
        }
        let id = request.id(owner)?;
        let pool = self.storage.creation_pool(&id).await?;
        let registered = self.storage.registered(&id).await?;
        crate::repository::create_record(&pool, self.storage, &id, owner, request, registered, now)
            .await?;
        // Release this handle; Storage owns whether it is cached or ephemeral.
        drop(pool);
        self.storage.register(&id, owner).await?;
        self.access
            .ensure_owner(owner, &id, Relationship::Owner)
            .await?;
        let seeded = self.effects.seed_model(owner, &id).await?;
        let result = json!({"id":id,"name":request.name.trim(),"assignment_required":!seeded});
        self.storage
            .save_receipt(&receipt, &json!({"digest":digest,"result":result}))
            .await?;
        Ok(result)
    }
    pub async fn set_status(&self, agent: &str, expected: u64, status: &str) -> Result<Value> {
        crate::validate_status(expected, status)?;
        let _guard = self.gate.lock().await;
        self.authorize(agent, true).await?;
        self.access.status_authority(agent).await?;
        let pool = self.storage.agent_pool(agent).await?;
        let value = Repository::new(&pool, agent, self.storage)?
            .set_status(expected, status)
            .await?;
        self.effects.status_event(&value).await?;
        self.effects.quiesce(agent).await?;
        Ok(value)
    }
    /// Managers move their agents to and from the recycling bin; the default agent stays.
    pub async fn set_deleted(&self, agent: &str, expected: u64, deleted: bool) -> Result<Value> {
        if agent == "shared_default" {
            bail!("The default agent cannot be deleted");
        }
        if expected >= i64::MAX as u64 {
            bail!("Invalid agent status");
        }
        let _guard = self.gate.lock().await;
        self.authorize(agent, true).await?;
        let pool = self.storage.agent_pool(agent).await?;
        let value = Repository::new(&pool, agent, self.storage)?
            .set_deleted(expected, deleted)
            .await?;
        self.effects.status_event(&value).await?;
        if deleted {
            self.effects.quiesce(agent).await?;
        }
        Ok(value)
    }
}
