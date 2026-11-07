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
                use std::os::unix::fs::OpenOptionsExt;
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
        match std::fs::hard_link(dir.join(from), dir.join(to)) {
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
    /// Bytes in the workspaces of this user's sessions (`/workspaces/<session id>`).
    async fn workspace_used(&self) -> Result<u64> {
        let ids: Vec<String> =
            sqlx::query_scalar("SELECT key FROM docs WHERE key LIKE 'session:%'")
                .fetch_all(&self.pool)
                .await?;
        let root = std::path::PathBuf::from(
            std::env::var("V3_WORKSPACE_ROOT").unwrap_or_else(|_| "/workspaces".into()),
        );
        Ok(tokio::task::spawn_blocking(move || {
            ids.iter()
                .filter_map(|key| key.strip_prefix("session:"))
                .filter(|sid| webagent_files::uploads::upload_id_ok(sid))
                .map(|sid| tree_bytes(&root.join(sid)))
                .fold(0u64, u64::saturating_add)
        })
        .await?)
    }
    /// Bytes this user stores: the database and its journal, attachment files and workspaces.
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
            .saturating_add(self.workspace_used().await?))
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
