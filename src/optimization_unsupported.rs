//! The optimization plugin pins its package through `/proc` descriptors and talks to a
//! peer-checked Unix socket, so it is a Linux server feature. Elsewhere it is never
//! installed: no declaration, no generation, and `init` starts nothing.
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) fn folder() -> PathBuf {
    webagent_configuration::paths::checkout()
        .join("plugins")
        .join("optimization")
}
pub(crate) fn declaration() -> anyhow::Result<Option<(String, Value)>> {
    Ok(None)
}
pub(crate) fn generation() -> Option<String> {
    None
}
pub(crate) fn available() -> bool {
    false
}
pub(crate) fn guidance() -> Option<String> {
    None
}
pub(crate) async fn init(_root: &Path, _role: &str) {}
