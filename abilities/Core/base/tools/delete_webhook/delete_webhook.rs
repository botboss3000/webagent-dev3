//! delete_webhook: revokes one webhook at its expected revision.
use super::{write_call, Args};
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};

pub(super) async fn delete(
    host: &dyn NativeHost,
    call: Call<'_>,
    run: &Value,
    id: &str,
    q: &Args,
) -> Result<Value> {
    let at = write_call(call, run)?;
    let output = host
        .write_artifact(
            at,
            "core_webhook",
            id,
            q.expected_revision
                .ok_or_else(|| anyhow!("Webhook revision required"))?,
            &json!({"deleted":true}),
        )
        .await?;
    Ok(json!({"webhook_id":id,"revision":output["revision"],"revoked":true}))
}
