//! Files attachment repository on the existing user-plane pool, cache and Store codec.
//! Attachment bytes live as encrypted files in `user_data/<user>/blobs/<attachment id>`;
//! records hold metadata only. Write the file before committing its record and remove it
//! after the record is gone, so a crash can leave only an unreferenced file.
use super::*;
use base64::{engine::general_purpose::STANDARD, Engine};

impl Store {
    fn blob_dir(&self) -> std::path::PathBuf {
        self.root
            .join("user_data")
            .join(&self.user_id)
            .join("blobs")
    }
    fn blob_aad(&self) -> Vec<u8> {
        format!("v3-attachment-file-v1:{}", self.user_id).into_bytes()
    }
    /// Encrypt and durably write one attachment file (temporary name, fsync, rename).
    pub(crate) async fn write_blob(&self, aid: &str, media: &str, bytes: &[u8]) -> Result<()> {
        let compression = webagent_storage::compression::Compression::current();
        let plain = if compression.applies_to_media(media) {
            compression.pack(bytes.to_vec())?
        } else {
            bytes.to_vec()
        };
        let sealed = self.codec.seal_bytes(&self.blob_aad(), &plain)?;
        let dir = self.blob_dir();
        let aid = aid.to_owned();
        tokio::task::spawn_blocking(move || -> Result<()> {
            use std::io::Write;
            std::fs::create_dir_all(&dir)?;
            let temporary = dir.join(format!(".{aid}.tmp"));
            // Owner-only: other local accounts never need these files.
            let mut file = {
                use webagent_platform::fs::OpenOptionsExt;
                std::fs::OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .mode(0o600)
                    .open(&temporary)?
            };
            file.write_all(&sealed)?;
            file.sync_all()?;
            std::fs::rename(&temporary, dir.join(&aid))?;
            Ok(())
        })
        .await?
    }
    /// The record with its bytes in `data` (memory only). Schema 1 and 2 records already carry them.
    pub(crate) async fn attachment_with_data(&self, mut record: Value) -> Result<Value> {
        if record["schema_version"] == 3 {
            let aid = record["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Invalid attachment"))?;
            webagent_files::uploads::upload_id_ok(aid)
                .then_some(())
                .ok_or_else(|| anyhow!("Invalid attachment"))?;
            let sealed = tokio::fs::read(self.blob_dir().join(aid))
                .await
                .map_err(|_| anyhow!("Attachment bytes unavailable"))?;
            let bytes = webagent_storage::compression::unpack(
                self.codec.unseal_bytes(&self.blob_aad(), &sealed)?,
            )?;
            record["data"] = json!(STANDARD.encode(bytes));
        }
        Ok(record)
    }
    /// Decoded bytes of any attachment schema.
    pub(crate) async fn attachment_bytes(&self, record: &Value) -> Result<Vec<u8>> {
        webagent_files::uploads::content_bytes(&self.attachment_with_data(record.clone()).await?)
    }
    /// Give a copied record its own file name; a missing source (legacy record) is skipped.
    pub(crate) fn link_blob(&self, from: &str, to: &str) -> Result<()> {
        let dir = self.blob_dir();
        match webagent_platform::link_or_copy(&dir.join(from), &dir.join(to)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => Ok(result?),
        }
    }
    /// Remove files whose records were deleted; missing files are already gone.
    pub(crate) fn remove_blobs<'a>(&self, ids: impl IntoIterator<Item = &'a str>) {
        let dir = self.blob_dir();
        for aid in ids {
            if webagent_files::uploads::upload_id_ok(aid) {
                let _ = std::fs::remove_file(dir.join(aid));
            }
        }
    }
    /// Bytes in the private browser folders and Sandbox ability folders of this user's
    /// sessions, as the worker publishes them. The rest of `/workspaces` is not counted:
    /// only administrator app-development tools write there, and that work is not the
    /// user's own storage.
    async fn worker_used(&self) -> Result<u64> {
        Ok(self
            .folder_bytes()
            .await?
            .values()
            .fold(0, |total, bytes| total.saturating_add(*bytes)))
    }
    /// Bytes of each of this user's sessions' browser and Sandbox folders, as the worker
    /// publishes them.
    pub(crate) async fn folder_bytes(&self) -> Result<std::collections::HashMap<String, u64>> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT key FROM docs WHERE key LIKE 'session:%'")
                .fetch_all(&self.pool)
                .await?;
        let published = |variable: &str, file: &str| {
            std::env::var_os(variable).map_or_else(
                || {
                    webagent_configuration::paths::runtime()
                        .join("worker")
                        .join(file)
                },
                std::path::PathBuf::from,
            )
        };
        let files = [
            published("V3_BROWSER_USAGE", "browser-usage.json"),
            published("V3_SANDBOX_USAGE", "sandbox-usage.json"),
        ];
        Ok(tokio::task::spawn_blocking(move || {
            let mut sessions = std::collections::HashMap::new();
            for usage in &files {
                // Small (one number per session folder); missing until the worker first writes it.
                let folders: std::collections::HashMap<String, u64> = std::fs::read(usage)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
                    .unwrap_or_default();
                for sid in ids.iter().filter_map(|key| key.strip_prefix("session:")) {
                    if let Some(bytes) = folders.get(sid).filter(|bytes| **bytes > 0) {
                        let total: &mut u64 = sessions.entry(sid.to_owned()).or_default();
                        *total = total.saturating_add(*bytes);
                    }
                }
            }
            sessions
        })
        .await?)
    }
    /// Bytes this user stores: the database and its journal, attachment files, browser
    /// folders, Sandbox folders and saved UI elements.
    pub(crate) async fn storage_used(&self) -> Result<u64> {
        let files = self
            .get(webagent_files::QUOTA_KEY)
            .await?
            .and_then(|quota| quota["bytes"].as_u64())
            .unwrap_or(0);
        let dir = self.root.join("user_data").join(&self.user_id);
        let mut database = 0u64;
        for suffix in ["db", "db-wal"] {
            if let Ok(meta) =
                tokio::fs::metadata(dir.join(format!("{}.{suffix}", self.user_id))).await
            {
                database = database.saturating_add(meta.len());
            }
        }
        Ok(files
            .saturating_add(database)
            .saturating_add(self.worker_used().await?)
            .saturating_add({
                let (root, user) = (self.root.clone(), self.user_id.clone());
                tokio::task::spawn_blocking(move || {
                    webagent_native_tools::sandbox::elements::user_bytes(&root, &user)
                })
                .await?
            }))
    }
    /// Remove only an unclaimed upload owned by this session and decrement its quota atomically.
    pub(crate) async fn delete_pending_upload(&self, sid: &str, aid: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        let Some(record) =
            webagent_files::repository::delete_pending_upload(self, &mut tx, sid, aid).await?
        else {
            return Ok(());
        };
        tx.commit().await?;
        let mut keys = vec![
            webagent_files::attachment_key(aid),
            webagent_files::QUOTA_KEY.into(),
        ];
        if let Some(agent) = record["agent_id"].as_str() {
            keys.push(webagent_files::agent_quota_key(agent));
        }
        self.cache.lock().unwrap().invalidate(&keys);
        self.remove_blobs([aid]);
        Ok(())
    }
}

