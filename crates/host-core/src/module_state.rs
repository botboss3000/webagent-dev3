//! Optional package state: one sealed, revisioned document per package in the user's database.
use crate::StoreCore;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

impl StoreCore {
    /// One optional package's private state, or the empty initial state.
    pub async fn module_state(&self, id: &str) -> Result<Value> {
        Ok(self
            .get(&format!("module_state:{id}"))
            .await?
            .unwrap_or_else(|| json!({"revision":0,"state":{},"pending":[]})))
    }
    /// Replace package `id`'s state when its stored revision is `expected`, bumping it.
    pub async fn module_cas(&self, id: &str, expected: u64, value: &Value) -> Result<()> {
        if serde_json::to_vec(value)?.len() > 3 * 1024 * 1024 {
            return Err(anyhow!("Module state capacity reached"));
        }
        let key = format!("module_state:{id}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
        let previous = raw.map(|raw| self.unseal(&raw)).transpose()?;
        if previous
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0)
            != expected
        {
            return Err(anyhow!("Module revision changed"));
        }
        let mut next = value.clone();
        next["revision"] = json!(expected + 1);
        // Never let administrator plaintext storage expose connector signing keys.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&key).bind(self.seal(&next)?).execute(&mut *tx).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(())
    }
}
