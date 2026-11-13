//! Source Control tools ship with the optional Administrator pack (`git_control/remote`).
//! Without the pack its tools are unknown and its settings routes answer 404.
#[cfg(administrator_pack)]
#[path = "../abilities/Administrator/git_control/remote/mod.rs"]
mod installed;
#[cfg(administrator_pack)]
pub use installed::*;

#[cfg(not(administrator_pack))]
pub const TOOLS: [&str; 0] = [];
#[cfg(not(administrator_pack))]
pub fn owns(_: &str) -> bool {
    false
}
#[cfg(not(administrator_pack))]
pub fn is_read(_: &str) -> bool {
    false
}
#[cfg(not(administrator_pack))]
pub async fn settings_get() -> crate::Api {
    Err(not_installed())
}
#[cfg(not(administrator_pack))]
pub async fn settings_put() -> crate::Api {
    Err(not_installed())
}
#[cfg(not(administrator_pack))]
fn not_installed() -> crate::ApiError {
    crate::ApiError(
        axum::http::StatusCode::NOT_FOUND,
        "Source Control is not installed".into(),
    )
}
#[cfg(not(administrator_pack))]
pub async fn execute(
    _: &crate::App,
    _: &str,
    name: &str,
    _: &serde_json::Value,
    _: tokio_util::sync::CancellationToken,
) -> anyhow::Result<serde_json::Value> {
    anyhow::bail!("{name} needs the Administrator pack")
}
