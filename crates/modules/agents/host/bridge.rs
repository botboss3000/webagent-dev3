//! Borrow existing root resources; Access/Models/Storage/Engine retain their authority.
use crate::{App, Store};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::SqlitePool;
use webagent_access::{Principal, Relationship};

pub(super) struct Bridge<'a> {
    pub app: &'a App,
    pub identity: &'a Value,
}
impl webagent_agents::service::Storage for Store {
    async fn agent_ids(&self) -> Result<Vec<String>> {
        Store::agent_ids(self).await
    }
    async fn agent_pool(&self, agent: &str) -> Result<SqlitePool> {
        Ok(self.for_agent(agent).await?.agent_pool)
    }
    async fn creation_pool(&self, agent: &str) -> Result<SqlitePool> {
        crate::agent_storage::open_agent_pool(&self.root, agent).await
    }
    async fn registered(&self, agent: &str) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, String>("SELECT id FROM agent_directory WHERE id=?")
                .bind(agent)
                .fetch_optional(&self.app_pool)
                .await?
                .is_some(),
        )
    }
    async fn register(&self, agent: &str, owner: &str) -> Result<()> {
        self.register_agent(agent, owner).await
    }
    async fn receipt(&self, key: &str) -> Result<Option<Value>> {
        self.get(key).await
    }
    async fn save_receipt(&self, key: &str, value: &Value) -> Result<()> {
        self.put(key, value).await
    }
}
impl webagent_agents::service::Access for Bridge<'_> {
    async fn principal(&self) -> Result<Principal> {
        let account = self
            .app
            .store
            .account_by_id(self.identity["_user_id"].as_str().unwrap_or(""))
            .await?
            .ok_or_else(|| anyhow!("Agent administration unavailable"))?;
        Principal::from_account(&account)
    }
    async fn authority(&self, agent: &str, write: bool) -> Result<Value> {
        super::authority(self.app, self.identity, agent, write)
            .await
            .map_err(|e| anyhow!(e.1))
    }
    async fn ensure_owner(
        &self,
        owner: &str,
        agent: &str,
        relationship: Relationship,
    ) -> Result<()> {
        if relationship != Relationship::Owner {
            bail!("Agent ownership changed");
        }
        match self.app.store.agent_membership(owner, agent).await? {
            None => {
                self.app
                    .store
                    .set_agent_membership(owner, agent, Relationship::Owner, 0)
                    .await?;
            }
            Some(value) if value["relationship"] == "owner" => {}
            Some(_) => bail!("Agent ownership changed"),
        }
        Ok(())
    }
    async fn status_authority(&self, agent: &str) -> Result<()> {
        crate::phase3_api::administrator(self.app, self.identity)
            .await
            .map_err(|e| anyhow!(e.1))?;
        super::authority(self.app, self.identity, agent, true)
            .await
            .map_err(|e| anyhow!(e.1))?;
        Ok(())
    }
}
impl webagent_agents::service::Effects for Bridge<'_> {
    async fn seed_model(&self, owner: &str, agent: &str) -> Result<bool> {
        self.app.store.seed_new_agent_default(owner, agent).await
    }
    async fn status_event(&self, status: &Value) -> Result<()> {
        let payload = json!({"agent_id":status["agent_id"],"status":status["status"],"revision":status["revision"]});
        sqlx::query(
            "INSERT INTO events(session_id,kind,payload) VALUES('','agent_status_changed',?)",
        )
        .bind(webagent_access::storage::RecordCodec::encode_application(
            &self.app.store,
            &payload,
        )?)
        .execute(&self.app.store.app_pool)
        .await?;
        Ok(())
    }
    async fn quiesce(&self, agent: &str) -> Result<()> {
        crate::phase3_api::quiesce_agent(self.app, agent)
            .await
            .map_err(|e| anyhow!(e.1))
    }
}
