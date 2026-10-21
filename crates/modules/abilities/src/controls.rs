//! Agent ability controls, using existing scoped Storage resources and one encrypted CAS transaction.
use anyhow::{anyhow,bail,Context,Result};
use serde_json::{json,Value};
use sqlx::{SqlitePool,Row};
use webagent_access::storage::RecordCodec;

pub struct Repository<'a,C:RecordCodec> {
    agent_pool:&'a SqlitePool,
    agent_id:&'a str,
    actor_id:&'a str,
    codec:&'a C,
}
impl<'a,C:RecordCodec> Repository<'a,C> {
    /// Composition borrows the existing agent scope; this creates no pools or caches.
    pub fn new(agent_pool:&'a SqlitePool,agent_id:&'a str,actor_id:&'a str,codec:&'a C)->Self {
        Self {agent_pool,agent_id,actor_id,codec}
    }
    /// Seed absent ability connections without overriding saved enablement choices.
    pub async fn ensure_agent_connections(&self, ability_ids: &[String]) -> Result<()> {
        let mut tx = self.agent_pool.begin().await?;
        for id in ability_ids {
            sqlx::query("INSERT OR IGNORE INTO agent_connections(
                id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,1,?,1,strftime('%s','now'),strftime('%s','now'))")
                .bind(format!("ability:{id}")).bind(self.agent_id).bind(id).bind("ability")
                .bind(self.codec.seal(&json!({}))?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Project current encrypted agent metadata and disabled connections into effective ability controls.
    pub async fn agent_abilities_for(&self, agent_id: &str) -> Result<Value> {
        if agent_id != self.agent_id {return Err(anyhow!("Agent view mismatch"));}
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id).fetch_one(self.agent_pool).await?;
        let metadata = self.codec.unseal(&row.get::<Vec<u8>,_>("metadata"))?;
        let mut visibility = metadata["ability_modes"].as_object().cloned().unwrap_or_default();
        let rows = sqlx::query("SELECT connection_type,enabled FROM agent_connections WHERE agent_id=? AND section='ability'")
            .bind(agent_id).fetch_all(self.agent_pool).await?;
        for conn in rows {
            if conn.get::<i64,_>("enabled")==0 {
                visibility.insert(conn.get::<String,_>("connection_type"),json!("off"));
            }
        }
        Ok(json!({"revision":metadata["ability_revision"].as_i64().unwrap_or(0),"visibility":visibility,
            "skill_visibility":metadata["skill_modes"],"tool_modes":metadata["tool_modes"],"settings":metadata["ability_settings"],
            "tool_execution":metadata["tool_execution"].as_object().cloned().unwrap_or_default(),
            "tool_result_deduplication":tool_result_overrides(&metadata)?}))
    }
    /// CAS encrypted visibility settings, connection state and their audit receipt in one agent transaction.
    pub async fn save_agent_abilities(&self, value: &Value, expected_revision: u64) -> Result<()> {
        if expected_revision==u64::MAX {return Err(anyhow!("Agent ability revision overflow"));}
        let mut tx = self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(self.agent_id).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(self.agent_id).fetch_one(&mut *tx).await?;
        let mut metadata = self.codec.unseal(&row.get::<Vec<u8>,_>("metadata"))?;
        if metadata["ability_revision"].as_u64().unwrap_or(0) != expected_revision {
            return Err(anyhow!("Agent ability revision changed"));
        }
        let modes = value["visibility"].as_object().ok_or_else(||anyhow!("Invalid ability modes"))?;
        let mut active = serde_json::Map::new();
        for (id,mode) in modes {
            if mode != "off" { active.insert(id.clone(),mode.clone()); }
            sqlx::query("INSERT INTO agent_connections(id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,?,?,1,strftime('%s','now'),strftime('%s','now'))
                ON CONFLICT(id) DO UPDATE SET enabled=excluded.enabled,revision=agent_connections.revision+1,updated_at=excluded.updated_at")
                .bind(format!("ability:{id}")).bind(self.agent_id).bind(id).bind("ability")
                .bind((mode != "off") as i64).bind(self.codec.seal(&json!({}))?)
                .execute(&mut *tx).await?;
        }
        metadata["ability_modes"]=Value::Object(active);
        metadata["skill_modes"]=value["skill_visibility"].clone();
        metadata["tool_modes"]=value["tool_modes"].clone();
        metadata["ability_settings"]=value["settings"].clone();
        if let Some(execution)=value.get("tool_execution") {metadata["tool_execution"]=execution.clone();}
        if let Some(settings)=value.get("tool_result_deduplication") {
            let fields=settings.as_object().context("Invalid duplicate-result tool settings")?;
            if fields.len()>1024 || fields.values().any(|value|!value.is_boolean()){bail!("Invalid duplicate-result tool settings");}
            metadata["tool_result_deduplication_tools"]=settings.clone();
        }
        metadata["ability_revision"]=json!(expected_revision + 1);
        // Visibility and its encrypted audit receipt have one agent-owned commit.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!("agent:{}:ability_visibility_audit:{}",self.agent_id,expected_revision+1))
            .bind(self.codec.seal(&json!({"agent_id":self.agent_id,"actor_id":self.actor_id,"revision":expected_revision+1}))?)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=? AND revision=?")
            .bind(self.codec.seal(&metadata)?).bind(self.agent_id)
            .bind(row.get::<i64,_>("revision"))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

    pub fn tool_result_overrides(metadata:&Value)->Result<Value> {
        let tools=if let Some(value)=metadata.get("tool_result_deduplication_tools") {
            value.as_object().context("Invalid duplicate-result tool overrides")?.clone()
        } else {
            let mut legacy=metadata["tool_result_deduplication"].as_object().cloned().unwrap_or_default();
            legacy.remove("enabled");legacy
        };
        if tools.len()>1024 || tools.values().any(|value|!value.is_boolean()) {
            bail!("Invalid duplicate-result tool overrides");
        }
        Ok(Value::Object(tools))
    }

#[cfg(test)]
mod tests;
