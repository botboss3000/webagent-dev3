//! Borrow existing root resources; Access/Models/Storage/Engine retain their authority.
use webagent_accounts_host::AccountsStore;
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
        Ok(self.for_agent(agent).await?.agent_pool.clone())
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
    async fn leave(&self, agent: &str) -> Result<()> {
        let user = self.identity["_user_id"].as_str().unwrap_or("");
        self.app.store.leave_agent(user, agent).await?;
        crate::phase3_api::quiesce_user(self.app, user, Some(agent))
            .await
            .map_err(|e| anyhow!(e.1))
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
    /// Each step is idempotent, so a failed erase leaves an archived agent that can be retried.
    async fn erase(&self, agent: &str) -> Result<()> {
        let store = &self.app.store;
        for user in store.all_user_ids().await? {
            let mut caller = self.app.clone();
            caller.store = store.for_user(&user).await?;
            for session in caller.store.list("session:").await? {
                let Some(sid) = session["id"].as_str() else {
                    continue;
                };
                if session["agent_id"] != agent || session["status"] == "deleted" {
                    continue;
                }
                crate::api::quiesce(&caller, sid).await;
                caller.store.delete_agent_session(sid).await?;
            }
        }
        store.shared_schema().await?;
        let mut tx = store.app_pool.begin().await?;
        for table in ["shared_participants", "shared_invitations", "shared_observation_audit"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE session_id IN (SELECT session_id FROM shared_sessions WHERE agent_id=?)"))
                .bind(agent)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query("DELETE FROM shared_sessions WHERE agent_id=?")
            .bind(agent)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        // The catalog reads every referenced roster from its agent database, so prune it first.
        let mut catalog = store.model_catalog().await?;
        let assignments = catalog.assignments.len();
        catalog.assignments.retain(|_, assignment| assignment.agent_id != agent);
        let pruned = catalog.agent_models.remove(agent).is_some();
        if pruned || assignments != catalog.assignments.len() {
            let revision = catalog.revision;
            store.save_model_catalog(&catalog, revision).await?;
        }
        store.unregister_agent(agent).await
    }
}
