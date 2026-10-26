//! AES-256-GCM records under the installation key, the versioned administrator
//! plaintext envelope and per-agent data keys. Formats are on-disk contracts.
use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm, Nonce,
};
use anyhow::{anyhow, Result};
use rand::RngCore;
use serde_json::{json, Value};
use sqlx::{Row, SqlitePool};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::{Arc, RwLock},
};

const AGENT_ENVELOPE: &[u8] = b"WAAGENT1";

/// Publish one complete private encryption key atomically; never replace a missing key for existing databases.
pub fn load_or_create_key(root: &Path) -> Result<Vec<u8>> {
    let keypath = root.join("encryption.key");
    let key = match std::fs::read(&keypath) {
        Ok(key) => key,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if [
                "runtime.sqlite",
                "runtime.sqlite-wal",
                "runtime.sqlite-shm",
                "db/app.db",
                "user_data/admin/admin.db",
                "agent_data/shared_default/shared_default.db",
            ]
            .iter()
            .any(|name| root.join(name).exists())
            {
                return Err(anyhow!(
                    "Storage encryption key is missing for existing database"
                ));
            }
            let mut key = vec![0; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let temp_path = root.join(format!(".encryption.key.{}.tmp", uuid::Uuid::new_v4()));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temp_path)?;
            let published = (|| -> Result<bool> {
                file.write_all(&key)?;
                file.sync_all()?;
                match std::fs::hard_link(&temp_path, &keypath) {
                    Ok(()) => {
                        File::open(root)?.sync_all()?;
                        Ok(true)
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                    Err(e) => Err(e.into()),
                }
            })();
            drop(file);
            let _ = std::fs::remove_file(&temp_path);
            if published? {
                key
            } else {
                std::fs::read(&keypath)?
            }
        }
        Err(e) => return Err(e.into()),
    };
    if key.len() != 32 {
        return Err(anyhow!("Invalid storage encryption key"));
    }
    Ok(key)
}

/// Installation-key codec shared by every storage view of one data root.
#[derive(Clone)]
pub struct Codec {
    cipher: Aes256Gcm,
}

impl Codec {
    /// Build the codec from a 32-byte installation key.
    pub fn new(key: &[u8]) -> Result<Self> {
        Ok(Self {
            cipher: Aes256Gcm::new_from_slice(key)
                .map_err(|_| anyhow!("Invalid encryption key"))?,
        })
    }
    /// Validate the plaintext policy, then load or first-start publish the data-root key.
    pub fn open(root: &Path) -> Result<Self> {
        crate::settings::admin_encryption_enabled(&crate::settings::read_app_settings(root)?)?;
        Self::new(&load_or_create_key(root)?)
    }
    /// Encode a JSON value with a fresh AES-GCM nonce and authenticated ciphertext.
    pub fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut data = nonce.to_vec();
        data.extend(
            self.cipher
                .encrypt(
                    Nonce::from_slice(&nonce),
                    serde_json::to_vec(value)?.as_ref(),
                )
                .map_err(|_| anyhow!("Encryption failed"))?,
        );
        Ok(data)
    }
    /// Authenticate either an installation-key record or the explicitly versioned
    /// administrator plaintext envelope. Agent envelopes are decoded by `AgentCipher`.
    pub fn unseal(&self, data: &[u8]) -> Result<Value> {
        // A nonce in the legacy binary format can begin with '{'; only a
        // parsed, explicitly versioned envelope selects the plaintext reader.
        if let Ok(record) = serde_json::from_slice::<Value>(data) {
            if record["format"] == "v3-admin-plaintext-v1" {
                let nonce: [u8; 12] = serde_json::from_value(record["nonce"].clone())?;
                let tag: Vec<u8> = serde_json::from_value(record["tag"].clone())?;
                let value = record
                    .get("value")
                    .ok_or_else(|| anyhow!("Invalid plaintext record"))?;
                let aad = serde_json::to_vec(&json!(["v3-admin-plaintext-v1", value]))?;
                self.cipher
                    .decrypt(
                        Nonce::from_slice(&nonce),
                        Payload {
                            msg: &tag,
                            aad: &aad,
                        },
                    )
                    .map_err(|_| anyhow!("Storage authentication failed"))?;
                return Ok(value.clone());
            }
        }
        if data.len() < 28 {
            return Err(anyhow!("Invalid encrypted record"));
        }
        let plain = self
            .cipher
            .decrypt(Nonce::from_slice(&data[..12]), &data[12..])
            .map_err(|_| anyhow!("Storage authentication failed"))?;
        Ok(serde_json::from_slice(&plain)?)
    }
    /// User-plane write policy. Only the fixed administrator account may store
    /// authenticated plaintext, and only while app-settings disables encryption.
    /// Application/agent secrets and search cursors keep using `seal`.
    pub fn encode_user_record(&self, root: &Path, user_id: &str, value: &Value) -> Result<Vec<u8>> {
        if user_id != "admin" {
            return self.seal(value);
        }
        if crate::settings::admin_encryption_enabled(&crate::settings::read_app_settings(root)?)? {
            return self.seal(value);
        }
        let mut nonce = [0; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let aad = serde_json::to_vec(&json!(["v3-admin-plaintext-v1", value]))?;
        let tag = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &[],
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("Storage authentication failed"))?;
        Ok(serde_json::to_vec(
            &json!({"format":"v3-admin-plaintext-v1","value":value,"nonce":nonce,"tag":tag}),
        )?)
    }
}

