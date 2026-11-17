//! remove_own_skill: argument shape and the own-skill removal.
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveArgs {
    name: String,
    expected_revision: i64,
}
pub(super) async fn remove(
    host: &dyn NativeHost,
    store: &StoreCore,
    args: &Value,
    pending: &str,
    scope: &str,
) -> Result<Value> {
    let query: RemoveArgs = serde_json::from_value(args.clone())?;
    host.mutate_own_skill(
        store,
        &query.name,
        None,
        None,
        None,
        query.expected_revision,
        true,
        pending,
        scope,
    )
    .await
}
