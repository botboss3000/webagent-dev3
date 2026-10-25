//! Shared conversation policy, access kinds, observer projection and app-plane directory/invitation repository.
use crate::RecordCodec;
use anyhow::{anyhow, bail, Result};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, Sqlite, SqlitePool, Transaction};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SharedChatPolicy {
    pub shareable: bool,
    pub admin_view: bool,
    pub admin_join: bool,
    pub help: bool,
    pub initial_active: bool,
}

impl Default for SharedChatPolicy {
    fn default() -> Self {
        Self {
            shareable: false,
            admin_view: false,
            admin_join: false,
            help: false,
            initial_active: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Owner,
    Participant,
    Observer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionAccess {
    pub actor_id: String,
    pub owner_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub kind: AccessKind,
}

impl SessionAccess {
    pub fn can_write(&self) -> bool {
        self.kind != AccessKind::Observer
    }
    pub fn is_owner(&self) -> bool {
        self.kind == AccessKind::Owner
    }
}

#[derive(Debug)]
pub struct SessionAuthorityDenied;
impl std::fmt::Display for SessionAuthorityDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conversation authority unavailable")
    }
}
impl std::error::Error for SessionAuthorityDenied {}

pub fn observer_projection(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        for field in [
            "checkpoint",
            "input",
            "diagnostics",
            "pending",
            "approval",
            "approvals",
            "tool_calls",
            "context",
            "recovery",
        ] {
            map.remove(field);
        }
    }
}

pub fn participation_state(session: &Value) -> Value {
    session
        .get("participation")
        .cloned()
        .unwrap_or_else(|| json!({"active":true,"revision":0,"generation":0}))
}

pub async fn require_participation_in<C: RecordCodec>(
    codec: &C,
    tx: &mut Transaction<'_, Sqlite>,
    run: &Value,
) -> Result<()> {
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run session missing"))?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(format!("session:{sid}"))
        .fetch_one(&mut **tx)
        .await?;
    let current = participation_state(&codec.unseal(&raw)?);
    if current["active"] != true
        || current["generation"].as_u64().unwrap_or(0)
            != run["participation_generation"].as_u64().unwrap_or(0)
    {
        return Err(anyhow!("Agent participation changed"));
    }
    Ok(())
}

/// App-plane shared session directory, participants, invitations and observation audit.
pub struct DirectoryRepository<'a, C: RecordCodec> {
    app_pool: &'a SqlitePool,
    codec: &'a C,
}

impl<'a, C: RecordCodec> DirectoryRepository<'a, C> {
    pub fn new(app_pool: &'a SqlitePool, codec: &'a C) -> Self {
        Self { app_pool, codec }
    }

    pub async fn schema(&self) -> Result<()> {
        sqlx::raw_sql(
            "CREATE TABLE IF NOT EXISTS shared_sessions(session_id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,agent_id TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS shared_participants(session_id TEXT NOT NULL,user_id TEXT NOT NULL,joined_at INTEGER NOT NULL,read_through_seq INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(session_id,user_id));
 CREATE TABLE IF NOT EXISTS shared_invitations(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,issuer_id TEXT NOT NULL,request_id TEXT NOT NULL,expires_at INTEGER NOT NULL,requested_ttl INTEGER NOT NULL,redeemed_by TEXT,revoked INTEGER NOT NULL DEFAULT 0,UNIQUE(session_id,issuer_id,request_id));
 CREATE TABLE IF NOT EXISTS shared_observation_audit(session_id TEXT NOT NULL,actor_id TEXT NOT NULL,observed_at INTEGER NOT NULL);",
        )
        .execute(self.app_pool)
        .await?;
        Ok(())
    }

    pub async fn register_session(&self, owner: &str, sid: &str, agent: &str) -> Result<()> {
        if !webagent_access::valid_id(owner)
            || sid.is_empty()
            || sid.len() > 128
            || !webagent_access::valid_agent_id(agent)
        {
            bail!("Invalid shared session");
        }
        self.schema().await?;
        sqlx::query("INSERT OR IGNORE INTO shared_sessions VALUES(?,?,?)")
            .bind(sid)
            .bind(owner)
            .bind(agent)
            .execute(self.app_pool)
            .await?;
        let dir = self.directory(sid).await?.unwrap();
        if dir["owner_id"] != owner || dir["agent_id"] != agent {
            bail!("Session directory conflict");
        }
        Ok(())
    }

    pub async fn directory(&self, sid: &str) -> Result<Option<Value>> {
        self.schema().await?;
        Ok(
            sqlx::query("SELECT owner_id,agent_id FROM shared_sessions WHERE session_id=?")
                .bind(sid)
                .fetch_optional(self.app_pool)
                .await?
                .map(|r| {
                    json!({
                        "session_id": sid,
                        "owner_id": r.get::<String, _>("owner_id"),
                        "agent_id": r.get::<String, _>("agent_id")
                    })
                }),
        )
    }

