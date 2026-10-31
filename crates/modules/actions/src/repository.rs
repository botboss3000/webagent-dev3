//! Requester-owned action records. SQLite, not a process mutex, owns claims: every change first
//! bumps the `action_lock` singleton, so competing connections serialize on the writer lock
//! before reading, including when they supply different action IDs for the same intent.
//! Records are always AEAD-sealed, even when administrator conversation encryption is off.
use crate::RecordCodec;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::{Sqlite, SqlitePool, Transaction};

/// Document key of the sealed record of action `id`.
#[must_use]
pub fn record_key(id: &str) -> String {
    format!("action:{id}")
}

const LOCK: &str = "UPDATE action_lock SET revision=revision+1 WHERE singleton=1";
const LATEST_FOR_INTENT: &str = "SELECT d.value FROM action_index i JOIN docs d ON d.key='action:'||i.id WHERE i.resource=? AND i.intent_key=? ORDER BY i.rowid DESC LIMIT 1";

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}

/// The action ledger of one requester's user plane, over the Storage codec and pool.
pub struct Ledger<'a, C> {
    codec: &'a C,
    pool: &'a SqlitePool,
    requester: &'a str,
}

impl<'a, C: RecordCodec> Ledger<'a, C> {
    /// Ledger of `requester`, whose user-plane pool is `pool`.
    pub fn new(codec: &'a C, pool: &'a SqlitePool, requester: &'a str) -> Self {
        Self {
            codec,
            pool,
            requester,
        }
    }

    /// The saved record of action `id`.
    pub async fn get(&self, id: &str) -> Result<Option<Value>> {
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(record_key(id))
            .fetch_optional(self.pool)
            .await?;
        raw.map(|raw| self.codec.unseal(&raw)).transpose()
    }

    /// The newest record with the same resource and intent key.
    pub async fn find_intent(&self, resource: &str, intent: &str) -> Result<Option<Value>> {
        let raw: Option<Vec<u8>> = sqlx::query_scalar(LATEST_FOR_INTENT)
            .bind(resource)
            .bind(intent)
            .fetch_optional(self.pool)
            .await?;
        raw.map(|v| self.codec.unseal(&v)).transpose()
    }

