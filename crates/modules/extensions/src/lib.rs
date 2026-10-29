//! Extensions: generic package and plugin contracts.
//!
//! - `requests`: the host requests an ability plugin may declare and make as the chatting
//!   user (`requests_v1`), and their validation.
//! - `plugin`: the app's side of one plugin process call (JSON lines, protocol 2).
//! - `health`: package availability and idle polling from probe receipts.
//! - `mcp_bridge`: bounded IPC with the administrator-installed MCP package.
//! - `providers`: external providers a plugin declares and reaches through the app.
//!
//! Adapters that need the app (optional package lifecycle, MCP connections and the plugin
//! runner) live in `host/` and are compiled into the root crate.
pub mod health;
pub mod mcp_bridge;
pub mod plugin;
pub mod providers;
pub mod requests;
