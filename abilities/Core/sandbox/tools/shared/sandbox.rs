//! The Sandbox ability: `sandbox_shell`, `sandbox_python` and `sandbox_file` in a session's
//! scratch folder. Each tool's module checks its arguments; every call then runs under
//! fresh run authority on the isolated worker's strict sandbox. The root adds the limits
//! (from [`limits`]) and the storage owner's remaining room as trusted fields, so a model
//! can never choose them.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[path = "../sandbox_file/sandbox_file.rs"]
mod file;
#[path = "../sandbox_python/sandbox_python.rs"]
mod python;
#[path = "../sandbox_shell/sandbox_shell.rs"]
mod shell;

/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &["sandbox_shell", "sandbox_python", "sandbox_file"];

/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// The ability's limits (`abilities/Core/sandbox/limits.json`), read on every call so an
/// edit applies at the next one. The worker clamps each to its own ceiling.
///
/// # Errors
/// When the file is missing or names a limit that is not a whole number.
pub fn limits() -> Result<Value> {
    let root = webagent_abilities::tool_definitions::root()
        .ok_or_else(|| anyhow!("Sandbox limits unavailable"))?;
    let path = root.join("Core/sandbox/limits.json");
    let limits: Value = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("Sandbox limits unavailable: {}", path.display()))?,
    )
    .context("Sandbox limits invalid")?;
    for key in [
        "timeout_seconds",
        "memory_mb",
        "processes",
        "file_mb",
        "open_files",
        "folder_mb",
        "read_kb",
    ] {
        if limits[key].as_u64().is_none() {
            bail!("Sandbox limit {key} must be a whole number");
        }
    }
    Ok(limits)
}

/// Run Sandbox tool `name` under fresh run authority at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let request = match name {
        "sandbox_shell" => shell::request(args),
        "sandbox_python" => python::request(args),
        "sandbox_file" => file::request(args),
        _ => bail!("Unknown Sandbox tool"),
    }?;
    let run = host.authorize(call, name, args, &cancel).await?;
    let result = host.worker(name, &request, call.sid, &run, cancel).await?;
    Ok(result)
}

/// An optional `timeout_seconds` argument, copied for the worker (which caps it).
fn timeout(args: &Value) -> Result<Option<u64>> {
    match args.get("timeout_seconds") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .filter(|seconds| (1..=600).contains(seconds))
            .map(Some)
            .ok_or_else(|| anyhow!("timeout_seconds must be a whole number from 1 to 600")),
    }
}

/// The one text argument `key` of a command tool, 1 to 65536 bytes.
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let text = args[key]
        .as_str()
        .ok_or_else(|| anyhow!("{key} must be text"))?;
    if text.is_empty() || text.len() > 65536 {
        bail!("{key} must be 1 to 65536 bytes");
    }
    Ok(text)
}

/// Refuses arguments outside `allowed`, as the tool's schema does.
fn only(args: &Value, allowed: &[&str]) -> Result<()> {
    let object = args
        .as_object()
        .ok_or_else(|| anyhow!("Arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!("Unknown argument {key}");
    }
    Ok(())
}
