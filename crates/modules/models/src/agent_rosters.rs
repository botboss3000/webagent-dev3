//! Immutable agent-owned rosters are staged before one atomic app-catalog pointer commit.
use crate::{storage::{RuntimeStorage,Repository},model_selection::{Catalog,AgentModels}};
use anyhow::{anyhow,bail,Result};
use serde::{Deserialize,Serialize};
use serde_json::Value;
use sqlx::Row;
use std::collections::BTreeMap;
const KEY:&str="model_selection:catalog:v1";
#[derive(Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredCatalog {version:u32,catalog:Catalog,agent_rosters:BTreeMap<String,String>}

impl<C:RuntimeStorage> Repository<'_,C> {
    pub(crate) async fn read_catalog_connection(&self,connection:&mut sqlx::SqliteConnection)->Result<Catalog> {
        let bytes:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(KEY).fetch_optional(&mut *connection).await?;
        let Some(bytes)=bytes else{return Ok(Catalog::default())};
        let value=self.unseal(&bytes)?;
        if value.get("version").is_none(){return Ok(serde_json::from_value(value)?);}
        let mut stored:StoredCatalog=serde_json::from_value(value)?;
        if stored.version!=1||!stored.catalog.agent_models.is_empty()||stored.agent_rosters.len()>1024 {bail!("Invalid model catalog storage version");}
        for (agent,key) in stored.agent_rosters {
            let prefix=format!("agent:{agent}:model_roster:");
            let hash=key.strip_prefix(&prefix).filter(|h|h.len()==64&&h.bytes().all(|b|b.is_ascii_hexdigit())).ok_or_else(||anyhow!("Invalid agent roster reference"))?;
            let pool=self.storage.agent_pool(&agent).await?;
            let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&pool).await?;
            let value=self.storage.unseal_agent_for(&agent,&raw)?;
            if webagent_configuration::digest(&value)!=hash {bail!("Agent roster commitment mismatch");}
            let roster:AgentModels=serde_json::from_value(value)?;
            if roster.agent_id!=agent {bail!("Agent roster identity mismatch");}
            stored.catalog.agent_models.insert(agent,roster);
        }
        Ok(stored.catalog)
    }
    pub(crate) async fn stage_catalog_connection(&self,connection:&mut sqlx::SqliteConnection,catalog:&Catalog)->Result<()> {
        catalog.validate()?;
        let mut public=catalog.clone();public.agent_models.clear();
        let mut references=BTreeMap::new();
        for (agent,roster) in &catalog.agent_models {
            let pool=self.storage.agent_pool(agent).await?;
            let value=serde_json::to_value(roster)?;
            let hash=webagent_configuration::digest(&value);
            let key=format!("agent:{agent}:model_roster:{hash}");
            sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES(?,?)").bind(&key)
                .bind(self.storage.seal_agent_for(agent,&value)?).execute(&pool).await?;
            let bytes:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&pool).await?;
            if self.storage.unseal_agent_for(agent,&bytes)?!=value {bail!("Staged agent roster differs from candidate");}
            references.insert(agent.clone(),key);
        }
        let stored=StoredCatalog{version:1,catalog:public,agent_rosters:references};
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(KEY).bind(self.seal(&serde_json::to_value(stored)?)?).execute(&mut *connection).await?;
        Ok(())
    }
    pub async fn ensure_agent_roster_storage(&self)->Result<()> {
        let mut tx=self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(KEY).execute(&mut *tx).await?;
        let bytes:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(KEY).fetch_optional(&mut *tx).await?;
        let Some(bytes)=bytes else{tx.commit().await?;return Ok(())};
        let value=self.unseal(&bytes)?;
        if value.get("version").is_some(){tx.commit().await?;return Ok(());}
        let mut catalog:Catalog=serde_json::from_value(value)?;
        catalog.migrate_stored(&mut tx).await?;
        self.stage_catalog_connection(&mut tx,&catalog).await?;
        tx.commit().await?;Ok(())
    }
}
