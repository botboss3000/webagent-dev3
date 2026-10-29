//! Direct authenticated credential submission has its own nonce, revision and
//! encrypted receipt.
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use webagent_host_core::{digest, StoreCore};

/// Credential names: 1–64 ASCII letters, digits, `_` or `-`.
pub fn valid_credential_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

pub(crate) async fn save_credential(
    store: &StoreCore,
    sid: &str,
    name: &str,
    request: &str,
    expected: u64,
    value: &str,
) -> Result<Value> {
    if !valid_credential_name(name)
        || request.len() != 64
        || value.is_empty()
        || value.len() > 8192
        || value.contains('\0')
    {
        bail!("Invalid credential form");
    }
    let key = format!("core_vault:{name}");
    let receipt_key = format!("receipt:core_vault:{sid}:{request}");
    let fingerprint = digest(&json!([name, expected, value]));
    let _guard = store.context_gate.write().await;
    let mut tx = store.pool.begin().await?;
    sqlx::query("UPDATE docs SET key=key WHERE key=?")
        .bind(format!("module_state:core_ui:{sid}"))
        .execute(&mut *tx)
        .await?;
    if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(&receipt_key)
        .fetch_optional(&mut *tx)
        .await?
    {
        let receipt = store.unseal(&raw)?;
        if receipt["fingerprint"] != fingerprint {
            bail!("Credential request reused");
        }
        return Ok(receipt["output"].clone());
    }
    webagent_chat::session::require_active_session(store, &mut tx, sid).await?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(format!("module_state:core_ui:{sid}"))
        .fetch_one(&mut *tx)
        .await?;
    let ui = store.unseal(&raw)?;
    let state = &ui["state"];
    if state["destination"] != "credential"
        || state["request_id"] != request
        || state["credential_name"] != name
        || state["actor_id"] != store.current_user_id()
        || state["expected_revision"] != expected
    {
        bail!("Credential request changed");
    }
    let old = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?
        .map(|v| store.unseal(&v))
        .transpose()?;
    if old
        .as_ref()
        .and_then(|v| v["revision"].as_u64())
        .unwrap_or(0)
        != expected
    {
        bail!("Credential revision changed");
    }
    if old.is_none() {
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM docs WHERE substr(key,1,11)='core_vault:'",
        )
        .fetch_one(&mut *tx)
        .await?;
        if count >= 100 {
            bail!("Credential quota reached");
        }
    }
    let revision = expected
        .checked_add(1)
        .ok_or_else(|| anyhow!("Credential revision exhausted"))?;
    let record = json!({"name":name,"revision":revision,"origin":state["credential_origin"],"value":value});
    let output = json!({"saved":true,"name":name,"revision":revision,"secret_returned":false});
    write_record(store, &mut tx, &key, &record).await?;
    // The form is spent: clear it so its card disappears on every device.
    let ui_key = format!("module_state:core_ui:{sid}");
    let mut spent = ui.clone();
    spent["state"] = json!({});
    spent["revision"] = json!(ui["revision"].as_u64().unwrap_or(0) + 1);
    write_record(store, &mut tx, &ui_key, &spent).await?;
    write_record(
        store,
        &mut tx,
        &receipt_key,
        &json!({"fingerprint":fingerprint,"output":output}),
    )
    .await?;
    tx.commit().await?;
    store.cache.lock().unwrap_or_else(|error| error.into_inner()).invalidate(&[key, receipt_key, ui_key]);
    Ok(output)
}

/// Sealed row write inside the caller's transaction; the caller invalidates caches.
async fn write_record(
    store: &StoreCore,
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    key: &str,
    value: &Value,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
    )
    .bind(key)
    .bind(store.seal(value)?)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