/// Apparent size of a directory tree without following links; unreadable entries count as zero.
fn tree_bytes(path: &std::path::Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| tree_bytes(&entry.path()))
                .fold(0u64, u64::saturating_add)
        })
        .unwrap_or(0)
}

impl Store {
    /// Size of this user's database and journal files.
    fn database_bytes(&self) -> u64 {
        let dir = self.root.join("user_data").join(&self.user_id);
        ["db", "db-wal"]
            .iter()
            .filter_map(|suffix| {
                std::fs::metadata(dir.join(format!("{}.{suffix}", self.user_id))).ok()
            })
            .map(|meta| meta.len())
            .sum()
    }
    /// Rewrite this user's stored records with the current codec settings (compression),
    /// optionally deleting every attachment first, then reclaim free pages. Values are
    /// unchanged: only their stored encoding is. Returns sizes before and after.
    pub(crate) async fn compact_storage(&self, delete_attachments: bool) -> Result<Value> {
        let before = self.storage_used().await?;
        let database_before = self.database_bytes();
        let mut removed_attachments = 0u64;
        if delete_attachments {
            let keys: Vec<String> = sqlx::query_scalar(
                "SELECT key FROM docs WHERE key GLOB 'attachment:*' OR key GLOB 'attachment_quota*' \
                 OR key GLOB 'message_attachments:*' OR key GLOB 'reply_attachments:*'",
            )
            .fetch_all(&self.pool)
            .await?;
            removed_attachments =
                keys.iter().filter(|k| k.starts_with("attachment:")).count() as u64;
            let mut tx = self.pool.begin().await?;
            for key in &keys {
                sqlx::query("DELETE FROM docs WHERE key=?")
                    .bind(key)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
            self.cache.lock().unwrap().invalidate(&keys);
            let _ = std::fs::remove_dir_all(self.blob_dir());
        }
        let (runs_trimmed, progress_removed) = self.prune_finished_runs().await?;
        let idle_days = webagent_files::storage_policy::StoragePolicy::live()
            .map(|policy| policy.trim_idle_days)
            .unwrap_or(7);
        let tool_calls_trimmed = self
            .trim_history(idle_days, webagent_host_core::now() as i64)
            .await?;
        let mut rewritten = 0u64;
        for (table, id, column) in [
            ("docs", "rowid", "value"),
            ("chat", "seq", "content"),
            ("interactions", "seq", "item"),
            ("events", "seq", "payload"),
            ("recovery_input", "rowid", "item"),
            ("loop_events", "seq", "payload"),
            ("loop_commands", "seq", "payload"),
            ("publication_outbox", "rowid", "payload"),
            ("reasoning_cleanup", "rowid", "payload"),
        ] {
            // Lazily created tables may not exist yet for this user.
            let exists: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?",
            )
            .bind(table)
            .fetch_one(&self.pool)
            .await?;
            if exists == 0 {
                continue;
            }
            let mut last = 0i64;
            loop {
                let rows = sqlx::query(&format!(
                    "SELECT {id} AS id,{column} AS value FROM {table} WHERE {id}>? ORDER BY {id} LIMIT 200"
                ))
                .bind(last)
                .fetch_all(&self.pool)
                .await?;
                let Some(end) = rows.last().map(|row| row.get::<i64, _>("id")) else {
                    break;
                };
                let mut tx = self.pool.begin().await?;
                for row in &rows {
                    // Rows another codec owns (agent envelopes) are left as they are.
                    let Ok(value) = self.unseal(&row.get::<Vec<u8>, _>("value")) else {
                        continue;
                    };
                    sqlx::query(&format!("UPDATE {table} SET {column}=? WHERE {id}=?"))
                        .bind(self.encode_user_record(&value)?)
                        .bind(row.get::<i64, _>("id"))
                        .execute(&mut *tx)
                        .await?;
                    rewritten += 1;
                }
                tx.commit().await?;
                last = end;
            }
        }
        let manifests = self.refresh_recovery_manifests().await?;
        // Free pages return to the filesystem only after a vacuum; a busy database keeps them.
        // Its temporary copy goes beside the database: the container's /tmp is too small.
        let vacuum = async {
            let mut connection = self.pool.acquire().await?;
            let dir = self.root.join("user_data").join(&self.user_id);
            let dir = dir.display().to_string().replace('\'', "''");
            sqlx::query(&format!("PRAGMA temp_store_directory='{dir}'"))
                .execute(&mut *connection)
                .await?;
            let result = sqlx::query("VACUUM").execute(&mut *connection).await;
            let _ = sqlx::query("PRAGMA temp_store_directory=''")
                .execute(&mut *connection)
                .await;
            result.map(|_| ())
        }
        .await
        .err()
        .map(|error: sqlx::Error| error.to_string());
        let _ = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(&self.pool)
            .await;
        Ok(
            json!({"used_bytes_before":before,"used_bytes_after":self.storage_used().await?,
            "database_bytes_before":database_before,"database_bytes_after":self.database_bytes(),
            "records_rewritten":rewritten,"recovery_manifests_refreshed":manifests,"attachments_deleted":removed_attachments,
            "finished_runs_trimmed":runs_trimmed,"progress_updates_removed":progress_removed,
            "tool_calls_trimmed":tool_calls_trimmed,"vacuum_error":vacuum}),
        )
    }
    /// Bring records saved before finished runs were trimmed to the current shape: empty each
    /// finished run's input copy, drop its duplicate-result tracking, and keep only the newest
    /// progress update per run. Returns (runs trimmed, progress updates removed).
    async fn prune_finished_runs(&self) -> Result<(u64, u64)> {
        let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM docs WHERE key GLOB 'run:*'")
            .fetch_all(&self.pool)
            .await?;
        let mut trimmed = 0u64;
        for key in keys {
            // Read then write: take the write lock first so a concurrent run save makes this wait,
            // not fail with "database is locked" on the upgrade.
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(&key)
                .fetch_optional(&mut *tx)
                .await?
            else {
                continue;
            };
            let Ok(mut run) = self.unseal(&raw) else {
                continue;
            };
            let Some(rid) = run["id"].as_str().map(str::to_owned) else {
                continue;
            };
            let finished = matches!(
                run["status"].as_str(),
                Some("completed" | "cancelled" | "failed")
            );
            let has_copy = run["_recovery_input"]["count"].as_u64().unwrap_or(0) > 0
                || run["input"]
                    .as_array()
                    .is_some_and(|items| !items.is_empty());
            if !finished || (!has_copy && run.get("tool_result_projection").is_none()) {
                continue;
            }
            sqlx::query("DELETE FROM recovery_input WHERE run_id=?")
                .bind(&rid)
                .execute(&mut *tx)
                .await?;
            let fields = run.as_object_mut().expect("run record is an object");
            fields.remove("tool_result_projection");
            fields.remove("input");
            fields.insert(
                "_recovery_input".into(),
                json!({"version":1,"count":0,"manifest":[]}),
            );
            // Same encoding as `write_document`, without its live-run admission checks.
            sqlx::query("UPDATE docs SET value=? WHERE key=?")
                .bind(self.encode_user_record(&run)?)
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            self.cache.lock().unwrap().invalidate(&[key]);
            trimmed += 1;
        }
        // Updates written before the run id column carry their run only inside the payload.
        let rows = sqlx::query(
            "SELECT seq,run_id,payload FROM events WHERE kind='run_status' ORDER BY seq",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut newest = std::collections::HashMap::new();
        for row in &rows {
            let run = match row.get::<Option<String>, _>("run_id") {
                Some(run) => run,
                None => match self.unseal(&row.get::<Vec<u8>, _>("payload")) {
                    Ok(payload) => match payload["id"].as_str() {
                        Some(run) => run.to_owned(),
                        None => continue,
                    },
                    Err(_) => continue,
                },
            };
            newest.insert(run, row.get::<i64, _>("seq"));
        }
        let kept: std::collections::HashSet<i64> = newest.values().copied().collect();
        let mut tx = self.pool.begin().await?;
        let mut removed = 0u64;
        for row in &rows {
            let seq = row.get::<i64, _>("seq");
            if !kept.contains(&seq) {
                removed += sqlx::query("DELETE FROM events WHERE seq=?")
                    .bind(seq)
                    .execute(&mut *tx)
                    .await?
                    .rows_affected();
            }
        }
        for (run, seq) in &newest {
            sqlx::query("UPDATE events SET run_id=? WHERE seq=?")
                .bind(run)
                .bind(seq)
                .execute(&mut *tx)
                .await?;
        }
        removed += sqlx::query(
            "DELETE FROM loop_events WHERE kind='run_status' AND seq NOT IN \
             (SELECT MAX(seq) FROM loop_events WHERE kind='run_status' GROUP BY run_id)",
        )
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok((trimmed, removed))
    }
    /// Recovery metadata commits to each `recovery_input` row's stored bytes. After those rows are
    /// re-encoded, refresh the stored-bytes hashes, first proving each row still holds exactly the
    /// committed content. A content mismatch is left untouched and reported by the recovery reader.
    async fn refresh_recovery_manifests(&self) -> Result<u64> {
        use webagent_storage::recovery::{
            recovery_digest, recovery_item_digest, recovery_manifest,
        };
        let keys: Vec<String> = sqlx::query_scalar("SELECT key FROM docs WHERE key GLOB 'run:*'")
            .fetch_all(&self.pool)
            .await?;
        let mut refreshed = 0u64;
        for key in keys {
            // Read then write: take the write lock first so a concurrent run save makes this wait,
            // not fail with "database is locked" on the upgrade.
            let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
            let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                .bind(&key)
                .fetch_optional(&mut *tx)
                .await?
            else {
                continue;
            };
            let Ok(mut run) = self.unseal(&raw) else {
                continue;
            };
            let Some(reference) = run.get("_recovery_input").cloned() else {
                continue;
            };
            let Some(rid) = run["id"].as_str().map(str::to_owned) else {
                continue;
            };
            let rows = sqlx::query(
                "SELECT ordinal,item FROM recovery_input WHERE run_id=? ORDER BY ordinal",
            )
            .bind(&rid)
            .fetch_all(&mut *tx)
            .await?;
            let Ok(Some(mut pairs)) = recovery_manifest(&reference, rows.len()) else {
                continue;
            };
            let mut changed = false;
            let mut intact = true;
            for (ordinal, row) in rows.iter().enumerate() {
                let sealed = row.get::<Vec<u8>, _>("item");
                let sealed_hash = recovery_digest(&sealed);
                if pairs[ordinal].1 == sealed_hash {
                    continue;
                }
                match self.unseal(&sealed).map(|item| recovery_item_digest(&item)) {
                    Ok(Ok(plain)) if plain == pairs[ordinal].0 => {
                        pairs[ordinal].1 = sealed_hash;
                        changed = true;
                    }
                    _ => {
                        intact = false;
                        break;
                    }
                }
            }
            if changed && intact {
                run["_recovery_input"]["manifest"] = json!(pairs);
                // Same encoding as `write_document`, without its live-run admission checks.
                sqlx::query("UPDATE docs SET value=? WHERE key=?")
                    .bind(self.encode_user_record(&run)?)
                    .bind(&key)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                self.cache.lock().unwrap().invalidate(&[key]);
                refreshed += 1;
            }
        }
        Ok(refreshed)
    }
    /// Refuse agent-plane writes once the agent's data folder reaches its owner's tier limit.
    pub(crate) async fn check_agent_folder(
        &self,
        agent: &str,
    ) -> Result<std::result::Result<(), String>> {
        let Some(owner): Option<String> =
            sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
                .bind(agent)
                .fetch_optional(&self.app_pool)
                .await?
        else {
            return Ok(Ok(()));
        };
        let Some(account) = self.account_by_id(&owner).await? else {
            return Ok(Ok(()));
        };
        let tier = crate::access::Principal::from_account(&account)?.tier_id;
        let Some(limit) = webagent_files::storage_policy::StoragePolicy::live()?.agent_limit(&tier)
        else {
            return Ok(Ok(()));
        };
        let dir = self.root.join("agent_data").join(agent);
        let used = tokio::task::spawn_blocking(move || tree_bytes(&dir)).await?;
        Ok(if used >= limit {
            Err(format!(
                "Agent storage limit reached: {} of {} MB used",
                used.div_ceil(1 << 20),
                limit >> 20
            ))
        } else {
            Ok(())
        })
    }
}
