//! Receiver admission, retained payloads, replay checks and metadata notifications
//! share one writer.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use webagent_host_core::{digest, now, StoreCore};

pub(crate) async fn ingest(
    store: &StoreCore,
    id: &str,
    revision: u64,
    token_digest: &str,
    event_id: &str,
    payload: &Value,
) -> Result<Value> {
    uuid::Uuid::parse_str(id)?;
    if event_id.is_empty() || event_id.len() > 128 || serde_json::to_vec(payload)?.len() > 16384
    {
        bail!("Webhook event exceeds bounds");
    }
    let record_key = format!("core_webhook:{id}");
    let log_key = format!("core_webhook_log:{id}");
    let _guard = store.context_gate.write().await;
    let mut tx = store.pool.begin().await?;
    sqlx::query("UPDATE docs SET key=key WHERE key=?")
        .bind(&record_key)
        .execute(&mut *tx)
        .await?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(&record_key)
        .fetch_one(&mut *tx)
        .await?;
    let record = store.unseal(&raw)?;
    let spec = &record["artifact"];
    if record["revision"] != revision
        || spec["deleted"] == true
        || spec["expires_at"].as_u64().unwrap_or(0) <= now()
        || spec["token_digest"] != token_digest
    {
        bail!("Receiver revoked or changed");
    }
    let sid = record["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Receiver conversation missing"))?;
    webagent_chat::session::require_active_session(store, &mut tx, sid).await?;
    let mut log = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(&log_key)
        .fetch_optional(&mut *tx)
        .await?
        .map(|v| store.unseal(&v))
        .transpose()?
        .unwrap_or(json!({"revision":0,"day":0,"daily_count":0,"entries":[]}));
    let fingerprint = digest(payload);
    if let Some(prior) = log["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|v| v["event_id"] == event_id)
    {
        if prior["digest"] != fingerprint {
            bail!("Webhook event identity reused");
        }
        return Ok(json!({"accepted":true,"duplicate":true,"seq":prior["seq"]}));
    }
    let day = now() / 86400;
    if log["day"] != day {
        log["day"] = json!(day);
        log["daily_count"] = json!(0);
    }
    let daily = log["daily_count"].as_u64().unwrap_or(0);
    if daily >= 1000 {
        bail!("Webhook daily cap reached");
    }
    let seq = log["revision"]
        .as_u64()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or_else(|| anyhow!("Webhook sequence exhausted"))?;
    let cap = spec["max_entries"]
        .as_u64()
        .filter(|v| (1..=100).contains(v))
        .ok_or_else(|| anyhow!("Invalid webhook retention"))? as usize;
    let entries = log["entries"]
        .as_array_mut()
        .ok_or_else(|| anyhow!("Invalid webhook log"))?;
    entries.push(json!({"event_id":event_id,"seq":seq,"digest":fingerprint,"payload":payload,"received_at":now(),"authority":"untrusted_external_data"}));
    if entries.len() > cap {
        entries.drain(..entries.len() - cap);
    }
    log["revision"] = json!(seq);
    log["daily_count"] = json!(daily + 1);
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(&log_key).bind(store.seal(&log)?).execute(&mut *tx).await?;
    store.stage_notification(
        &mut tx,
        &format!("core-webhook:{id}:{seq}"),
        "webhook.received",
        sid,
        None,
        Some(id),
    )
    .await?;
    tx.commit().await?;
    store.cache.lock().unwrap_or_else(|error| error.into_inner()).invalidate(&[log_key]);
    Ok(json!({"accepted":true,"duplicate":false,"seq":seq}))
}
