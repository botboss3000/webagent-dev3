//! register_webhook: stable webhook IDs and bounded private inbox registration.
use super::{public, token_digest, write_call, Args, PURPOSE};
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use uuid::Uuid;
use webagent_accounts_host::AccountsStore;
use webagent_host_core::{digest, now};

pub(super) fn id_from(value: &Value) -> String {
    let h = digest(value);
    let mut b = [0u8; 16];
    for (i, x) in b.iter_mut().enumerate() {
        *x = u8::from_str_radix(&h[i * 2..i * 2 + 2], 16).unwrap();
    }
    b[6] = (b[6] & 15) | 80;
    b[8] = (b[8] & 63) | 128;
    Uuid::from_bytes(b).to_string()
}
/// Registers one private webhook; a replayed invocation returns its existing record.
pub(super) async fn register(
    host: &dyn NativeHost,
    call: Call<'_>,
    run: &Value,
    args: &Value,
    q: &Args,
) -> Result<Value> {
    let store = host.store();
    let label = q
        .name
        .as_deref()
        .filter(|s| !s.trim().is_empty() && s.len() <= 120 && !s.chars().any(char::is_control))
        .ok_or_else(|| anyhow!("Bounded webhook name required"))?;
    let cap = q.max_entries.unwrap_or(50);
    if !(1..=100).contains(&cap) {
        return Err(anyhow!("Retained event limit must be 1..100"));
    }
    let expires = q.expires_at.unwrap_or(now() + 30 * 86400);
    if expires <= now() || expires > now() + 365 * 86400 {
        return Err(anyhow!("Webhook expiry must be within one year"));
    }
    let id = id_from(&json!([
        host.actor(),
        call.sid,
        call.run_id,
        call.pending_id
    ]));
    if let Some(prior) = store.get(&format!("core_webhook:{id}")).await? {
        if prior["artifact"]["creation_digest"] != digest(args) {
            return Err(anyhow!("Webhook invocation identity reused"));
        }
        return Ok(public(&prior));
    }
    if store
        .list("core_webhook:")
        .await?
        .iter()
        .filter(|v| v["artifact"]["deleted"] != true)
        .count()
        >= 100
    {
        return Err(anyhow!("Webhook quota reached"));
    }
    let account = store
        .account_by_id(host.actor())
        .await?
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let token=URL_SAFE_NO_PAD.encode(store.seal(&json!({"purpose":PURPOSE,"webhook_id":id,"owner_id":host.actor(),"generation":account["generation"],"expires_at":expires}))?);
    let artifact = json!({"name":label,"max_entries":cap,"expires_at":expires,"agent_id":run["agent_id"],"token":token,"token_digest":token_digest(&token),"creation_digest":digest(args),"deleted":false});
    let saved = host
        .write_artifact(write_call(call, run)?, "core_webhook", &id, 0, &artifact)
        .await?;
    Ok(public(
        &json!({"id":id,"revision":saved["revision"],"artifact":artifact}),
    ))
}
