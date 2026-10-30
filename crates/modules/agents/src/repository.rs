//! SQL owns Agents records only. Storage supplies existing scoped handles and encryption.
use crate::{IdentityUpdate, NewAgent};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

/// Existing Storage codec contract; no keys, pools or processes are created by Agents.
pub use webagent_access::storage::RecordCodec;

pub struct Repository<'a, C: RecordCodec> {
    pool: &'a SqlitePool,
    agent: &'a str,
    codec: &'a C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    /// Trusted composition binds this existing pool to its tenant before any SQL is issued.
    pub fn new(pool: &'a SqlitePool, agent: &'a str, codec: &'a C) -> Result<Self> {
        if !webagent_access::valid_agent_id(agent) {
            bail!("Invalid agent identity");
        }
        Ok(Self { pool, agent, codec })
    }
    pub fn agent_id(&self) -> &str {
        self.agent
    }
    pub async fn summary(&self) -> Result<Value> {
        let row = sqlx::query("SELECT name,description,status,revision FROM agents WHERE id=?")
            .bind(self.agent)
            .fetch_one(self.pool)
            .await?;
        Ok(json!({"id":self.agent,"name":row.get::<String,_>("name"),
            "description":row.get::<String,_>("description"),"status":row.get::<String,_>("status"),
            "revision":row.get::<i64,_>("revision")}))
    }
    pub async fn identity(&self) -> Result<Value> {
        let row =
            sqlx::query("SELECT name,description,status,revision,metadata FROM agents WHERE id=?")
                .bind(self.agent)
                .fetch_one(self.pool)
                .await?;
        let metadata = self
            .codec
            .unseal_agent_for(self.agent, &row.get::<Vec<u8>, _>("metadata"))?;
        let modes = crate::modes::config(&metadata)?;
        Ok(json!({"id":self.agent,"name":row.get::<String,_>("name"),
            "description":row.get::<String,_>("description"),"status":row.get::<String,_>("status"),
            "revision":row.get::<i64,_>("revision"),"execution_modes":modes["execution_modes"],
            "default_execution_mode":modes["default_execution_mode"]}))
    }
    pub async fn update(&self, update: &IdentityUpdate, now: i64) -> Result<()> {
        update.validate()?;
        let mut tx = self.pool.begin().await?;
        self.update_in(&mut tx, update, now).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Host composition may retain its existing transaction for neighbor settings/prompt effects.
    pub async fn update_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        update: &IdentityUpdate,
        now: i64,
    ) -> Result<()> {
        update.validate()?;
        let row = sqlx::query("SELECT metadata,revision FROM agents WHERE id=?")
            .bind(self.agent)
            .fetch_one(&mut **tx)
            .await?;
        if row.get::<i64, _>("revision") != update.revision {
            bail!("Identity changed; reload before saving");
        }
        let mut metadata = self
            .codec
            .unseal_agent_for(self.agent, &row.get::<Vec<u8>, _>("metadata"))?;
        update.apply(&mut metadata)?;
        self.save_in(tx, update, &metadata, now).await
    }

    /// Commit the composed, owner-validated metadata under the shared identity revision.
    /// The caller retains the transaction for existing neighbor prompt effects.
    pub async fn save_in(
        &self,
        tx: &mut Transaction<'_, Sqlite>,
        update: &IdentityUpdate,
        metadata: &Value,
        now: i64,
    ) -> Result<()> {
        crate::validate_identity(update.name.as_deref(), update.description.as_deref())?;
        if update.revision < 0 || update.revision == i64::MAX || !metadata.is_object() {
            bail!("Invalid identity update");
        }
        crate::modes::config(metadata)?;
        let changed = sqlx::query("UPDATE agents SET name=COALESCE(?,name),description=COALESCE(?,description),metadata=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?")
            .bind(update.name.as_deref().map(str::trim)).bind(update.description.as_deref())
            .bind(self.codec.seal_agent_for(self.agent, metadata)?).bind(now).bind(self.agent)
            .bind(update.revision).execute(&mut **tx).await?;
        if changed.rows_affected() != 1 {
            bail!("Identity changed; reload before saving");
        }
        Ok(())
    }
    pub async fn status(&self) -> Result<Value> {
        let row = sqlx::query("SELECT status,revision FROM agents WHERE id=?")
            .bind(self.agent)
            .fetch_optional(self.pool)
            .await?
            .ok_or_else(|| anyhow!("Agent not found"))?;
        Ok(
            json!({"agent_id":self.agent,"status":row.get::<String,_>("status"),"revision":row.get::<i64,_>("revision")}),
        )
    }
    /// Disable rather than delete: references and retained work remain available to their owners.
    pub async fn set_status(&self, expected: u64, status: &str) -> Result<Value> {
        crate::validate_status(expected, status)?;
        let changed = sqlx::query("UPDATE agents SET status=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=? AND revision=?")
            .bind(status).bind(self.agent).bind(expected as i64).execute(self.pool).await?;
        if changed.rows_affected() != 1 {
            bail!("Agent revision changed");
        }
        Ok(json!({"agent_id":self.agent,"status":status,"revision":expected+1}))
    }
    /// Owner archive. A disabled agent archives as `archived_disabled` and unarchives disabled,
    /// so unarchiving never lifts an administrator's disable.
    pub async fn set_archived(&self, expected: u64, archived: bool) -> Result<Value> {
        let current = self.status().await?;
        let to = match (archived, current["status"].as_str().unwrap_or("")) {
            (true, "active") => "archived",
            (true, "disabled") => "archived_disabled",
            (false, "archived") => "active",
            (false, "archived_disabled") => "disabled",
            _ => "",
        };
        let from = current["status"].as_str().unwrap_or("");
        let changed = if to.is_empty() {
            0
        } else {
            sqlx::query("UPDATE agents SET status=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=? AND revision=? AND status=?")
                .bind(to).bind(self.agent).bind(expected as i64).bind(from).execute(self.pool).await?.rows_affected()
        };
        if changed != 1 {
            let current = self.status().await?;
            if current["revision"].as_u64() != Some(expected) {
                bail!("Agent revision changed");
            }
            bail!(if archived {
                "Agent is already archived"
            } else {
                "Agent is not archived"
            });
        }
        Ok(json!({"agent_id":self.agent,"status":to,"revision":expected+1}))
    }
}

