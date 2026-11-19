//! The Sandbox ability: `sandbox_shell`, `sandbox_python` and `sandbox_file` in a session's
//! scratch folder, and the saved UI elements built there (`sandbox_save`,
//! `sandbox_elements`, `sandbox_present`, `sandbox_delete`, `sandbox_share`; storage in
//! [`elements`]). Each tool's module checks its arguments; every call then runs under
//! fresh run authority. Folder calls run on the isolated worker's strict sandbox; the root
//! adds the limits (from [`limits`]) and the storage owner's remaining room as trusted
//! fields, so a model can never choose them. Before each, the session's read-only copy of
//! the agent's saved elements is brought up to date.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

#[path = "../sandbox_delete/sandbox_delete.rs"]
mod delete;
#[path = "../sandbox_elements/sandbox_elements.rs"]
mod element_list;
#[path = "elements.rs"]
pub mod elements;
#[path = "../sandbox_file/sandbox_file.rs"]
mod file;
#[path = "../sandbox_present/sandbox_present.rs"]
mod present;
#[path = "../sandbox_python/sandbox_python.rs"]
mod python;
#[path = "../sandbox_save/sandbox_save.rs"]
mod save;
#[path = "../sandbox_share/sandbox_share.rs"]
mod share;
#[path = "../sandbox_shell/sandbox_shell.rs"]
mod shell;

/// Tools this module executes; their schemas live in the ability's `tools/<tool>/<tool>.tool.json`.
pub const TOOLS: &[&str] = &[
    "sandbox_shell",
    "sandbox_python",
    "sandbox_file",
    "sandbox_save",
    "sandbox_elements",
    "sandbox_present",
    "sandbox_delete",
    "sandbox_share",
];

/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("sandbox_delete", delete::DeleteArgs),
    webagent_abilities::argument_type!("sandbox_file", file::FileArgs),
    webagent_abilities::argument_type!("sandbox_present", present::PresentArgs),
    webagent_abilities::argument_type!("sandbox_python", python::PythonArgs),
    webagent_abilities::argument_type!("sandbox_save", save::SaveArgs),
    webagent_abilities::argument_type!("sandbox_share", share::ShareArgs),
    webagent_abilities::argument_type!("sandbox_shell", shell::ShellArgs),
];

/// The worker request the root sends with `trusted.element_sync`, never a model tool.
pub const ELEMENT_SYNC: &str = "sandbox_element";

/// Whether the root must send the Sandbox limits with worker request `name`.
#[must_use]
pub fn limited(name: &str) -> bool {
    matches!(name, "sandbox_shell" | "sandbox_python" | "sandbox_file")
}

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
        "sandbox_save" => save::check(args),
        "sandbox_elements" => element_list::check(args),
        "sandbox_present" => present::check(args),
        "sandbox_delete" => delete::check(args),
        "sandbox_share" => share::check(args),
        _ => bail!("Unknown Sandbox tool"),
    }?;
    let run = host.authorize(call, name, args, &cancel).await?;
    let owner = Owner {
        root: host.store().root.clone(),
        agent: host.session_agent_id(call.sid).await?,
        user: host.actor().to_owned(),
    };
    let config = || elements::Config::read();
    let now = webagent_host_core::now();
    let output = match name {
        "sandbox_save" => {
            let read = host
                .worker(
                    ELEMENT_SYNC,
                    &serde_json::json!({"action":"read","folder":request["folder"]}),
                    call.sid,
                    &run,
                    cancel,
                )
                .await?;
            if read["status"] != "success" {
                return Ok(read);
            }
            save::run(
                &owner,
                call.sid,
                &request,
                read["output"]["html"].as_str().unwrap_or(""),
                &config()?,
                now,
            )
        }
        "sandbox_elements" => element_list::run(&owner),
        "sandbox_present" => {
            let (id, body) = present::artifact(&owner, &request, &config()?)?;
            host.write_session_artifact(call, "ui_element", &id, &body)
                .await?;
            Ok(serde_json::json!({"status":"success","output":format!(
                "Placed \"{}\" in this chat's Artifacts chip; the user opens it there.",
                body["title"].as_str().unwrap_or("element"))}))
        }
        "sandbox_delete" => delete::run(&owner, &request),
        "sandbox_share" => share::run(&owner, &request, &config()?, now),
        _ => {
            sync_saved(host, &owner, call.sid, &run, cancel.clone()).await?;
            return host.worker(name, &request, call.sid, &run, cancel).await;
        }
    };
    Ok(output
        .unwrap_or_else(|error| serde_json::json!({"status":"failed","output":error.to_string()})))
}

/// Whose elements a call reaches: the session's agent and the user it acts for.
struct Owner {
    root: std::path::PathBuf,
    agent: String,
    user: String,
}

/// Brings the session's read-only element copy up to date: puts each saved element whose
/// revision differs and removes the ones no longer saved.
async fn sync_saved(
    host: &dyn NativeHost,
    owner: &Owner,
    sid: &str,
    run: &Value,
    cancel: CancellationToken,
) -> Result<()> {
    let wanted = elements::mirror_manifest(&owner.root, &owner.agent, &owner.user)?;
    let status = host
        .worker(
            ELEMENT_SYNC,
            &serde_json::json!({"action":"status"}),
            sid,
            run,
            cancel.clone(),
        )
        .await?;
    if status["status"] != "success" {
        bail!(
            "Saved elements unavailable: {}",
            status["output"].as_str().unwrap_or("worker error")
        );
    }
    let present = &status["output"]["revisions"];
    for (path, revision) in wanted.as_object().into_iter().flatten() {
        if &present[path] == revision {
            continue;
        }
        let (scope, name) = path.split_once('/').unwrap_or_default();
        let (_, html) = elements::read(
            &owner.root,
            &owner.agent,
            &owner.user,
            elements::Scope::parse(scope)?,
            name,
        )?;
        let put = serde_json::json!({"action":"put","path":path,"revision":revision,"html":html});
        host.worker(ELEMENT_SYNC, &put, sid, run, cancel.clone())
            .await?;
    }
    for path in present
        .as_object()
        .into_iter()
        .flatten()
        .map(|(path, _)| path)
    {
        if wanted.get(path).is_none() {
            let remove = serde_json::json!({"action":"remove","path":path});
            host.worker(ELEMENT_SYNC, &remove, sid, run, cancel.clone())
                .await?;
        }
    }
    Ok(())
}

/// An optional `timeout_seconds` argument, copied for the worker (which caps it).
fn timeout(seconds: Option<u64>) -> Result<Option<u64>> {
    match seconds {
        Some(seconds) if !(1..=600).contains(&seconds) => {
            bail!("timeout_seconds must be a whole number from 1 to 600")
        }
        seconds => Ok(seconds),
    }
}

/// Text argument `key` of at most `max` characters.
fn short<'a>(key: &str, text: &'a str, max: usize) -> Result<&'a str> {
    if text.chars().count() > max {
        bail!("{key} must be at most {max} characters");
    }
    Ok(text)
}

/// The one text argument `key` of a command tool, 1 to 65536 bytes.
fn text<'a>(key: &str, text: &'a str) -> Result<&'a str> {
    if text.is_empty() || text.len() > 65536 {
        bail!("{key} must be 1 to 65536 bytes");
    }
    Ok(text)
}

/// Refuses arguments outside `allowed`, as the tool's schema does (for `sandbox_elements`,
/// which takes none).
fn only(args: &Value, allowed: &[&str]) -> Result<()> {
    let object = args
        .as_object()
        .ok_or_else(|| anyhow!("Arguments must be an object"))?;
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!("Unknown argument {key}");
    }
    Ok(())
}
