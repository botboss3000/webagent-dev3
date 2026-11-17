//! edit_own_prompt: argument shape and the prompt-slot edit.
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::Value;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptArgs {
    slot_name: String,
    content: String,
    expected_revision: i64,
    #[serde(default = "replace")]
    mode: String,
}
fn replace() -> String {
    "replace".into()
}
pub(super) async fn edit(
    host: &dyn NativeHost,
    store: &StoreCore,
    args: &Value,
    pending: &str,
    scope: &str,
) -> Result<Value> {
    let query: PromptArgs = serde_json::from_value(args.clone())?;
    host.self_edit_prompt_slot(
        store,
        &query.slot_name,
        &query.content,
        &query.mode,
        query.expected_revision,
        pending,
        scope,
    )
    .await
}
