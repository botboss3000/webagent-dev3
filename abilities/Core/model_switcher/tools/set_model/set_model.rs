//! set_model: select one roster entry for the rest of the run.
use crate::NativeHost;
use anyhow::Result;
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_models::model_selection::Catalog;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Model {
    entry_id: String,
}
pub(super) async fn run(
    host: &dyn NativeHost,
    catalog: &Catalog,
    agent: &str,
    args: &Value,
    state: &mut Value,
) -> Result<()> {
    let q: Model = serde_json::from_value(args.clone())?;
    let mut plan = catalog.resolve_custom(agent, &q.entry_id)?;
    host.pin_model_plan(&mut plan).await?;
    state["state"]["selection"] = json!({"entry_id":q.entry_id});
    Ok(())
}
