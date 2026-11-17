//! get_webhook_log: bounded pages of a webhook's retained untrusted events.
use super::Args;
use crate::NativeHost;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub(super) async fn log(
    host: &dyn NativeHost,
    id: &str,
    record: &Value,
    q: &Args,
) -> Result<Value> {
    let store = host.store();
    let log = store
        .get(&format!("core_webhook_log:{id}"))
        .await?
        .unwrap_or(json!({"revision":0,"entries":[]}));
    let all = log["entries"]
        .as_array()
        .ok_or_else(|| anyhow!("Invalid webhook log"))?;
    let after = q.after.unwrap_or(0);
    let oldest = all.first().and_then(|v| v["seq"].as_u64()).unwrap_or(1);
    Ok(if after < oldest.saturating_sub(1) {
        json!({"cursor_expired":true,"oldest_seq":oldest,"next_seq":oldest-1,"entries":[]})
    } else {
        let limit = q.limit.unwrap_or(5);
        if !(1..=20).contains(&limit) {
            return Err(anyhow!("Log page limit must be 1..20"));
        }
        let mut page = Vec::new();
        let mut bytes = 0usize;
        for event in all
            .iter()
            .filter(|v| v["seq"].as_u64().unwrap_or(0) > after)
            .take(limit)
        {
            let size = serde_json::to_vec(event)?.len();
            if bytes + size > 65536 {
                break;
            }
            bytes += size;
            page.push(event.clone());
        }
        let next = page.last().and_then(|v| v["seq"].as_u64()).unwrap_or(after);
        json!({"entries":page,"next_seq":next,"has_more":all.iter().any(|v|v["seq"].as_u64().unwrap_or(0)>next),"authority":"untrusted_external_webhook_data","definition_revoked":record["artifact"]["deleted"]==true})
    })
}