/// Agent records carry a fixed prefix so readers select the agent key.
pub fn is_agent_envelope(data: &[u8]) -> bool {
    data.starts_with(AGENT_ENVELOPE)
}

/// One agent's random data key. Clones share the expanded key schedule instead
/// of copying it into futures; the agent identity is bound as associated data.
#[derive(Clone)]
pub struct AgentCipher(Arc<Aes256Gcm>);

/// Process-wide registry of unwrapped agent keys shared by Store views.
pub type AgentKeys = Arc<RwLock<HashMap<String, AgentCipher>>>;

impl AgentCipher {
    /// Build an agent cipher from its 32-byte data key.
    pub fn new(key: &[u8]) -> Result<Self> {
        Ok(Self(Arc::new(
            Aes256Gcm::new_from_slice(key).map_err(|_| anyhow!("Invalid agent key"))?,
        )))
    }
    /// Seal a value for `agent`; another agent's key or identity cannot open it.
    pub fn seal(&self, agent: &str, value: &Value) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        rand::thread_rng().fill_bytes(&mut nonce);
        let mut result = AGENT_ENVELOPE.to_vec();
        result.extend(nonce);
        result.extend(
            self.0
                .encrypt(
                    Nonce::from_slice(&nonce),
                    Payload {
                        msg: &serde_json::to_vec(value)?,
                        aad: agent.as_bytes(),
                    },
                )
                .map_err(|_| anyhow!("Agent encryption failed"))?,
        );
        Ok(result)
    }
    /// Authenticate an agent envelope against this key and agent identity.
    pub fn unseal(&self, agent: &str, data: &[u8]) -> Result<Value> {
        if data.len() < 36 || !is_agent_envelope(data) {
            return Err(anyhow!("Invalid agent record envelope"));
        }
        let plain = self
            .0
            .decrypt(
                Nonce::from_slice(&data[8..20]),
                Payload {
                    msg: &data[20..],
                    aad: agent.as_bytes(),
                },
            )
            .map_err(|_| anyhow!("Agent record authentication failed"))?;
        Ok(serde_json::from_slice(&plain)?)
    }
}

