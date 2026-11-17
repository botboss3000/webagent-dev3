//! list_vault_keys: public projections of the actor's stored credentials.
use super::public;
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};

pub(super) async fn list(host: &dyn NativeHost) -> Result<Value> {
    let store = host.store();
    let keys = store
        .list("core_vault:")
        .await?
        .iter()
        .filter_map(|v| v["name"].as_str().map(|name| public(name, v)))
        .take(100)
        .collect::<Vec<_>>();
    Ok(json!({"status":"success","output":{"keys":keys}}))
}
