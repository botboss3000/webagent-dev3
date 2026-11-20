//! Storage-limit records in the user's own database: what each session takes up, removing a
//! session's files while keeping the session, and the `storage_state` the limit service
//! keeps (level reached, whether the account is full).
use super::*;
use std::collections::HashMap;

pub(crate) const STATE_KEY: &str = "storage_state";
pub(crate) const POLICY_KEY: &str = "storage_cleanup_policy";

impl Store {
    /// Bytes each session takes up: its history, messages and events as stored, its files,
    /// and its browser and Sandbox folders.
    pub(crate) async fn session_bytes(&self) -> Result<HashMap<String, u64>> {
        let mut sizes: HashMap<String, u64> = HashMap::new();
        for (table, column) in [
            ("interactions", "item"),
            ("chat", "content"),
            ("events", "payload"),
        ] {
            let rows = sqlx::query(&format!(
                "SELECT session_id AS sid,SUM(LENGTH({column})) AS bytes FROM {table} GROUP BY session_id"
            ))
            .fetch_all(&self.pool)
            .await?;
            for row in rows {
                let Some(sid) = row.get::<Option<String>, _>("sid") else {
                    continue;
                };
                *sizes.entry(sid).or_default() +=
                    row.get::<Option<i64>, _>("bytes").unwrap_or(0) as u64;
            }
        }
        for record in self.list("attachment:").await? {
            if let Some(sid) = record["session_id"].as_str() {
                *sizes.entry(sid.to_owned()).or_default() += record["bytes"].as_u64().unwrap_or(0);
            }
        }
        for (sid, bytes) in self.folder_bytes().await? {
            *sizes.entry(sid).or_default() += bytes;
        }
        Ok(sizes)
    }

    /// Bytes and count of the files attached in session `sid`.
    pub(crate) async fn session_file_bytes(&self) -> Result<HashMap<String, (u64, u64)>> {
        let mut files: HashMap<String, (u64, u64)> = HashMap::new();
        for record in self.list("attachment:").await? {
            if let Some(sid) = record["session_id"].as_str() {
                let entry = files.entry(sid.to_owned()).or_default();
                entry.0 += 1;
                entry.1 += record["bytes"].as_u64().unwrap_or(0);
            }
        }
        Ok(files)
    }

    /// Delete the files attached in session `sid`, keeping the session and its messages
    /// (their files then show as removed). Returns the files and bytes removed.
    pub(crate) async fn remove_session_files(&self, sid: &str) -> Result<(u64, u64)> {
        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        let rows = sqlx::query("SELECT key,value FROM docs WHERE key GLOB 'attachment:*'")
            .fetch_all(&mut *tx)
            .await?;
        let mut removed = Vec::new();
        let mut bytes = 0u64;
        let mut by_agent: HashMap<String, u64> = HashMap::new();
        for row in rows {
            let key: String = row.get("key");
            let Ok(record) = self.unseal(&row.get::<Vec<u8>, _>("value")) else {
                continue;
            };
            if record["session_id"] != sid {
                continue;
            }
            let size = record["bytes"].as_u64().unwrap_or(0);
            bytes = bytes.saturating_add(size);
            if let Some(agent) = record["agent_id"].as_str() {
                *by_agent.entry(agent.to_owned()).or_default() += size;
            }
            sqlx::query("DELETE FROM docs WHERE key=?")
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            removed.push(key);
        }
        if bytes > 0 {
            if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>(
                "SELECT value FROM docs WHERE key='attachment_quota'",
            )
            .fetch_optional(&mut *tx)
            .await?
            {
                let mut quota = self.unseal(&raw)?;
                quota["bytes"] = json!(quota["bytes"].as_u64().unwrap_or(0).saturating_sub(bytes));
                self.write_document(&mut tx, webagent_files::QUOTA_KEY, &quota)
                    .await?;
                removed.push(webagent_files::QUOTA_KEY.into());
            }
            for (agent, size) in by_agent {
                let key = webagent_files::agent_quota_key(&agent);
                webagent_files::repository::subtract_quota(self, &mut tx, &key, size).await?;
                removed.push(key);
            }
        }
        tx.commit().await?;
        let count = removed
            .iter()
            .filter(|key| key.starts_with("attachment:"))
            .count() as u64;
        self.remove_blobs(
            removed
                .iter()
                .filter_map(|key| key.strip_prefix("attachment:")),
        );
        self.cache.lock().unwrap().invalidate(&removed);
        Ok((count, bytes))
    }

    /// Whether the limit service marked this account full. Read past the record cache: the
    /// loop asks while the API process writes it.
    pub(crate) async fn storage_full(&self) -> Result<bool> {
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(STATE_KEY)
            .fetch_optional(&self.pool)
            .await?;
        Ok(match raw {
            Some(raw) => self.unseal(&raw)?["full"] == true,
            None => false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn session_files_go_and_the_quota_follows() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for (aid, sid, bytes) in [("a1", "s1", 100), ("a2", "s1", 50), ("a3", "s2", 7)] {
            store
                .put(
                    &format!("attachment:{aid}"),
                    &json!({"id":aid,"session_id":sid,"agent_id":"shared_default","bytes":bytes}),
                )
                .await
                .unwrap();
        }
        store
            .put("attachment_quota", &json!({"bytes":157}))
            .await
            .unwrap();
        assert_eq!(store.session_file_bytes().await.unwrap()["s1"], (2, 150));
        assert_eq!(store.remove_session_files("s1").await.unwrap(), (2, 150));
        assert!(store.get("attachment:a1").await.unwrap().is_none());
        assert!(store.get("attachment:a3").await.unwrap().is_some());
        assert_eq!(
            store.get("attachment_quota").await.unwrap().unwrap()["bytes"],
            7
        );
        assert!(!store.storage_full().await.unwrap());
        store.put(STATE_KEY, &json!({"full":true})).await.unwrap();
        assert!(store.storage_full().await.unwrap());
    }
}
