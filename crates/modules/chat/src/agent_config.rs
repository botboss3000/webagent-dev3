//! Per-agent sparse overlays of `config/chat.json` (`chat`) and the session dropdown component
//! JSON (`session_dropdown`), stored encrypted in that agent's database. The base JSON stays
//! authoritative: overlays merge over it, attachment limits only tighten, and an overlay that no
//! longer validates against the current base is ignored at read time instead of failing reads.
use crate::{configuration::validate_chat_config, RecordCodec};
use anyhow::anyhow;
use serde_json::{json, Map, Value};
use sqlx::SqlitePool;

pub const MAX_OVERLAY_BYTES: usize = 64 * 1024;
const SURFACES: [&str; 4] = ["common", "desktop", "mobile", "widget"];
const LIMITS: [&str; 2] = ["max_file_size_mb", "max_per_message"];

pub fn known_document(document: &str) -> bool {
    matches!(document, "chat" | "session_dropdown")
}

/// Agent-plane document key; the `agent:<id>:` prefix routes to that agent's database.
pub fn key(agent_id: &str, document: &str) -> String {
    format!("agent:{agent_id}:ui_config:{document}")
}

/// The client's `mergeProfile` rules: objects merge key by key; arrays and scalars replace.
pub fn deep_merge(base: &Value, overlay: &Value) -> Value {
    let Value::Object(overlay) = overlay else {
        return overlay.clone();
    };
    let mut merged = base.as_object().cloned().unwrap_or_default();
    for (key, value) in overlay {
        let next = deep_merge(merged.get(key).unwrap_or(&Value::Null), value);
        merged.insert(key.clone(), next);
    }
    Value::Object(merged)
}

/// RFC 7386 merge patch: `null` removes a key so it falls back to base; objects left empty are pruned.
pub fn merge_patch(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match value {
            Value::Null => {
                target.remove(key);
            }
            Value::Object(patch) => {
                let mut child = match target.remove(key) {
                    Some(Value::Object(child)) => child,
                    _ => Map::new(),
                };
                merge_patch(&mut child, patch);
                if !child.is_empty() {
                    target.insert(key.clone(), Value::Object(child));
                }
            }
            _ => {
                target.insert(key.clone(), value.clone());
            }
        }
    }
}

/// An agent may lower an installation attachment limit, or set one where base has none, never raise it.
fn merged_chat(base: &Value, overlay: &Value) -> Value {
    let mut merged = deep_merge(base, overlay);
    for name in LIMITS {
        let Some(limit) = base["chat_limits"]["attachments"][name].as_u64() else {
            continue;
        };
        if let Some(slot) = merged.pointer_mut(&format!("/chat_limits/attachments/{name}")) {
            if slot.as_u64().is_some_and(|value| value > limit) {
                *slot = json!(limit);
            }
        }
    }
    merged
}

fn reserved_key(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!(key.as_str(), "__proto__" | "constructor" | "prototype") || reserved_key(value)
        }),
        Value::Array(items) => items.iter().any(reserved_key),
        _ => false,
    }
}

/// Check one overlay against the current base; a chat overlay must leave a valid merged chat.json.
pub fn validate(document: &str, base: &Value, overlay: &Value) -> Result<(), String> {
    let object = overlay.as_object().ok_or("Overlay must be an object")?;
    if serde_json::to_vec(overlay).map_or(true, |bytes| bytes.len() > MAX_OVERLAY_BYTES) {
        return Err("Overlay exceeds 64 KiB".into());
    }
    // Browsers merge these documents into plain objects.
    if reserved_key(overlay) {
        return Err("Overlay contains a reserved key".into());
    }
    match document {
        "chat" => {
            validate_chat_config(base).map_err(|_| "Base chat configuration is invalid")?;
            validate_chat_config(&merged_chat(base, overlay)).map_err(|error| error.to_string())
        }
        "session_dropdown" => {
            if object
                .iter()
                .all(|(key, value)| SURFACES.contains(&key.as_str()) && value.is_object())
            {
                Ok(())
            } else {
                Err("Session dropdown overlays allow only common, desktop, mobile and widget objects".into())
            }
        }
        _ => Err("Unknown chat configuration document".into()),
    }
}

/// The document a client or enforcement point uses; an overlay invalid against the current base is ignored.
pub fn effective(document: &str, base: &Value, overlay: &Value) -> Value {
    if validate(document, base, overlay).is_err() {
        return base.clone();
    }
    match document {
        "chat" => merged_chat(base, overlay),
        _ => deep_merge(base, overlay),
    }
}

pub enum Change<'a> {
    Patch(&'a Value),
    Reset,
}

#[derive(Debug)]
pub enum SaveError {
    Conflict,
    Invalid(String),
    Storage(anyhow::Error),
}

impl From<anyhow::Error> for SaveError {
    fn from(error: anyhow::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<sqlx::Error> for SaveError {
    fn from(error: sqlx::Error) -> Self {
        Self::Storage(error.into())
    }
}

fn empty() -> Value {
    json!({"overlay":{},"revision":0,"updated_at":null,"updated_by":null})
}

/// Read the stored record `{overlay, revision, updated_at, updated_by}`; absent means an empty overlay at revision 0.
pub async fn read<C: RecordCodec>(
    pool: &SqlitePool,
    codec: &C,
    agent_id: &str,
    document: &str,
) -> anyhow::Result<Value> {
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key(agent_id, document))
        .fetch_optional(pool)
        .await?;
    Ok(raw
        .map(|raw| codec.unseal(&raw))
        .transpose()?
        .unwrap_or_else(empty))
}

/// Revision-fenced merge patch or reset in one agent-pool transaction. The empty UPDATE takes
/// SQLite's writer reservation before the read, so the API and loop processes cannot interleave.
#[allow(clippy::too_many_arguments)]
pub async fn save<C: RecordCodec>(
    pool: &SqlitePool,
    codec: &C,
    agent_id: &str,
    document: &str,
    change: Change<'_>,
    expected_revision: i64,
    base: &Value,
    actor: &str,
    now: i64,
) -> Result<Value, SaveError> {
    if !known_document(document) {
        return Err(SaveError::Invalid(
            "Unknown chat configuration document".into(),
        ));
    }
    let key = key(agent_id, document);
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE docs SET value=value WHERE 0")
        .execute(&mut *tx)
        .await?;
    let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?;
    let current = raw
        .map(|raw| codec.unseal(&raw))
        .transpose()?
        .unwrap_or_else(empty);
    let revision = current["revision"]
        .as_i64()
        .ok_or_else(|| anyhow!("Invalid chat configuration record"))?;
    if revision != expected_revision {
        return Err(SaveError::Conflict);
    }
    let mut overlay = current["overlay"].as_object().cloned().unwrap_or_default();
    match change {
        Change::Reset => overlay.clear(),
        Change::Patch(patch) => merge_patch(
            &mut overlay,
            patch
                .as_object()
                .ok_or_else(|| SaveError::Invalid("Patch must be an object".into()))?,
        ),
    }
    let overlay = Value::Object(overlay);
    // An empty overlay is the base itself and must always be saveable.
    if !overlay.as_object().unwrap().is_empty() {
        validate(document, base, &overlay).map_err(SaveError::Invalid)?;
    }
    let record =
        json!({"overlay":overlay,"revision":revision + 1,"updated_at":now,"updated_by":actor});
    sqlx::query(
        "INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(&key)
    .bind(codec.seal(&record)?)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(record)
}
