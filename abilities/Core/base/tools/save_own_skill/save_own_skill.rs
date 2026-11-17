//! save_own_skill: argument shape and the own-skill save.
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SaveArgs {
    name: String,
    expected_revision: i64,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    instructions: Option<String>,
    #[serde(default)]
    mode: Option<String>,
}
pub(super) async fn save(
    host: &dyn NativeHost,
    store: &StoreCore,
    args: &Value,
    pending: &str,
    scope: &str,
) -> Result<Value> {
    let query: SaveArgs = serde_json::from_value(args.clone())?;
    host.mutate_own_skill(
        store,
        &query.name,
        query.description.as_deref(),
        query.instructions.as_deref(),
        query.mode.as_deref(),
        query.expected_revision,
        false,
        pending,
        scope,
    )
    .await
}
