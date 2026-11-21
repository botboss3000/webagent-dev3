//! Static calls to the existing foreign domain owners. No App, Store, pools, or copied authority.
use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};
pub trait Host: Clone + Send + Sync + 'static {
    fn selected_source() -> Result<Option<PathBuf>>;
    fn native_schemas_for_catalog(root: &Path, publication: bool) -> Result<Vec<Value>>;
    fn native_schemas_without_administrator() -> Vec<Value>;
    fn native_schemas() -> Vec<Value>;
    fn administrator_supported(name: &str) -> bool;
    fn module_known(name: &str) -> bool;
    /// Installed and enabled by an administrator, whatever its health: the catalog offers
    /// a module's tools from this, so their schemas never follow a health probe.
    fn module_enabled(name: &str) -> bool;
    fn optimization_folder() -> PathBuf;
    fn optimization_declaration() -> Result<Option<(String, Value)>>;
    /// A valid package is installed, whatever its readiness marker says.
    fn optimization_installed() -> bool;
    fn optimization_generation() -> Option<String>;
    fn optimization_guidance() -> Option<String>;
    /// A valid package is installed, whatever its readiness marker says.
    fn mcp_installed() -> bool;
    /// Whether this build carries the Administrator tier. Without it the catalog skips the
    /// `Administrator` group, whose abilities would have no installed adapters.
    fn administrator_tier() -> bool {
        true
    }
    /// The live checkout's drift from the running release; none unless the host compares them.
    fn drift() -> std::sync::Arc<crate::drift::Drift> {
        std::sync::Arc::default()
    }
}
