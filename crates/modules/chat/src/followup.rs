//! Durable follow-up queue records, validation, projection and user-plane SQL helpers.
use crate::{session::require_active_session, shared::require_participation_in, RecordCodec};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::{Sqlite, Transaction};

pub fn empty(sid: &str) -> Value {
    json!({"schema_version":1,"session_id":sid,"revision":0,"held":false,"reason":null,"entries":[]})
}

pub fn public_entry(e: &Value) -> Value {
    json!({
        "id": e["id"],
        "message_id": e["message_id"],
        "revision": e["revision"],
        "choice": e["choice"],
        "status": if e["status"] == "pending" && e["interrupt_committed"] == true {
            json!("interrupting")
        } else {
            e["status"].clone()
        },
        "content": e["content"],
        "attachments": e["attachments"],
        "consuming_run_id": e["consuming_run_id"],
        "batch_id": e["batch_id"],
        "review_status": e["review_status"]
    })
}

pub fn public(q: &Value) -> Value {
    json!({
        "queue": q["entries"].as_array().into_iter().flatten().map(public_entry).collect::<Vec<_>>(),
        "queue_state": {"held": q["held"], "reason": q["reason"], "revision": q["revision"]}
    })
}

pub fn validate_queue(s: &str, q: &Value) -> Result<()> {
    if q["schema_version"] != 1
        || q["session_id"] != s
        || q["revision"].as_u64().is_none()
        || q["held"].as_bool().is_none()
        || q["entries"].as_array().is_none_or(|entries| {
            entries.iter().any(|e| {
                e["id"].as_str().is_none()
                    || e["revision"].as_u64().is_none()
                    || e["content"].as_str().is_none()
                    || e["context_content"].as_str().is_none()
                    || e["attachments"].as_array().is_none()
                    || !matches!(
                        e["choice"].as_str(),
                        Some("queue" | "steer" | "interrupt" | "cancel")
                    )
                    || !matches!(
                        e["status"].as_str(),
                        Some(
                            "pending"
                                | "reviewing"
                                | "held"
                                | "delivered"
                                | "cancelled"
                                | "rejected"
                        )
                    )
            })
        })
    {
        return Err(anyhow!("Invalid or unsupported followup queue record"));
    }
    Ok(())
}

pub async fn legacy_followup_fence<C: RecordCodec>(
    codec: &C,
    owner_user_id: &str,
    t: &mut Transaction<'_, Sqlite>,
    sid: &str,
    template: &Value,
) -> Result<()> {
    if template["requester_user_id"]
        .as_str()
        .is_some_and(|actor| actor != owner_user_id)
    {
        return Err(anyhow!("Private followup requester mismatch"));
    }
    if template["session_id"]
        .as_str()
        .is_some_and(|session| session != sid)
    {
        return Err(anyhow!("Private followup session mismatch"));
    }
    let mut fence = template.clone();
    if !fence.is_object() {
        fence = json!({});
    }
    fence["session_id"] = json!(sid);
    require_participation_in(codec, t, &fence).await
}

pub fn legacy_message_meta_doc(owner_user_id: &str, entry: &Value, eligible: bool) -> Result<(String, Value)> {
    let id = entry["id"]
        .as_str()
        .ok_or_else(|| anyhow!("Followup id missing"))?;
    let mut meta = json!({"author_id":owner_user_id,"context_eligible":eligible,"queued":!eligible});
    if entry["agent_message"].is_object() {
        meta["agent_message"] = entry["agent_message"].clone();
        meta["message_kind"] = json!("agent_message");
        meta["author_name"] = json!(format!(
            "Message from {} · {}",
            entry["agent_message"]["source_agent_id"]
                .as_str()
                .unwrap_or("agent"),
            entry["agent_message"]["source_title"]
                .as_str()
                .unwrap_or("conversation")
        ));
    }
    if eligible {
        meta["context_content"] = entry["context_content"].clone();
    }
    Ok((format!("message_meta:{id}"), meta))
}

pub async fn queue_read_tx<C: RecordCodec>(
    codec: &C,
    t: &mut Transaction<'_, Sqlite>,
    s: &str,
) -> Result<Value> {
    let r: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(format!("followup_queue:{s}"))
        .fetch_optional(&mut **t)
        .await?;
    let q = r
        .map(|v| codec.unseal(&v))
        .transpose()?
        .unwrap_or_else(|| empty(s));
    validate_queue(s, &q)?;
    Ok(q)
}

pub async fn queue_write_tx<C: RecordCodec>(
    codec: &C,
    t: &mut Transaction<'_, Sqlite>,
    q: &mut Value,
) -> Result<()> {
    q["revision"] = json!(q["revision"].as_u64().unwrap_or(0) + 1);
    let s = q["session_id"].as_str().unwrap();
    sqlx::query(
        "INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(format!("followup_queue:{s}"))
    .bind(codec.seal(q)?)
    .execute(&mut **t)
    .await?;
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'queue_updated',?)")
        .bind(s)
        .bind(codec.encode_user_record(&json!({"revision":q["revision"]}))?)
        .execute(&mut **t)
        .await?;
    Ok(())
}

pub async fn queue_reserve<C: RecordCodec>(
    codec: &C,
    t: &mut Transaction<'_, Sqlite>,
    s: &str,
) -> Result<()> {
    sqlx::query("UPDATE docs SET key=key WHERE key=?")
        .bind(format!("session:{s}"))
        .execute(&mut **t)
        .await?;
    let present: i64 = sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?")
        .bind(format!("session:{s}"))
        .fetch_one(&mut **t)
        .await?;
    if present == 0 {
        return Err(anyhow!("Session not found"));
    }
    require_active_session(codec, t, s).await
}
