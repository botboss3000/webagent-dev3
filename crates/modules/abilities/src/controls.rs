//! Agent ability controls, using existing scoped Storage resources and one encrypted CAS transaction.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use webagent_access::storage::RecordCodec;

pub struct Repository<'a, C: RecordCodec> {
    agent_pool: &'a SqlitePool,
    agent_id: &'a str,
    actor_id: &'a str,
    codec: &'a C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    /// Composition borrows the existing agent scope; this creates no pools or caches.
    pub fn new(
        agent_pool: &'a SqlitePool,
        agent_id: &'a str,
        actor_id: &'a str,
        codec: &'a C,
    ) -> Self {
        Self {
            agent_pool,
            agent_id,
            actor_id,
            codec,
        }
    }
    /// Seed absent ability connections without overriding saved enablement choices.
    pub async fn ensure_agent_connections(&self, ability_ids: &[String]) -> Result<()> {
        let mut tx = self.agent_pool.begin().await?;
        for id in ability_ids {
            sqlx::query(
                "INSERT OR IGNORE INTO agent_connections(
                id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,1,?,1,strftime('%s','now'),strftime('%s','now'))",
            )
            .bind(format!("ability:{id}"))
            .bind(self.agent_id)
            .bind(id)
            .bind("ability")
            .bind(self.codec.seal_agent_for(self.agent_id, &json!({}))?)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Project current encrypted agent metadata and disabled connections into effective ability controls.
    pub async fn agent_abilities_for(&self, agent_id: &str) -> Result<Value> {
        if agent_id != self.agent_id {
            return Err(anyhow!("Agent view mismatch"));
        }
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(agent_id)
            .fetch_one(self.agent_pool)
            .await?;
        let metadata = self
            .codec
            .unseal_agent_for(self.agent_id, &row.get::<Vec<u8>, _>("metadata"))?;
        let mut visibility = metadata["ability_modes"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let rows = sqlx::query("SELECT connection_type,enabled FROM agent_connections WHERE agent_id=? AND section='ability'")
            .bind(agent_id).fetch_all(self.agent_pool).await?;
        for conn in rows {
            if conn.get::<i64, _>("enabled") == 0 {
                visibility.insert(conn.get::<String, _>("connection_type"), json!("off"));
            }
        }
        Ok(
            json!({"revision":metadata["ability_revision"].as_i64().unwrap_or(0),"visibility":visibility,
            "skill_visibility":metadata["skill_modes"],"tool_modes":metadata["tool_modes"],"settings":metadata["ability_settings"],
            "tool_execution":metadata["tool_execution"].as_object().cloned().unwrap_or_default(),
            "tool_result_deduplication":tool_result_overrides(&metadata)?}),
        )
    }
    /// CAS encrypted visibility settings, connection state and their audit receipt in one agent transaction.
    pub async fn save_agent_abilities(&self, value: &Value, expected_revision: u64) -> Result<()> {
        if expected_revision == u64::MAX {
            return Err(anyhow!("Agent ability revision overflow"));
        }
        let mut tx = self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?")
            .bind(self.agent_id)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(self.agent_id)
            .fetch_one(&mut *tx)
            .await?;
        let mut metadata = self
            .codec
            .unseal_agent_for(self.agent_id, &row.get::<Vec<u8>, _>("metadata"))?;
        if metadata["ability_revision"].as_u64().unwrap_or(0) != expected_revision {
            return Err(anyhow!("Agent ability revision changed"));
        }
        let modes = value["visibility"]
            .as_object()
            .ok_or_else(|| anyhow!("Invalid ability modes"))?;
        let mut active = serde_json::Map::new();
        for (id, mode) in modes {
            if mode != "off" {
                active.insert(id.clone(), mode.clone());
            }
            sqlx::query("INSERT INTO agent_connections(id,agent_id,connection_type,section,enabled,config,revision,created_at,updated_at)
                VALUES(?,?,?,?,?,?,1,strftime('%s','now'),strftime('%s','now'))
                ON CONFLICT(id) DO UPDATE SET enabled=excluded.enabled,revision=agent_connections.revision+1,updated_at=excluded.updated_at")
                .bind(format!("ability:{id}")).bind(self.agent_id).bind(id).bind("ability")
                .bind((mode != "off") as i64).bind(self.codec.seal_agent_for(self.agent_id,&json!({}))?)
                .execute(&mut *tx).await?;
        }
        metadata["ability_modes"] = Value::Object(active);
        metadata["skill_modes"] = value["skill_visibility"].clone();
        metadata["tool_modes"] = value["tool_modes"].clone();
        metadata["ability_settings"] = value["settings"].clone();
        if let Some(execution) = value.get("tool_execution") {
            metadata["tool_execution"] = execution.clone();
        }
        if let Some(settings) = value.get("tool_result_deduplication") {
            let fields = settings
                .as_object()
                .context("Invalid duplicate-result tool settings")?;
            if fields.len() > 1024 || fields.values().any(|value| !value.is_boolean()) {
                bail!("Invalid duplicate-result tool settings");
            }
            metadata["tool_result_deduplication_tools"] = settings.clone();
        }
        metadata["ability_revision"] = json!(expected_revision + 1);
        // Visibility and its encrypted audit receipt have one agent-owned commit.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!("agent:{}:ability_visibility_audit:{}",self.agent_id,expected_revision+1))
            .bind(self.codec.seal_agent_for(self.agent_id,&json!({"agent_id":self.agent_id,"actor_id":self.actor_id,"revision":expected_revision+1}))?)
            .execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=? AND revision=?")
            .bind(self.codec.seal_agent_for(self.agent_id,&metadata)?).bind(self.agent_id)
            .bind(row.get::<i64,_>("revision"))
            .execute(&mut *tx).await?;
        // C7: the repository-notice opt-in as a plain flag, stamped with the agent epoch this
        // commit leaves (the epoch trigger has already moved it). Without an epoch table no
        // flag is written and readers decrypt as before; a failure here changes no saved setting.
        if sqlx::query(OPT_IN_TABLE).execute(&mut *tx).await.is_ok() {
            let _ = sqlx::query(OPT_IN_WRITE)
                .bind(self.agent_id)
                .bind(i64::from(repository_notices(&metadata["ability_settings"])))
                .execute(&mut *tx)
                .await;
        }
        tx.commit().await?;
        Ok(())
    }
    /// C7: whether this agent's settings opt into repository notices, without decrypting the
    /// agent record while the plain flag was written at the agent database's current epoch
    /// (every change of the agent row moves the epoch, so any other writer of the settings
    /// makes the flag stale, never wrong). A stale or missing flag is read from the record and
    /// stored at that epoch for the next reader.
    pub async fn repository_notices_opted_in(&self) -> Result<bool> {
        let fresh: Option<i64> = sqlx::query_scalar(
            "SELECT o.opted_in FROM repository_notice_optin o, agent_epoch e
            WHERE o.agent_id=? AND e.singleton=1 AND o.checked=e.epoch",
        )
        .bind(self.agent_id)
        .fetch_optional(self.agent_pool)
        .await
        .ok()
        .flatten();
        if let Some(opted_in) = fresh {
            return Ok(opted_in != 0);
        }
        let mut tx = self.agent_pool.begin().await?;
        let epoch: i64 = sqlx::query_scalar("SELECT epoch FROM agent_epoch WHERE singleton=1")
            .fetch_one(&mut *tx)
            .await?;
        let metadata: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
                .bind(self.agent_id)
                .fetch_optional(&mut *tx)
                .await?;
        tx.commit().await?;
        let opted_in = match metadata {
            Some(blob) => repository_notices(
                &self.codec.unseal_agent_for(self.agent_id, &blob)?["ability_settings"],
            ),
            None => false,
        };
        // Best effort: a failed write only means the next reader decrypts again.
        if sqlx::query(OPT_IN_TABLE)
            .execute(self.agent_pool)
            .await
            .is_ok()
        {
            let _ = sqlx::query(
                "INSERT INTO repository_notice_optin(agent_id,checked,opted_in) VALUES(?,?,?)
                ON CONFLICT(agent_id) DO UPDATE SET checked=excluded.checked,
                opted_in=excluded.opted_in WHERE excluded.checked>repository_notice_optin.checked",
            )
            .bind(self.agent_id)
            .bind(epoch)
            .bind(i64::from(opted_in))
            .execute(self.agent_pool)
            .await;
        }
        Ok(opted_in)
    }
}

/// The plain repository-notice flag per agent and the agent epoch it was read at.
const OPT_IN_TABLE: &str = "CREATE TABLE IF NOT EXISTS repository_notice_optin(
    agent_id TEXT PRIMARY KEY, checked INTEGER NOT NULL, opted_in INTEGER NOT NULL)";
const OPT_IN_WRITE: &str = "INSERT INTO repository_notice_optin(agent_id,checked,opted_in)
    SELECT ?1,epoch,?2 FROM agent_epoch WHERE singleton=1
    ON CONFLICT(agent_id) DO UPDATE SET checked=excluded.checked,opted_in=excluded.opted_in";

/// Whether ability settings opt into repository-change notices (Git Control).
pub fn repository_notices(settings: &Value) -> bool {
    settings["git_control"]["notify_repo_changes"] == true
}

pub fn tool_result_overrides(metadata: &Value) -> Result<Value> {
    let tools = if let Some(value) = metadata.get("tool_result_deduplication_tools") {
        value
            .as_object()
            .context("Invalid duplicate-result tool overrides")?
            .clone()
    } else {
        let mut legacy = metadata["tool_result_deduplication"]
            .as_object()
            .cloned()
            .unwrap_or_default();
        legacy.remove("enabled");
        legacy
    };
    if tools.len() > 1024 || tools.values().any(|value| !value.is_boolean()) {
        bail!("Invalid duplicate-result tool overrides");
    }
    Ok(Value::Object(tools))
}

#[cfg(test)]
mod tests;

/// Whether an agent's Agent Management `settings` let it start sessions of other agents.
/// The Default Agent may unless its settings say otherwise; other agents may not.
pub fn delegation_allowed(settings: &Value, agent: &str) -> bool {
    settings["allow_delegation"]
        .as_bool()
        .unwrap_or(agent == "shared_default")
}

/// The Web Search providers an agent's settings switch on, in manifest order. Defaults
/// follow `abilities/Web/web_search/web_search.json`; a value of the wrong type keeps the
/// default, as the plugin itself reads it.
pub fn web_search_providers(settings: &Value) -> Vec<&'static str> {
    [
        ("tavily", true),
        ("brave", false),
        ("wikipedia", true),
        ("duckduckgo", false),
    ]
    .into_iter()
    .filter(|(provider, default)| {
        settings[format!("{provider}_enabled")]
            .as_bool()
            .unwrap_or(*default)
    })
    .map(|(provider, _)| provider)
    .collect()
}

#[cfg(test)]
mod web_search_provider_tests {
    use super::*;

    #[test]
    fn only_switched_on_providers_are_offered() {
        assert_eq!(web_search_providers(&Value::Null), ["tavily", "wikipedia"]);
        assert_eq!(
            web_search_providers(
                &json!({"tavily_enabled":false,"brave_enabled":true,"duckduckgo_enabled":"yes"})
            ),
            ["brave", "wikipedia"]
        );
    }
}

#[cfg(test)]
mod delegation_tests {
    use super::*;

    #[test]
    fn default_agent_delegates_unless_turned_off() {
        assert!(delegation_allowed(&Value::Null, "shared_default"));
        assert!(!delegation_allowed(&Value::Null, "agent-x"));
        assert!(!delegation_allowed(
            &json!({"allow_delegation":false}),
            "shared_default"
        ));
        assert!(delegation_allowed(
            &json!({"allow_delegation":true}),
            "agent-x"
        ));
    }
}
