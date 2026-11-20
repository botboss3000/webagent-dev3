//! Main Notes tools: this agent's private notes across its sessions. The host checks that
//! the caller owns the session and has live access to its agent, and keeps the notes'
//! storage and wake-ups; these handlers read the arguments and shape the results.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[path = "../main_notes_read/main_notes_read.rs"]
mod main_notes_read;
#[path = "../main_notes_write/main_notes_write.rs"]
mod main_notes_write;

/// Tools this module executes; their schemas live in their `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &["main_notes_read", "main_notes_write"];
/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("main_notes_read", main_notes_read::ReadArgs),
    webagent_abilities::argument_type!("main_notes_write", main_notes_write::WriteArgs),
];

/// Run one Main Notes tool for the session at `call`.
///
/// # Errors
/// When the call is cancelled, the caller may not use the notes, or the arguments or the
/// write are refused.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    match name {
        "main_notes_read" => main_notes_read::run(host, call, args).await,
        "main_notes_write" => main_notes_write::run(host, call, args).await,
        _ => bail!("Unknown Main Notes tool"),
    }
}
