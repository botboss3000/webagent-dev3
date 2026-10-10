use super::*;
const KEY: &str = "user_agent_page_preferences:v1";
impl Store {
    pub async fn agent_page_preferences(&self) -> Result<Value> {
        Ok(self.get(KEY).await?.unwrap_or(json!({"page_ids":["config"],"revision":0,"configured":false})))
    }
    pub async fn save_agent_page_preferences(&self, pages: &[String], expected: u64) -> Result<Option<Value>> {
        let mut tx=self.pool.begin().await?;
        // Reserve the user-plane writer before reading the revision, including across processes.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO NOTHING")
            .bind(KEY).bind(self.encode_user_record(&json!({"page_ids":["config"],"revision":0,"configured":false}))?)
            .execute(&mut *tx).await?;
        let blob:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(KEY).fetch_one(&mut *tx).await?;
        let current=self.unseal(&blob)?;
        if current["revision"].as_u64()!=Some(expected) {tx.rollback().await?;return Ok(None);}
        let revision=expected.checked_add(1).ok_or_else(||anyhow!("Preference revision exhausted"))?;
        let saved=json!({"page_ids":pages,"revision":revision,"configured":true});
        sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.encode_user_record(&saved)?).bind(KEY).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(Some(saved))
    }
}
