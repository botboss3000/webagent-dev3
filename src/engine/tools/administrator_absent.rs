//! The Administration module's root surface in a build without the `administrator`
//! feature: no Administrator tools or schemas, no live-checkout grants, and every
//! broker or source request is refused. The app runs as a non-coding platform.
use super::failure::{Code, Delivery, Failure, Stage};
use crate::App;
use anyhow::{bail, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// Whether `name` is an Administrator native tool: never in this build.
pub(crate) fn supported_schema_name(_: &str) -> bool {
    false
}

/// No Administrator tool has a plugin in this build.
pub(crate) fn plugin(_: &str) -> Option<&'static str> {
    None
}

/// Whether the Administrator handlers execute `name`: never in this build.
pub fn owns(_: &str) -> bool {
    false
}

/// Tool folders of an installed Administrator pack, which this build has no handlers for:
/// the definition check counts them as handled, since the catalog skips that group and
/// they report unavailable like a missing ability.
pub(crate) fn pack_tools() -> Vec<String> {
    use webagent_abilities::tool_definitions as definitions;
    let Some(root) = definitions::root() else {
        return Vec::new();
    };
    std::fs::read_dir(root.join("Administrator"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|ability| definitions::read_folder(&ability.path()).ok().flatten())
        .flatten()
        .map(|tool| tool.name)
        .collect()
}

/// Administrator schemas: none in this build.
pub fn schemas() -> Vec<Value> {
    Vec::new()
}

/// Without the Administration module nothing grants live-checkout access.
pub(crate) async fn checkout_grant(
    _: &App,
    _: &str,
    _: &Value,
    _: &str,
    trusted: Option<Value>,
) -> Option<Value> {
    trusted
}

pub fn build_socket(app: &App) -> std::path::PathBuf {
    app.build_socket.clone()
}

/// No caller holds Administrator authority in this build.
pub(crate) async fn host_fingerprint(_: &App, _: &str, _: &str, _: &str) -> Result<String> {
    Err(Failure::new(
        Stage::Authorization,
        Code::AuthorizationRejected,
        Delivery::NotSent,
    )
    .into())
}

pub async fn workspace_host(
    _: &App,
    _: &str,
    name: &str,
    _: &Value,
    _: Option<Value>,
    _: CancellationToken,
) -> Result<Value> {
    bail!("{name} needs the Administrator tier")
}

#[allow(clippy::too_many_arguments)]
pub async fn execute(
    _: &App,
    _: &str,
    _: &str,
    name: &str,
    _: &Value,
    _: &str,
    _: u64,
    _: CancellationToken,
) -> Result<Value> {
    bail!("{name} needs the Administrator tier")
}