/// Create/reconcile an unregistered row using Storage's existing creation pool.
/// Registered records use tenant encoding; pre-registration records retain the published shared encoding.
pub async fn create_record<C: RecordCodec>(
    pool: &SqlitePool,
    codec: &C,
    agent: &str,
    owner: &str,
    request: &NewAgent,
    registered: bool,
    now: i64,
) -> Result<()> {
    request.validate()?;
    if !webagent_access::valid_id(owner) || request.id(owner)? != agent {
        bail!("Invalid agent identity");
    }
    let digest = request.request_digest()?;
    let existing: Option<Vec<u8>> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_optional(pool)
        .await?;
    if let Some(blob) = existing {
        let metadata = if registered {
            codec.unseal_agent_for(agent, &blob)?
        } else {
            codec.unseal(&blob)?
        };
        if metadata["creation_digest"] != digest || metadata["owner_id"] != owner {
            bail!("Request identifier reused with different input");
        }
        return Ok(());
    }
    if registered {
        bail!("Registered agent record missing");
    }
    let metadata = codec.seal(&json!({"owner_id":owner,"ability_modes":{"codebase_admin":"off"},
        "skill_modes":{},"tool_modes":{},"ability_revision":0,
        "member_access":{"visitor_access":false,"app_login_members":true,"conversation_privacy":"operator_reviewable"},
        "creation_digest":digest}))?;
    sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES(?,?,?,'openai_subscription','','active',?,1,?,?)")
        .bind(agent).bind(request.name.trim()).bind(request.description.as_deref().unwrap_or(""))
        .bind(metadata).bind(now).bind(now).execute(pool).await?;
    Ok(())
}
