//! Caller-owned file records: names are metadata, never host filesystem paths.
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use webagent_access::storage::RecordCodec;

pub fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 255
        || value.trim() != value
        || value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || matches!(value, "." | "..")
    {
        bail!("Invalid user file name");
    }
    Ok(())
}
pub fn metadata(record: &Value) -> Value {
    json!({"name":record["name"],"media_type":record["media_type"],
        "bytes":record["bytes"],"sha256":record["sha256"],"revision":record["revision"]})
}
pub struct Invocation<'a> {
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub pending_id: &'a str,
    pub generation: &'a str,
    pub arguments: &'a Value,
}
pub struct Repository<'a, C: RecordCodec> {
    pool: &'a SqlitePool,
    codec: &'a C,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    /// Composition must supply the authenticated caller's private user pool.
    pub fn new(pool: &'a SqlitePool, codec: &'a C) -> Self {
        Self { pool, codec }
    }
    fn key(filename: &str) -> Result<String> {
        name(filename)?;
        use sha2::{Digest, Sha256};
        Ok(format!(
            "core_user_file:{}",
            hex::encode(Sha256::digest(filename.as_bytes()))
        ))
    }
    pub async fn read(&self, filename: &str) -> Result<Option<Value>> {
        let bytes: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(Self::key(filename)?)
            .fetch_optional(self.pool)
            .await?;
        bytes.map(|b| self.codec.unseal(&b)).transpose()
    }
    pub async fn list(&self, offset: u32, limit: u32) -> Result<Vec<Value>> {
        if limit == 0 || limit > 100 {
            bail!("File list limit must be 1..100");
        }
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM docs WHERE substr(key,1,15)='core_user_file:' ORDER BY key LIMIT ? OFFSET ?")
            .bind(limit).bind(offset).fetch_all(self.pool).await?;
        rows.iter()
            .map(|b| self.codec.unseal(b).map(|v| metadata(&v)))
            .collect()
    }
    /// File CAS and the exact invocation receipt commit together; retries cannot rewrite a newer file.
    pub async fn save(
        &self,
        record: &Value,
        expected: u64,
        invocation: &Invocation<'_>,
    ) -> Result<Value> {
        let filename = record["name"].as_str().context("File name required")?;
        let key = Self::key(filename)?;
        let receipt = format!(
            "receipt:core_user_file:{}",
            webagent_configuration::digest(&json!([
                invocation.session_id,
                invocation.run_id,
                invocation.pending_id
            ]))
        );
        let fingerprint = webagent_configuration::digest(&json!([record, expected]));
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?")
            .bind(format!("session:{}", invocation.session_id))
            .execute(&mut *tx)
            .await?;
        let prior: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&receipt)
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(prior) = prior {
            let prior = self.codec.unseal(&prior)?;
            if prior["digest"] != fingerprint {
                bail!("Invocation reused with different file input");
            }
            return Ok(prior["result"].clone());
        }
        let session: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("session:{}", invocation.session_id))
            .fetch_optional(&mut *tx)
            .await?
            .context("Session unavailable")?;
        let session = self.codec.unseal(&session)?;
        let run: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("run:{}", invocation.run_id))
            .fetch_optional(&mut *tx)
            .await?
            .context("Run unavailable")?;
        let run = self.codec.unseal(&run)?;
        let pending = if run["pending"]["id"] == invocation.pending_id {
            &run["pending"]
        } else {
            run["batch"]
                .as_array()
                .and_then(|xs| {
                    xs.iter()
                        .find(|x| x["pending"]["id"] == invocation.pending_id)
                })
                .map(|x| &x["pending"])
                .unwrap_or(&Value::Null)
        };
        if session["status"] != "active"
            || run["session_id"] != invocation.session_id
            || run["id"] != invocation.run_id
            || run["generation"] != invocation.generation
            || run["status"] != "running"
            || pending["state"] != "executing"
            || pending["name"] != "save_file"
            || pending["arguments"] != *invocation.arguments
        {
            bail!("File call stopped or its execution generation changed");
        }
        if run["lease_epoch"].as_i64().unwrap_or(0) > 0 {
            let lease = sqlx::query(
                "SELECT lease_epoch,lease_owner,lease_expires_at FROM run_index WHERE id=?",
            )
            .bind(invocation.run_id)
            .fetch_optional(&mut *tx)
            .await?
            .context("Run lease unavailable")?;
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs() as i64;
            if lease.get::<i64, _>("lease_epoch") != run["lease_epoch"].as_i64().unwrap_or(-1)
                || lease.get::<Option<String>, _>("lease_owner").as_deref()
                    != run["lease_owner"].as_str()
                || lease.get::<i64, _>("lease_expires_at") < now
            {
                bail!("File run lease expired or changed");
            }
        }
        let old: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
        let old = old.map(|b| self.codec.unseal(&b)).transpose()?;
        if old
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0)
            != expected
        {
            bail!("User file changed; read its revision before saving");
        }
        let mut record = record.clone();
        record["revision"] = json!(expected.checked_add(1).context("File revision exhausted")?);
        let result = metadata(&record);
        for (key, value) in [
            (key, record),
            (receipt, json!({"digest":fingerprint,"result":result})),
        ] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.codec.seal(&value)?).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(result)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Codec;
    impl RecordCodec for Codec {
        fn seal(&self, v: &Value) -> Result<Vec<u8>> {
            Ok(serde_json::to_vec(v)?)
        }
        fn unseal(&self, b: &[u8]) -> Result<Value> {
            Ok(serde_json::from_slice(b)?)
        }
        fn encode_application(&self, v: &Value) -> Result<Vec<u8>> {
            self.seal(v)
        }
    }
    #[tokio::test]
    async fn private_files_fence_revisions_and_retries() {
        let a = SqlitePool::connect("sqlite::memory:").await.unwrap();
        let b = SqlitePool::connect("sqlite::memory:").await.unwrap();
        for pool in [&a, &b] {
            sqlx::query("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL)")
                .execute(pool)
                .await
                .unwrap();
        }
        let repo = Repository::new(&a, &Codec);
        let record = json!({"name":"report.txt","media_type":"text/plain","bytes":5,"data":"aGVsbG8=","sha256":"fixture"});
        let args = json!({});
        let run = json!({"id":"run","session_id":"session","status":"running","generation":"generation","pending":{"id":"pending","state":"executing","name":"save_file","arguments":args}});
        for (key, value) in [
            ("session:session", json!({"id":"session","status":"active"})),
            ("run:run", run.clone()),
        ] {
            sqlx::query("INSERT INTO docs VALUES(?,?)")
                .bind(key)
                .bind(Codec.seal(&value).unwrap())
                .execute(&a)
                .await
                .unwrap();
        }
        let invocation = Invocation {
            session_id: "session",
            run_id: "run",
            pending_id: "pending",
            generation: "generation",
            arguments: &args,
        };
        let first = repo.save(&record, 0, &invocation).await.unwrap();
        assert_eq!(first["revision"], 1);
        assert_eq!(repo.save(&record, 0, &invocation).await.unwrap(), first);
        let mut next = run.clone();
        next["pending"]["id"] = json!("next");
        sqlx::query("UPDATE docs SET value=? WHERE key='run:run'")
            .bind(Codec.seal(&next).unwrap())
            .execute(&a)
            .await
            .unwrap();
        let next_call = Invocation {
            pending_id: "next",
            ..invocation
        };
        assert!(repo.save(&record, 0, &next_call).await.is_err());
        next["status"] = json!("stopped");
        sqlx::query("UPDATE docs SET value=? WHERE key='run:run'")
            .bind(Codec.seal(&next).unwrap())
            .execute(&a)
            .await
            .unwrap();
        assert!(repo.save(&record, 1, &next_call).await.is_err());
        assert_eq!(repo.list(0, 20).await.unwrap().len(), 1);
        assert!(Repository::new(&b, &Codec)
            .read("report.txt")
            .await
            .unwrap()
            .is_none());
        for path in ["../report", "/state/secret", "a\\b", ".", ".."] {
            assert!(repo.read(path).await.is_err());
        }
    }
}