    /// The newest 32 records on `resource`, newest first.
    pub async fn list(&self, resource: &str) -> Result<Vec<Value>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM action_index WHERE resource=? ORDER BY rowid DESC LIMIT 32",
        )
        .bind(resource)
        .fetch_all(self.pool)
        .await?;
        let mut records = Vec::new();
        for id in ids {
            records.push(
                self.get(&id)
                    .await?
                    .ok_or_else(|| anyhow!("Action index is incomplete"))?,
            );
        }
        Ok(records)
    }

    /// Record `candidate` unless it already exists or duplicates a prior intent. Returns
    /// `{kind, action}` where kind is `created`, `existing`, `duplicate` or `conflict`.
    /// `separate_from` names the prior action a fresh, approved additional intent replaces.
    pub async fn prepare(&self, candidate: &Value, separate_from: Option<&str>) -> Result<Value> {
        if candidate["requester"] != self.requester {
            return Err(anyhow!("Action requester mismatch"));
        }
        let id = candidate["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action ID"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query(LOCK).execute(&mut *tx).await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(record_key(id))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(raw) = raw {
            let old = self.codec.unseal(&raw)?;
            if old["arguments_digest"] != candidate["arguments_digest"]
                || old["resource"] != candidate["resource"]
                || old["tool"] != candidate["tool"]
            {
                return Ok(json!({"kind":"conflict","action":Value::Null}));
            }
            return Ok(json!({"kind":"existing","action":old}));
        }
        let prior: Option<Vec<u8>> = sqlx::query_scalar(LATEST_FOR_INTENT)
            .bind(candidate["resource"].as_str())
            .bind(candidate["intent_key"].as_str())
            .fetch_optional(&mut *tx)
            .await?;
        let mut record = candidate.clone();
        record["requires_new_intent_approval"] = json!(false);
        if let Some(raw) = prior {
            let old = self.codec.unseal(&raw)?;
            if old["requester"] != self.requester
                || old["resource"] != candidate["resource"]
                || old["intent_key"] != candidate["intent_key"]
            {
                return Err(anyhow!("Action index scope mismatch"));
            }
            let fresh = !candidate["origin"].is_null()
                && !old["origin"].is_null()
                && if candidate["origin"]["storage_owner"] == old["origin"]["storage_owner"] {
                    candidate["origin"]["seq"].as_i64().unwrap_or(0)
                        > old["origin"]["seq"].as_i64().unwrap_or(0)
                } else {
                    candidate["origin"]["created_at"].as_i64().unwrap_or(0)
                        > old["updated_at"].as_i64().unwrap_or(i64::MAX)
                };
            if separate_from != old["id"].as_str()
                || !fresh
                || matches!(
                    old["state"].as_str(),
                    Some("running" | "uncertain" | "prepared")
                )
            {
                return Ok(json!({"kind":"duplicate","action":old}));
            }
            record["requires_new_intent_approval"] = json!(true);
            record["separate_from"] = old["id"].clone();
        } else if separate_from.is_some() {
            return Ok(json!({"kind":"conflict","action":Value::Null}));
        }
        record["state"] = json!("prepared");
        record["attempt"] = json!(0);
        record["revision"] = json!(0);
        sqlx::query("INSERT INTO action_index(id,resource,intent_key,state) VALUES(?,?,?,?)")
            .bind(id)
            .bind(record["resource"].as_str())
            .bind(record["intent_key"].as_str())
            .bind("prepared")
            .execute(&mut *tx)
            .await?;
        self.write(&mut tx, &record).await?;
        tx.commit().await?;
        Ok(json!({"kind":"created","action":record}))
    }

    async fn write(&self, tx: &mut Transaction<'_, Sqlite>, action: &Value) -> Result<()> {
        let id = action["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action identity"))?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(record_key(id))
            .bind(self.codec.seal(action)?)
            .execute(&mut **tx)
            .await?;
        sqlx::query("UPDATE action_index SET state=? WHERE id=?")
            .bind(action["state"].as_str())
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    /// Claim action `id` for one attempt. Only a prepared action, or a retry-safe confirmed
    /// failure, can be claimed; an additional matching intent also needs `approved`.
    /// Returns `{claimed, action}`.
    pub async fn claim(&self, id: &str, approved: bool) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(LOCK).execute(&mut *tx).await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(record_key(id))
            .fetch_one(&mut *tx)
            .await?;
        let mut action = self.codec.unseal(&raw)?;
        let retry = action["state"] == "confirmed_failed" && action["retry_safe"] == true;
        if (action["state"] != "prepared" && !retry)
            || (action["requires_new_intent_approval"] == true && !approved)
        {
            return Ok(json!({"claimed":false,"action":action}));
        }
        action["state"] = json!("running");
        action["attempt"] = json!(action["attempt"].as_u64().unwrap_or(0) + 1);
        action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
        action["claim"] = json!(uuid::Uuid::new_v4().to_string());
        action["updated_at"] = json!(now());
        self.write(&mut tx, &action).await?;
        tx.commit().await?;
        Ok(json!({"claimed":true,"action":action}))
    }

    /// Record the outcome of the attempt holding `claim`. Returns false when another attempt
    /// owns the action or it is already settled (a late finish is fenced off).
    pub async fn finish(
        &self,
        id: &str,
        claim: &str,
        state: &str,
        result: &Value,
        receipt: &Value,
        retry_safe: bool,
    ) -> Result<bool> {
        if !matches!(state, "completed" | "confirmed_failed" | "uncertain") {
            return Err(anyhow!("Invalid action outcome"));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query(LOCK).execute(&mut *tx).await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(record_key(id))
            .fetch_one(&mut *tx)
            .await?;
        let mut action = self.codec.unseal(&raw)?;
        if action["claim"] != claim
            || !matches!(action["state"].as_str(), Some("running" | "uncertain"))
        {
            return Ok(false);
        }
        action["state"] = json!(state);
        action["result"] = result.clone();
        action["receipt"] = receipt.clone();
        action["retry_safe"] = json!(state == "confirmed_failed" && retry_safe);
        action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
        action["updated_at"] = json!(now());
        self.write(&mut tx, &action).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Mark a still-prepared action as a retry-safe confirmed failure: it was never dispatched.
    pub async fn not_dispatched(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(LOCK).execute(&mut *tx).await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(record_key(id))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(raw) = raw {
            let mut action = self.codec.unseal(&raw)?;
            if action["state"] == "prepared" {
                action["state"] = json!("confirmed_failed");
                action["retry_safe"] = json!(true);
                action["result"] =
                    json!({"status":"denied","output":"Operation was not dispatched"});
                action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
                self.write(&mut tx, &action).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Mark a running action uncertain; its effect may or may not have happened.
    pub async fn interrupt(&self, id: &str) -> Result<()> {
        if let Some(action) = self.get(id).await? {
            if action["state"] == "running" {
                self.finish(
                    id,
                    action["claim"].as_str().unwrap_or(""),
                    "uncertain",
                    &Value::Null,
                    &Value::Null,
                    false,
                )
                .await?;
            }
        }
        Ok(())
    }
}
