//! Private bounded webhook inboxes. Bearer setup and the receiver stay in the root's
//! authenticated HTTP routes, which share the credential binding here.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use webagent_host_core::{digest, now, StoreCore};

#[path = "../delete_webhook/delete_webhook.rs"]
mod delete_webhook;
#[path = "../get_webhook_log/get_webhook_log.rs"]
mod get_webhook_log;
#[path = "../list_webhooks/list_webhooks.rs"]
mod list_webhooks;
#[path = "../register_webhook/register_webhook.rs"]
mod register_webhook;

/// The sealed receiver credential's purpose.
pub(super) const PURPOSE: &str = "core-webhook-receiver-v1";

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "register_webhook" | "list_webhooks" | "delete_webhook" | "get_webhook_log"
    )
}
/// Tools this adapter executes; their schemas are built in below.
pub const TOOLS: &[&str] = &[
    "register_webhook",
    "list_webhooks",
    "delete_webhook",
    "get_webhook_log",
];
/// The digest under which a receiver credential is stored and matched.
#[must_use]
pub fn token_digest(token: &str) -> String {
    digest(&json!(token))
}
/// The sealed binding of bearer `token` for webhook `id` with its digest, or `None` when
/// it is malformed, for another purpose or webhook, or expired.
#[must_use]
pub fn verify_token(store: &StoreCore, id: &str, token: &str) -> Option<(Value, String)> {
    let bytes = URL_SAFE_NO_PAD.decode(token).ok()?;
    let binding = store.unseal(&bytes).ok()?;
    if binding["purpose"] != PURPOSE
        || binding["webhook_id"] != id
        || binding["expires_at"].as_u64().unwrap_or(0) <= now()
    {
        return None;
    }
    Some((binding, token_digest(token)))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    name: Option<String>,
    max_entries: Option<u64>,
    expires_at: Option<u64>,
    webhook_id: Option<String>,
    expected_revision: Option<u64>,
    after: Option<u64>,
    limit: Option<usize>,
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("delete_webhook", Args),
    webagent_abilities::argument_type!("get_webhook_log", Args),
    webagent_abilities::argument_type!("list_webhooks", Args),
    webagent_abilities::argument_type!("register_webhook", Args),
];
pub(super) fn public(record: &Value) -> Value {
    json!({"webhook_id":record["id"],"revision":record["revision"],"name":record["artifact"]["name"],"expires_at":record["artifact"]["expires_at"],"active":record["artifact"]["deleted"]!=true&&record["artifact"]["expires_at"].as_u64().unwrap_or(0)>now(),"setup_url":format!("/api/v3/core/webhooks/{}/setup",record["id"].as_str().unwrap_or(""))})
}
/// The call position at which this run's artifacts are written: the run's own generation.
pub(super) fn write_call<'a>(call: Call<'a>, run: &'a Value) -> Result<Call<'a>> {
    Ok(Call {
        generation: run["generation"]
            .as_str()
            .ok_or_else(|| anyhow!("Run generation missing"))?,
        ..call
    })
}
/// Registers, lists, revokes or reads the actor's private webhooks.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let run = host.authorize(call, name, args, &cancel).await?;
    let store = host.store();
    if host.actor() != store.current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for webhook records"
        ));
    }
    let q: Args = serde_json::from_value(args.clone())?;
    let output = match name {
        "list_webhooks" => list_webhooks::list(host).await?,
        "register_webhook" => register_webhook::register(host, call, &run, args, &q).await?,
        "delete_webhook" | "get_webhook_log" => {
            let id = q
                .webhook_id
                .as_deref()
                .ok_or_else(|| anyhow!("Webhook ID required"))?;
            Uuid::parse_str(id)?;
            let record = store
                .get(&format!("core_webhook:{id}"))
                .await?
                .ok_or_else(|| anyhow!("Webhook not found"))?;
            if name == "delete_webhook" {
                delete_webhook::delete(host, call, &run, id, &q).await?
            } else {
                get_webhook_log::log(host, id, &record, &q).await?
            }
        }
        _ => return Err(anyhow!("Unknown webhook tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
#[cfg(test)]
#[path = "../../tests/webhooks.rs"]
mod tests;
