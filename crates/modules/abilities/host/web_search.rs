//! Web Search runs as an ability plugin (`plugins/abilities/Web/web_search`) through
//! `crate::extension_plugins`; the app makes each provider call it declares. This file
//! keeps its tool list for schemas and the startup check.
use crate::{App, CancellationToken};
use anyhow::Result;
use serde_json::Value;

/// Tool folders of the ability; their schemas live in `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["web_search", "web_read"];

/// Whether the plugin serves `name`.
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

pub fn schemas() -> Vec<Value> {
    crate::tools::folder_schemas(TOOLS)
}

#[allow(clippy::too_many_arguments)]
pub async fn execute(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    crate::extension_plugins::execute(crate::extension_plugins::Call {
        app,
        sid,
        run_id: rid,
        generation,
        pending,
        name,
        args,
        cancel,
        ability: "Web/web_search",
    })
    .await
}