    pub async fn is_participant(&self, sid: &str, user: &str) -> Result<bool> {
        self.schema().await?;
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM shared_participants WHERE session_id=? AND user_id=?",
        )
        .bind(sid)
        .bind(user)
        .fetch_optional(self.app_pool)
        .await?
        .is_some())
    }

    pub async fn sessions_for(&self, user: &str) -> Result<Vec<Value>> {
        self.schema().await?;
        Ok(sqlx::query("SELECT s.session_id,s.owner_id,s.agent_id FROM shared_sessions s WHERE s.owner_id=? OR EXISTS(SELECT 1 FROM shared_participants p WHERE p.session_id=s.session_id AND p.user_id=?) ORDER BY s.session_id")
            .bind(user)
            .bind(user)
            .fetch_all(self.app_pool)
            .await?
            .into_iter()
            .map(|r| json!({
                "session_id": r.get::<String, _>("session_id"),
                "owner_id": r.get::<String, _>("owner_id"),
                "agent_id": r.get::<String, _>("agent_id")
            }))
            .collect())
    }

    pub async fn all_sessions(&self) -> Result<Vec<Value>> {
        self.schema().await?;
        Ok(sqlx::query(
            "SELECT session_id,owner_id,agent_id FROM shared_sessions ORDER BY session_id LIMIT 1000",
        )
        .fetch_all(self.app_pool)
        .await?
        .into_iter()
        .map(|r| {
            json!({
                "session_id": r.get::<String, _>("session_id"),
                "owner_id": r.get::<String, _>("owner_id"),
                "agent_id": r.get::<String, _>("agent_id")
            })
        })
        .collect())
    }

    pub async fn participants(&self, sid: &str) -> Result<Vec<Value>> {
        self.schema().await?;
        let dir = self
            .directory(sid)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        let mut members = vec![json!({"user_id":dir["owner_id"],"role":"owner"})];
        for r in sqlx::query("SELECT user_id,joined_at,read_through_seq FROM shared_participants WHERE session_id=? ORDER BY joined_at,user_id")
            .bind(sid)
            .fetch_all(self.app_pool)
            .await?
        {
            members.push(json!({
                "user_id": r.get::<String, _>("user_id"),
                "joined_at": r.get::<i64, _>("joined_at"),
                "read_through_seq": r.get::<i64, _>("read_through_seq"),
                "role": "participant"
            }));
        }
        Ok(members)
    }

    pub async fn join(&self, sid: &str, user: &str, now: u64) -> Result<bool> {
        self.schema().await?;
        let directory = self
            .directory(sid)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        if directory["owner_id"] == user {
            return Ok(false);
        }
        Ok(sqlx::query(
            "INSERT OR IGNORE INTO shared_participants(session_id,user_id,joined_at) VALUES(?,?,?)",
        )
        .bind(sid)
        .bind(user)
        .bind(now as i64)
        .execute(self.app_pool)
        .await?
        .rows_affected()
            == 1)
    }

    pub async fn invite_issue(
        &self,
        sid: &str,
        issuer: &str,
        request: &str,
        ttl: u64,
        now: u64,
    ) -> Result<Value> {
        self.schema().await?;
        if request.is_empty() || request.len() > 128 || ttl == 0 || ttl > 30 * 86400 {
            bail!("Invalid invitation");
        }
        let mut tx = self.app_pool.begin().await?;
        let mut random = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random);
        sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES('shared_invitation_key',?)")
            .bind(self.codec.seal(&json!(hex::encode(random)))?)
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='shared_invitation_key'")
                .fetch_one(&mut *tx)
                .await?;
        let secret = self.codec.unseal(&raw)?;
        let secret = hex::decode(
            secret
                .as_str()
                .ok_or_else(|| anyhow!("Invalid invitation key"))?,
        )?;
        let existing = sqlx::query("SELECT id,expires_at,requested_ttl FROM shared_invitations WHERE session_id=? AND issuer_id=? AND request_id=?")
            .bind(sid)
            .bind(issuer)
            .bind(request)
            .fetch_optional(&mut *tx)
            .await?;
        let (id, expires) = if let Some(row) = existing {
            if row.get::<i64, _>("requested_ttl") != ttl as i64 {
                bail!("Invitation request ID reused with different input");
            }
            (row.get::<String, _>("id"), row.get::<i64, _>("expires_at"))
        } else {
            (
                uuid::Uuid::new_v4().to_string(),
                now.checked_add(ttl)
                    .ok_or_else(|| anyhow!("Expiration overflow"))? as i64,
            )
        };
        let mut inner = [0x36u8; 64];
        let mut outer = [0x5cu8; 64];
        if secret.len() != 32 {
            bail!("Invalid invitation key");
        }
        for (i, b) in secret.iter().enumerate() {
            inner[i] ^= b;
            outer[i] ^= b;
        }
        let mut hash = Sha256::new();
        hash.update(inner);
        hash.update(serde_json::to_vec(&json!([
            "shared-chat-invitation-v1",
            id,
            sid,
            issuer,
            request,
            expires
        ]))?);
        let mut result = Sha256::new();
        result.update(outer);
        result.update(hash.finalize());
        let token = hex::encode(result.finalize());
        sqlx::query("INSERT OR IGNORE INTO shared_invitations(id,session_id,token_hash,issuer_id,request_id,expires_at,requested_ttl) VALUES(?,?,?,?,?,?,?)")
            .bind(&id)
            .bind(sid)
            .bind(format!("{:x}", Sha256::digest(token.as_bytes())))
            .bind(issuer)
            .bind(request)
            .bind(expires)
            .bind(ttl as i64)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(json!({"id":id,"token":token,"expires_at":expires}))
    }

    pub async fn invites(&self, sid: &str) -> Result<Vec<Value>> {
        self.schema().await?;
        Ok(sqlx::query("SELECT id,issuer_id,expires_at,redeemed_by,revoked FROM shared_invitations WHERE session_id=? ORDER BY expires_at DESC LIMIT 200")
            .bind(sid)
            .fetch_all(self.app_pool)
            .await?
            .into_iter()
            .map(|r| json!({
                "id": r.get::<String, _>("id"),
                "issuer_id": r.get::<String, _>("issuer_id"),
                "expires_at": r.get::<i64, _>("expires_at"),
                "redeemed": r.get::<Option<String>, _>("redeemed_by").is_some(),
                "revoked": r.get::<i64, _>("revoked") == 1
            }))
            .collect())
    }

    pub async fn invite_session(&self, token: &str, now: u64) -> Result<Option<String>> {
        self.schema().await?;
        if token.len() != 64 {
            return Ok(None);
        }
        Ok(sqlx::query_scalar("SELECT session_id FROM shared_invitations WHERE token_hash=? AND revoked=0 AND (redeemed_by IS NULL) AND expires_at>?")
            .bind(format!("{:x}", Sha256::digest(token.as_bytes())))
            .bind(now as i64)
            .fetch_optional(self.app_pool)
            .await?)
    }

    pub async fn invite_session_for(
        &self,
        token: &str,
        user: &str,
        now: u64,
    ) -> Result<Option<String>> {
        self.schema().await?;
        if token.len() != 64 {
            return Ok(None);
        }
        Ok(sqlx::query_scalar("SELECT session_id FROM shared_invitations WHERE token_hash=? AND ((revoked=0 AND redeemed_by IS NULL AND expires_at>?) OR redeemed_by=?)")
            .bind(format!("{:x}", Sha256::digest(token.as_bytes())))
            .bind(now as i64)
            .bind(user)
            .fetch_optional(self.app_pool)
            .await?)
    }

    pub async fn invite_redeem(&self, token: &str, user: &str, now: u64) -> Result<(String, bool)> {
        self.schema().await?;
        if token.len() != 64 {
            bail!("Invalid invitation");
        }
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE shared_invitations SET revoked=revoked WHERE token_hash=?")
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        let row = sqlx::query(
            "SELECT session_id,redeemed_by,revoked,expires_at FROM shared_invitations WHERE token_hash=?",
        )
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| anyhow!("Invalid invitation"))?;
        let sid: String = row.get("session_id");
        let redeemed: Option<String> = row.get("redeemed_by");
        if redeemed.as_deref() == Some(user) {
            tx.commit().await?;
            return Ok((sid, false));
        }
        if redeemed.is_some()
            || row.get::<i64, _>("revoked") != 0
            || row.get::<i64, _>("expires_at") <= now as i64
        {
            bail!("Invitation expired or used");
        }
        sqlx::query("UPDATE shared_invitations SET redeemed_by=? WHERE token_hash=?")
            .bind(user)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        let added = sqlx::query(
            "INSERT OR IGNORE INTO shared_participants(session_id,user_id,joined_at) VALUES(?,?,?)",
        )
        .bind(&sid)
        .bind(user)
        .bind(now as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok((sid, added))
    }

    pub async fn invite_revoke(&self, sid: &str, id: &str) -> Result<()> {
        self.schema().await?;
        sqlx::query(
            "UPDATE shared_invitations SET revoked=1 WHERE session_id=? AND id=? AND redeemed_by IS NULL",
        )
        .bind(sid)
        .bind(id)
        .execute(self.app_pool)
        .await?;
        Ok(())
    }

    pub async fn observe_audit(&self, sid: &str, user: &str, now: u64) -> Result<()> {
        self.schema().await?;
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO shared_observation_audit VALUES(?,?,?)")
            .bind(sid)
            .bind(user)
            .bind(now as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM shared_observation_audit WHERE observed_at<? OR rowid NOT IN (SELECT rowid FROM shared_observation_audit ORDER BY observed_at DESC,rowid DESC LIMIT 100000)")
            .bind(now.saturating_sub(30 * 86400) as i64)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn acknowledge_read(&self, sid: &str, user: &str, through: u64) -> Result<()> {
        self.schema().await?;
        sqlx::query("UPDATE shared_participants SET read_through_seq=MAX(read_through_seq,?) WHERE session_id=? AND user_id=?")
            .bind(through as i64)
            .bind(sid)
            .bind(user)
            .execute(self.app_pool)
            .await?;
        Ok(())
    }
}
