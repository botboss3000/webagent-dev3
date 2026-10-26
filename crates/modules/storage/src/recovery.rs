//! Split recovery input: a run document's `input` items are sealed row by row in
//! `recovery_input`, committed with sealed metadata in the caller's transaction.
use crate::ScopedCodec;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

// RECOVERY-MANIFEST: hashes are committed inside AEAD-sealed metadata, never
// trusted from a caller or an unencrypted side table. Pair content and ciphertext
// digests to compare current input without decrypting unchanged retained rows.
pub fn recovery_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

// RECOVERY-HASH-STREAM: serde writes canonical compact JSON directly into the
// digest. No history-sized temporary JSON buffer is allocated for comparison.
// The digest must match recovery_digest(serde_json::to_vec(item)) exactly.
pub fn recovery_item_digest(item: &Value) -> Result<String> {
    struct HashWriter(Sha256);
    impl std::io::Write for HashWriter {
        /// Stream canonical JSON bytes into the recovery digest without retaining a history-sized buffer.
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        /// Finish the Write contract; this digest writer has no buffered bytes to flush.
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    serde_json::to_writer(&mut writer, item)?;
    Ok(format!("{:x}", writer.0.finalize()))
}

/// Optional extension of the v1 split format. Old records validate by decryption;
/// malformed manifests fail closed instead of silently falling back to that path.
pub fn recovery_manifest(reference: &Value, count: usize) -> Result<Option<Vec<(String, String)>>> {
    match reference.get("manifest") {
        None => Ok(None),
        Some(value) => {
            let pairs: Vec<(String, String)> = serde_json::from_value(value.clone())
                .map_err(|_| anyhow!("Invalid recovery input manifest"))?;
            if pairs.len() != count
                || pairs.iter().any(|(plain, sealed)| {
                    [plain, sealed].iter().any(|hash| {
                        hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                })
            {
                return Err(anyhow!("Invalid recovery input manifest"));
            }
            Ok(Some(pairs))
        }
    }
}

/// Reconstruct a split recovery record under the reader's transaction.
/// Legacy records contain input inline and need no migration on read. Missing
/// rows or gaps are corruption, never an excuse to resume with partial input.
pub async fn read_document<C: ScopedCodec>(
    connection: &mut SqliteConnection,
    codec: &C,
    raw: &[u8],
) -> Result<Value> {
    let mut value = codec.unseal(raw)?;
    if let Some(reference) = value.get("_recovery_input").cloned() {
        if reference["version"] != 1 {
            return Err(anyhow!("Unsupported recovery input version"));
        }
        let rid = value["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Recovery record has no run id"))?;
        let count = reference["count"]
            .as_u64()
            .ok_or_else(|| anyhow!("Invalid recovery input count"))?;
        let rows =
            sqlx::query("SELECT ordinal,item FROM recovery_input WHERE run_id=? ORDER BY ordinal")
                .bind(rid)
                .fetch_all(&mut *connection)
                .await?;
        if rows.len() as u64 != count {
            return Err(anyhow!("Incomplete recovery input"));
        }
        let manifest = recovery_manifest(&reference, rows.len())?;
        let mut input = Vec::with_capacity(rows.len());
        for (ordinal, row) in rows.iter().enumerate() {
            if row.get::<i64, _>("ordinal") != ordinal as i64 {
                return Err(anyhow!("Recovery input ordinal gap"));
            }
            let sealed = row.get::<Vec<u8>, _>("item");
            let item = codec.unseal(&sealed)?;
            // RECOVERY-MANIFEST-READ: reconstruction verifies both commitments;
            // save optimization never weakens the authoritative read path.
            if let Some(pairs) = &manifest {
                if pairs[ordinal].1 != recovery_digest(&sealed)
                    || pairs[ordinal].0 != recovery_item_digest(&item)?
                {
                    return Err(anyhow!("Recovery input manifest mismatch"));
                }
            }
            input.push(item);
        }
        value.as_object_mut().unwrap().remove("_recovery_input");
        value["input"] = Value::Array(input);
    }
    Ok(value)
}

/// Persist a run's recovery-input delta inside the caller's transaction and
/// return the metadata document to store under `key` in its place.
/// Unchanged items retain their ciphertext; append, replacement, compaction,
/// and truncation update only affected rows. The metadata reference is valid
/// only alongside the complete item set committed in the same transaction.
pub async fn save_run_input<C: ScopedCodec>(
    tx: &mut SqliteConnection,
    codec: &C,
    key: &str,
    value: &Value,
) -> Result<Value> {
    let rid = value["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run has no id"))?;
    if key != format!("run:{rid}") {
        return Err(anyhow!("Recovery run identity mismatch"));
    }
    let input = value["input"]
        .as_array()
        .ok_or_else(|| anyhow!("Run has no input"))?;
    let rows =
        sqlx::query("SELECT ordinal,item FROM recovery_input WHERE run_id=? ORDER BY ordinal")
            .bind(rid)
            .fetch_all(&mut *tx)
            .await?;
    // RECOVERY-MANIFEST-SAVE: verify every previous ciphertext against
    // sealed metadata before any mutation. Legacy records decrypt once to
    // establish content hashes; manifest-backed saves skip that decryption.
    let mut prior_manifest = None;
    if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?
    {
        let previous = codec.unseal(&raw)?;
        if let Some(reference) = previous.get("_recovery_input") {
            if reference["version"] != 1 || reference["count"].as_u64() != Some(rows.len() as u64) {
                return Err(anyhow!("Incomplete or unsupported recovery input"));
            }
            prior_manifest = recovery_manifest(reference, rows.len())?;
        }
    }
    let mut previous_hashes = Vec::with_capacity(rows.len());
    for (ordinal, row) in rows.iter().enumerate() {
        if row.get::<i64, _>("ordinal") != ordinal as i64 {
            return Err(anyhow!("Recovery input ordinal gap"));
        }
        let sealed = row.get::<Vec<u8>, _>("item");
        let sealed_hash = recovery_digest(&sealed);
        let plain_hash = if let Some(pairs) = &prior_manifest {
            if pairs[ordinal].1 != sealed_hash {
                return Err(anyhow!("Recovery input manifest mismatch"));
            }
            pairs[ordinal].0.clone()
        } else {
            recovery_item_digest(&codec.unseal(&sealed)?)?
        };
        previous_hashes.push((plain_hash, sealed_hash));
    }
    let mut manifest = Vec::with_capacity(input.len());
    for (ordinal, item) in input.iter().enumerate() {
        let plain_hash = recovery_item_digest(item)?;
        if let Some(pair) = previous_hashes
            .get(ordinal)
            .filter(|pair| pair.0 == plain_hash)
        {
            manifest.push(pair.clone());
        } else {
            let sealed = codec.encode_user_record(item)?;
            manifest.push((plain_hash, recovery_digest(&sealed)));
            sqlx::query("INSERT INTO recovery_input(run_id,ordinal,item) VALUES(?,?,?) ON CONFLICT(run_id,ordinal) DO UPDATE SET item=excluded.item")
                .bind(rid).bind(ordinal as i64).bind(sealed).execute(&mut *tx).await?;
        }
    }
    sqlx::query("DELETE FROM recovery_input WHERE run_id=? AND ordinal>=?")
        .bind(rid)
        .bind(input.len() as i64)
        .execute(&mut *tx)
        .await?;
    let mut metadata = Value::Object(
        value
            .as_object()
            .ok_or_else(|| anyhow!("Invalid run document"))?
            .iter()
            .filter(|(key, _)| key.as_str() != "input")
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    );
    metadata["_recovery_input"] = json!({"version":1,"count":input.len(),"manifest":manifest});
    Ok(metadata)
}
