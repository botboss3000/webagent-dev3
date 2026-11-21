//! Concrete existing application records and owner-supplied codec/resources.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use std::{future::Future, path::Path};
use webagent_access::{
    authority::LiveAccess,
    storage::{RecordCodec, Repository as AccessRepository},
};
pub trait RuntimeStorage: RecordCodec + Clone + Send + Sync + 'static {
    fn credential_get<'a>(
        &'a self,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a;
    fn credential_put<'a>(
        &'a self,
        key: &'a str,
        value: &'a Value,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    fn agent_pool<'a>(
        &'a self,
        agent: &'a str,
    ) -> impl Future<Output = Result<SqlitePool>> + Send + 'a;
    /// The named account's own database.
    fn user_pool<'a>(
        &'a self,
        user: &'a str,
    ) -> impl Future<Output = Result<SqlitePool>> + Send + 'a;
    /// One decoded document from the named agent's own database, read as that agent.
    fn agent_document<'a>(
        &'a self,
        agent: &'a str,
        key: &'a str,
    ) -> impl Future<Output = Result<Option<Value>>> + Send + 'a {
        async move {
            let pool = self.agent_pool(agent).await?;
            let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(key)
                .fetch_optional(&pool)
                .await?;
            raw.map(|raw| self.unseal(&raw)).transpose()
        }
    }
}
pub struct Repository<'a, C: RuntimeStorage> {
    pub(crate) root: &'a Path,
    pub(crate) app_pool: &'a SqlitePool,
    pub(crate) agent_pool: &'a SqlitePool,
    pub(crate) agent_id: &'a str,
    pub(crate) actor: &'a str,
    pub(crate) storage: &'a C,
}
impl<'a, C: RuntimeStorage> Repository<'a, C> {
    pub fn new(
        root: &'a Path,
        app_pool: &'a SqlitePool,
        agent_pool: &'a SqlitePool,
        agent_id: &'a str,
        actor: &'a str,
        storage: &'a C,
    ) -> Self {
        Self {
            root,
            app_pool,
            agent_pool,
            agent_id,
            actor,
            storage,
        }
    }
    pub(crate) fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.storage.seal(value)
    }
    pub(crate) fn unseal(&self, value: &[u8]) -> Result<Value> {
        self.storage.unseal(value)
    }
    pub(crate) fn access(&self) -> AccessRepository<'_, C> {
        AccessRepository::new(
            self.root,
            self.app_pool,
            self.agent_pool,
            self.agent_id,
            self.actor,
            self.storage,
        )
    }
    pub(crate) async fn live_access(&self, user: &str, agent: &str) -> Result<LiveAccess> {
        webagent_access::authority::validate_live_access(user, agent)?;
        let pool = self.storage.agent_pool(agent).await?;
        AccessRepository::new(
            self.root,
            self.app_pool,
            &pool,
            agent,
            self.actor,
            self.storage,
        )
        .live_access(user, agent)
        .await
    }
}

impl<C: RuntimeStorage> Repository<'_, C> {
    pub(crate) fn current_user_id(&self) -> &str {
        self.actor
    }
    pub(crate) async fn get(&self, key: &str) -> Result<Option<Value>> {
        self.storage.credential_get(key).await
    }
    pub(crate) async fn put(&self, key: &str, value: &Value) -> Result<()> {
        self.storage.credential_put(key, value).await
    }
    pub(crate) async fn for_agent(&self, agent: &str) -> Result<SqlitePool> {
        self.storage.agent_pool(agent).await
    }
    pub(crate) async fn for_user(&self, user: &str) -> Result<SqlitePool> {
        self.storage.user_pool(user).await
    }
    pub(crate) async fn account_by_id(&self, user: &str) -> Result<Option<Value>> {
        self.access().account_by_id(user).await
    }
    pub(crate) async fn entitlement_tier(&self, id: &str) -> Result<Option<webagent_access::Tier>> {
        self.access().entitlement_tier(id).await
    }
    pub(crate) async fn entitlement_tiers(&self) -> Result<Vec<webagent_access::Tier>> {
        self.access().entitlement_tiers().await
    }
    pub(crate) async fn agent_authority(&self, user: &str, agent: &str) -> Result<Value> {
        let pool = self.storage.agent_pool(agent).await?;
        AccessRepository::new(
            self.root,
            self.app_pool,
            &pool,
            agent,
            self.actor,
            self.storage,
        )
        .agent_authority(user, agent)
        .await
    }
}

#[cfg(test)]
pub(crate) mod fixtures;
