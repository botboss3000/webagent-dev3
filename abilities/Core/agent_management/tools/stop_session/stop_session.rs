//! stop_session: cancel a session's open runs and queued messages.
use crate::NativeHost;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use uuid::Uuid;

/// Cancel session `target`'s open runs and queued messages from its `snapshot`.
pub(super) async fn stop(
    host: &dyn NativeHost,
    target: &str,
    request_id: &str,
    snapshot: &Value,
) -> Result<Value> {
    let mut results = Vec::new();
    for item in snapshot["runs"].as_array().into_iter().flatten() {
        if matches!(
            item["status"].as_str(),
            Some("queued" | "running" | "waiting_approval" | "paused" | "waiting_external")
        ) {
            if let Some(id) = item["id"].as_str() {
                Uuid::parse_str(id)?;
                results.push(
                    host.agent_request(
                        "POST",
                        format!("/runs/{id}/cancel"),
                        json!({"client_request_id":format!("{request_id}-{id}")}),
                    )
                    .await?,
                );
            }
        }
    }
    for item in snapshot["queue"].as_array().into_iter().flatten() {
        if matches!(
            item["status"].as_str(),
            Some("pending" | "reviewing" | "held")
        ) {
            if let Some(id) = item["id"].as_str() {
                if !valid_queue_id(id) {
                    return Err(anyhow!("Invalid queued message"));
                }
                results.push(
                    host.agent_request(
                        "PATCH",
                        format!("/sessions/{target}/messages/{id}/delivery"),
                        json!({"client_request_id":format!("{request_id}-{id}"),"expected_revision":item["revision"],"choice":"cancel"}),
                    )
                    .await?,
                );
            }
        }
    }
    Ok(json!({"controls":results,"external_effects_undone":false}))
}

/// Whether `id` is a bounded queued-message ID safe to place in a route.
pub(super) fn valid_queue_id(id: &str) -> bool {
    id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
