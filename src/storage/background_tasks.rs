use super::*;

impl Store {
    async fn background_index(&self) -> Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS background_task_index(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,revision INTEGER NOT NULL,status TEXT NOT NULL)").execute(&self.pool).await?;
        sqlx::query("CREATE TABLE IF NOT EXISTS background_task_delivery(id TEXT PRIMARY KEY,run_id TEXT NOT NULL)").execute(&self.pool).await?;
        Ok(())
    }
    pub(crate) async fn background_get(&self, id: &str) -> Result<Option<Value>> {
        self.background_index().await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("background_task:{id}")).fetch_optional(&self.pool).await?;
        raw.map(|raw| self.unseal(&raw)).transpose()
    }
    pub(crate) async fn background_scan(&self, sid: Option<&str>) -> Result<Vec<Value>> {
        self.background_index().await?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM background_task_index WHERE (? IS NULL OR session_id=?) AND (status IN ('queued','running') OR (status='completed' AND NOT EXISTS (SELECT 1 FROM background_task_delivery d WHERE d.id=background_task_index.id))) ORDER BY CASE WHEN status IN ('queued','running') THEN 0 ELSE 1 END,rowid LIMIT 256")
            .bind(sid).bind(sid).fetch_all(&self.pool).await?;
        let mut tasks=Vec::new();
        for id in ids { if let Some(task)=self.background_get(&id).await? { tasks.push(task); } }
        Ok(tasks)
    }
    pub(crate) async fn background_claim_delivery(
        &self, tx: &mut sqlx::Transaction<'_,sqlx::Sqlite>, run: &Value,
    ) -> Result<()> {
        let Some(ids)=run["background_delivered"].as_array() else {return Ok(());};
        let rid=run["id"].as_str().ok_or_else(||anyhow!("Missing delivery run"))?;
        for id in ids {
            let id=id.as_str().ok_or_else(||anyhow!("Invalid delivery identity"))?;
            let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("background_task:{id}")).fetch_one(&mut **tx).await?;
            let mut task=self.unseal(&raw)?;
            if task["user_id"]!=self.user_id || task["session_id"]!=run["session_id"] || task["status"]!="completed" {
                return Err(anyhow!("Background delivery scope mismatch"));
            }
            if let Some(owner)=task["delivery_run_id"].as_str() {
                if owner!=rid {return Err(anyhow!("Background result already delivered"));}
            }
            sqlx::query("INSERT OR IGNORE INTO background_task_delivery(id,run_id) VALUES(?,?)")
                .bind(id).bind(rid).execute(&mut **tx).await?;
            let owner:String=sqlx::query_scalar("SELECT run_id FROM background_task_delivery WHERE id=?")
                .bind(id).fetch_one(&mut **tx).await?;
            if owner!=rid {return Err(anyhow!("Background result already delivered"));}
            if task["delivery_run_id"]!=rid {
                task["delivery_run_id"]=json!(rid);
                let revision=task["revision"].as_u64().unwrap_or(0)+1;
                task["revision"]=json!(revision);
                sqlx::query("UPDATE background_task_index SET revision=? WHERE id=?")
                    .bind(revision as i64).bind(id).execute(&mut **tx).await?;
                sqlx::query("UPDATE docs SET value=? WHERE key=?")
                    .bind(self.seal(&task)?).bind(format!("background_task:{id}")).execute(&mut **tx).await?;
            }
        }
        Ok(())
    }
    pub(crate) async fn background_cas(&self, task: &Value, expected: Option<u64>) -> Result<bool> {
        self.background_index().await?;
        let id=task["id"].as_str().ok_or_else(||anyhow!("Missing task ID"))?;
        if task["user_id"] != self.user_id { return Err(anyhow!("Task owner mismatch")); }
        let mut tx=self.pool.begin().await?;
        let changed=if let Some(revision)=expected {
            sqlx::query("UPDATE background_task_index SET revision=revision+1,status=? WHERE id=? AND revision=?")
                .bind(task["status"].as_str().unwrap_or("cancelled")).bind(id).bind(revision as i64)
                .execute(&mut *tx).await?.rows_affected()
        } else {
            sqlx::query("INSERT OR IGNORE INTO background_task_index(id,session_id,revision,status) SELECT ?,?,0,? WHERE (SELECT COUNT(*) FROM background_task_index b WHERE b.status IN ('queued','running') OR (b.status='completed' AND NOT EXISTS (SELECT 1 FROM background_task_delivery d WHERE d.id=b.id)))<256")
                .bind(id).bind(task["session_id"].as_str().unwrap_or("")).bind(task["status"].as_str().unwrap_or("queued"))
                .execute(&mut *tx).await?.rows_affected()
        };
        if changed==0 {return Ok(false);}
        let mut saved=task.clone(); saved["revision"]=json!(expected.map_or(0,|value|value+1));
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(format!("background_task:{id}")).bind(self.seal(&saved)?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(true)
    }
}

#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn delivery_is_atomic_exclusive_and_encrypted() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let task=json!({"id":"delivery","user_id":"admin","session_id":"s","status":"completed","revision":0,"result":"private evidence"});
        assert!(store.background_cas(&task,None).await.unwrap());
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='background_task:delivery'").fetch_one(&store.pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
        let run=json!({"id":"first","session_id":"s","background_delivered":["delivery"]});
        let mut tx=store.pool.begin().await.unwrap();
        store.background_claim_delivery(&mut tx,&run).await.unwrap();tx.rollback().await.unwrap();
        assert!(store.background_get("delivery").await.unwrap().unwrap()["delivery_run_id"].is_null());
        let mut tx=store.pool.begin().await.unwrap();
        store.background_claim_delivery(&mut tx,&run).await.unwrap();tx.commit().await.unwrap();
        let mut other=run.clone();other["id"]=json!("second");
        let mut tx=store.pool.begin().await.unwrap();
        assert!(store.background_claim_delivery(&mut tx,&other).await.is_err());tx.rollback().await.unwrap();
        assert!(store.background_scan(Some("s")).await.unwrap().is_empty());
    }
}