/// Load the agent's wrapped data key from its own database, or create it and
/// convert that agent's fixed-schema payloads in the same transaction. A
/// completed conversion is never repaired by silently creating another key.
pub async fn initialize_agent_key(
    pool: &SqlitePool,
    codec: &Codec,
    agent_id: &str,
) -> Result<AgentCipher> {
    sqlx::query("CREATE TABLE IF NOT EXISTS agent_key_envelopes(id INTEGER PRIMARY KEY CHECK(id=1),envelope BLOB NOT NULL)")
        .execute(pool).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE agents SET revision=revision WHERE id=?")
        .bind(agent_id)
        .execute(&mut *tx)
        .await?;
    let envelope: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT envelope FROM agent_key_envelopes WHERE id=1")
            .fetch_optional(&mut *tx)
            .await?;
    let (key, convert) = match envelope {
        Some(bytes) => {
            if is_agent_envelope(&bytes) {
                return Err(anyhow!("Agent key unavailable"));
            }
            let value = codec.unseal(&bytes)?;
            if value["version"] != 1 || value["agent_id"] != agent_id {
                return Err(anyhow!("Agent key identity mismatch"));
            }
            let key: Vec<u8> = serde_json::from_value(value["key"].clone())?;
            (key, false)
        }
        None => {
            let exists: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
                .bind(format!("agent:{agent_id}:encryption_v1"))
                .fetch_optional(&mut *tx)
                .await?;
            if exists.is_some() {
                return Err(anyhow!("Agent key missing for existing encrypted data"));
            }
            let mut key = vec![0; 32];
            rand::thread_rng().fill_bytes(&mut key);
            let wrapped = codec.seal(&json!({"version":1,"agent_id":agent_id,"key":key}))?;
            sqlx::query("INSERT INTO agent_key_envelopes VALUES(1,?)")
                .bind(wrapped)
                .execute(&mut *tx)
                .await?;
            (key, true)
        }
    };
    let cipher = AgentCipher::new(&key)?;
    if convert {
        // Fixed schema only; convert each payload and its key envelope in one transaction.
        for (table, column) in [
            ("agents", "metadata"),
            ("agent_connections", "config"),
            ("agent_prompts", "content"),
            ("docs", "value"),
            ("prompt_slots", "content"),
            ("agent_memberships", "value"),
        ] {
            let exists: Option<String> =
                sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type='table' AND name=?")
                    .bind(table)
                    .fetch_optional(&mut *tx)
                    .await?;
            if exists.is_none() {
                continue;
            }
            let query = match table {
                "agents" => format!("SELECT rowid,{column} FROM {table} WHERE id=?"),
                "docs" => format!("SELECT rowid,{column} FROM {table} WHERE key=? OR key=? OR substr(key,1,?)=? OR key LIKE 'capability_audit:%' OR key LIKE 'ability_policy_audit:%' OR key LIKE 'ability_visibility_audit:%'"),
                "prompt_slots" => format!("SELECT rowid,{column} FROM {table}"),
                _ => format!("SELECT rowid,{column} FROM {table} WHERE agent_id=?"),
            };
            let query = sqlx::query(&query);
            let query = if table == "docs" {
                let prefix = format!("agent:{agent_id}:");
                query
                    .bind(format!("agent:{agent_id}"))
                    .bind(format!("agent_abilities:{agent_id}"))
                    .bind(prefix.len() as i64)
                    .bind(prefix)
            } else if table == "prompt_slots" {
                query
            } else {
                query.bind(agent_id)
            };
            let rows = query.fetch_all(&mut *tx).await?;
            for row in rows {
                let bytes: Vec<u8> = row.get(column);
                let value = if is_agent_envelope(&bytes) {
                    cipher.unseal(agent_id, &bytes)?
                } else {
                    codec.unseal(&bytes)?
                };
                let sealed = cipher.seal(agent_id, &value)?;
                sqlx::query(&format!("UPDATE {table} SET {column}=? WHERE rowid=?"))
                    .bind(&sealed)
                    .bind(row.get::<i64, _>("rowid"))
                    .execute(&mut *tx)
                    .await?;
                if cipher.unseal(agent_id, &sealed)? != value {
                    return Err(anyhow!("Agent conversion verification failed"));
                }
            }
        }
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!("agent:{agent_id}:encryption_v1"))
            .bind(cipher.seal(agent_id, &json!({"version":1}))?)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(cipher)
}
