//! list_webhooks: public projections of the actor's webhooks.
use super::public;
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};

pub(super) async fn list(host: &dyn NativeHost) -> Result<Value> {
    let store = host.store();
    Ok(
        json!({"webhooks":store.list("core_webhook:").await?.iter().map(public).take(100).collect::<Vec<_>>()}),
    )
}
