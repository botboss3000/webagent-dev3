//! reset_to_default: return the run to the standard role and default effort.
use super::Empty;
use crate::NativeHost;
use anyhow::Result;
use serde_json::{json, Value};
use webagent_models::model_selection::{self, Catalog};

pub(super) async fn run(
    host: &dyn NativeHost,
    catalog: &Catalog,
    agent: &str,
    args: &Value,
    state: &mut Value,
) -> Result<()> {
    let _: Empty = serde_json::from_value(args.clone())?;
    let mut plan = catalog.resolve_role(agent, model_selection::ModelRole::Standard)?;
    host.pin_model_plan(&mut plan).await?;
    state["state"]["selection"] = json!({"role":"standard"});
    state["state"]["effort"] = Value::Null;
    Ok(())
}
