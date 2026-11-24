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
use serde_json::{json, Value};
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

/// Codebase Admin's file tools that also take `"root": "sandbox"`.
const ADMIN_FILE_TOOLS: [&str; 4] = ["file_read", "file_write", "file_list", "file_edit"];

/// Whether `name` is a Codebase Admin file tool asked to work in the sandbox folder.
#[must_use]
pub fn admin_root(name: &str, args: &Value) -> bool {
    ADMIN_FILE_TOOLS.contains(&name) && args["root"] == "sandbox"
}

/// What a Codebase Admin file tool does with `"root": "sandbox"`: one `sandbox_file`
/// request, or an edit (read, replace, write back).
#[derive(Debug, PartialEq)]
enum AdminFile {
    Request(Value),
    Edit {
        path: String,
        edits: Vec<(String, String)>,
    },
    /// A write that must not replace an existing file (`mode: create`).
    Create {
        path: String,
        content: String,
    },
    /// A write added to the end of an existing file (`mode: append`).
    Append {
        path: String,
        content: String,
    },
}

/// The sandbox form of Codebase Admin tool `name`'s `args`; source-only fields are refused.
fn admin_request(name: &str, args: &Value) -> Result<AdminFile> {
    let object = args
        .as_object()
        .ok_or_else(|| anyhow!("arguments must be an object"))?;
    let allowed: &[&str] = match name {
        "file_read" | "file_list" => &["root", "path"],
        "file_write" => &["root", "path", "content", "delete", "mode"],
        _ => &["root", "path", "old_text", "new_text", "edits"],
    };
    if let Some(extra) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        bail!(
            "{extra} does not apply with root sandbox; {name} takes {} there",
            allowed[1..].join(", ")
        );
    }
    let path = args["path"].as_str().unwrap_or("");
    let file = match name {
        "file_read" => json!({"action":"read","path":path}),
        "file_list" => json!({"action":"list","path":path}),
        "file_write" if args["delete"] == true => {
            if !args["content"].is_null() || !args["mode"].is_null() {
                bail!("delete takes no content or mode");
            }
            json!({"action":"delete","path":path})
        }
        "file_write" if matches!(args["mode"].as_str(), Some("create" | "append")) => {
            let content = args["content"]
                .as_str()
                .ok_or_else(|| anyhow!("write needs content as text"))?
                .to_owned();
            if path.is_empty() {
                bail!("write needs a path");
            }
            let path = path.to_owned();
            return Ok(if args["mode"] == "create" {
                AdminFile::Create { path, content }
            } else {
                AdminFile::Append { path, content }
            });
        }
        "file_write" if !args["mode"].is_null() && args["mode"] != "replace" => {
            bail!("mode is create, replace or append")
        }
        "file_write" => json!({"action":"write","path":path,"content":args["content"]}),
        _ => {
            if path.is_empty() {
                bail!("file_edit needs a path");
            }
            let mut edits = Vec::new();
            if let Some(list) = args["edits"].as_array() {
                if !args["old_text"].is_null() || !args["new_text"].is_null() {
                    bail!("use edits or old_text/new_text, not both");
                }
                for edit in list {
                    edits.push(edit_pair(&edit["old_text"], &edit["new_text"])?);
                }
            } else {
                edits.push(edit_pair(&args["old_text"], &args["new_text"])?);
            }
            if edits.is_empty() || edits.len() > 20 {
                bail!("edits takes 1 to 20 replacements");
            }
            return Ok(AdminFile::Edit {
                path: path.to_owned(),
                edits,
            });
        }
    };
    Ok(AdminFile::Request(file::request(&file)?))
}

fn edit_pair(old: &Value, new: &Value) -> Result<(String, String)> {
    match (old.as_str(), new.as_str()) {
        (Some(old), Some(new)) if !old.is_empty() => Ok((old.to_owned(), new.to_owned())),
        _ => bail!("each edit needs old_text (not empty) and new_text"),
    }
}

/// `text` with every `(old, new)` replacement applied together: each `old` must occur
/// exactly once in the original and the matched spans may not overlap.
fn edited(text: &str, edits: &[(String, String)]) -> Result<String> {
    let mut spans = Vec::new();
    for (index, (old, new)) in edits.iter().enumerate() {
        let mut found = text.match_indices(old.as_str());
        let Some((start, _)) = found.next() else {
            bail!("edit {} old_text was not found", index + 1);
        };
        if found.next().is_some() {
            bail!("edit {} old_text must match exactly once", index + 1);
        }
        spans.push((start, start + old.len(), new.as_str()));
    }
    spans.sort_by_key(|span| span.0);
    if spans.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        bail!("edits overlap");
    }
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end, new) in spans {
        out.push_str(&text[at..start]);
        out.push_str(new);
        at = end;
    }
    out.push_str(&text[at..]);
    Ok(out)
}

/// Run Codebase Admin file tool `name` with `"root": "sandbox"` through the `sandbox_file`
/// handler: the call's own authority, plus a live check that `sandbox_file` is allowed, so
/// the root never widens what the Sandbox ability permits.
pub async fn execute_admin_root(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let request = match admin_request(name, args) {
        Ok(request) => request,
        Err(error) => {
            return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                "output":format!("Not run; nothing changed. {error}")}))
        }
    };
    let run = host.authorize(call, name, args, &cancel).await?;
    if host
        .dependency(call.sid, &run, "sandbox_file")
        .await
        .is_err()
    {
        return Ok(json!({"status":"denied","output":
            "root sandbox uses the Sandbox ability's sandbox_file tool, which this agent cannot use now"}));
    }
    let owner = Owner {
        root: host.store().root.clone(),
        agent: host.session_agent_id(call.sid).await?,
        user: host.actor().to_owned(),
    };
    sync_saved(host, &owner, call.sid, &run, cancel.clone()).await?;
    match request {
        AdminFile::Request(request) => {
            host.worker("sandbox_file", &request, call.sid, &run, cancel)
                .await
        }
        AdminFile::Create { path, content } => {
            let read = json!({"action":"read","path":path});
            let read = host
                .worker("sandbox_file", &read, call.sid, &run, cancel.clone())
                .await?;
            if read["status"] == "success" {
                return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                    "output":format!("Not run; nothing changed. {path} already exists; use mode replace or append")}));
            }
            let write = file::request(&json!({"action":"write","path":path,"content":content}))?;
            host.worker("sandbox_file", &write, call.sid, &run, cancel)
                .await
        }
        AdminFile::Append { path, content } => {
            let read = json!({"action":"read","path":path});
            let read = host
                .worker("sandbox_file", &read, call.sid, &run, cancel.clone())
                .await?;
            if read["truncated"] == true {
                return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                    "output":"Not run; nothing changed. The file is larger than one read; append with sandbox_python"}));
            }
            // A missing file is started, as an append to nothing.
            let before = read["output"]
                .as_str()
                .filter(|_| read["status"] == "success");
            let next = format!("{}{content}", before.unwrap_or_default());
            let write = file::request(&json!({"action":"write","path":path,"content":next}))?;
            host.worker("sandbox_file", &write, call.sid, &run, cancel)
                .await
        }
        AdminFile::Edit { path, edits } => {
            let read = json!({"action":"read","path":path});
            let read = host
                .worker("sandbox_file", &read, call.sid, &run, cancel.clone())
                .await?;
            if read["status"] != "success" {
                return Ok(read);
            }
            if read["truncated"] == true {
                return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                    "output":"Not run; nothing changed. The file is larger than one read; edit it with sandbox_python"}));
            }
            let next = match edited(read["output"].as_str().unwrap_or_default(), &edits) {
                Ok(next) => next,
                Err(error) => {
                    return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                        "output":format!("Not run; nothing changed. {error}")}))
                }
            };
            let write = file::request(&json!({"action":"write","path":path,"content":next}))?;
            host.worker("sandbox_file", &write, call.sid, &run, cancel)
                .await
        }
    }
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

#[cfg(test)]
mod admin_root_tests {
    use super::*;

    #[test]
    fn admin_file_tools_map_onto_sandbox_file_requests() {
        assert!(admin_root(
            "file_write",
            &json!({"root":"sandbox","path":"a"})
        ));
        assert!(!admin_root("file_write", &json!({"path":"a"})));
        assert!(!admin_root("file_search", &json!({"root":"sandbox"})));
        assert_eq!(
            admin_request(
                "file_write",
                &json!({"root":"sandbox","path":"a/b.txt","content":"x"})
            )
            .expect("write"),
            AdminFile::Request(json!({"action":"write","path":"a/b.txt","content":"x"}))
        );
        assert_eq!(
            admin_request("file_list", &json!({"root":"sandbox"})).expect("list"),
            AdminFile::Request(json!({"action":"list","path":""}))
        );
        assert_eq!(
            admin_request(
                "file_write",
                &json!({"root":"sandbox","path":"a","delete":true})
            )
            .expect("delete"),
            AdminFile::Request(json!({"action":"delete","path":"a"}))
        );
        // Source-only fields are refused before anything runs.
        assert!(admin_request(
            "file_write",
            &json!({"root":"sandbox","path":"a","content":"x",
            "broker_id":"new"})
        )
        .is_err());
        assert!(admin_request("file_write", &json!({"root":"sandbox","path":"a"})).is_err());
        // The source tool's write modes keep their meaning in the sandbox.
        assert_eq!(
            admin_request(
                "file_write",
                &json!({"root":"sandbox","path":"a","content":"x",
                "mode":"replace"})
            )
            .expect("replace"),
            AdminFile::Request(json!({"action":"write","path":"a","content":"x"}))
        );
        assert_eq!(
            admin_request(
                "file_write",
                &json!({"root":"sandbox","path":"a","content":"x",
                "mode":"create"})
            )
            .expect("create"),
            AdminFile::Create {
                path: "a".to_owned(),
                content: "x".to_owned()
            }
        );
        assert!(matches!(
            admin_request(
                "file_write",
                &json!({"root":"sandbox","path":"a","content":"x",
                "mode":"append"})
            ),
            Ok(AdminFile::Append { .. })
        ));
    }

    #[test]
    fn sandbox_edits_apply_together_or_not_at_all() {
        let edits = |pairs: &[(&str, &str)]| {
            pairs
                .iter()
                .map(|(old, new)| ((*old).to_owned(), (*new).to_owned()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            edited("one two three", &edits(&[("three", "3"), ("one", "1")])).expect("edit"),
            "1 two 3"
        );
        assert!(edited("a a", &edits(&[("a", "b")])).is_err());
        assert!(edited("abc", &edits(&[("x", "y")])).is_err());
        assert!(edited("abcd", &edits(&[("abc", "x"), ("bcd", "y")])).is_err());
        assert_eq!(
            admin_request(
                "file_edit",
                &json!({"root":"sandbox","path":"n.txt",
                "old_text":"a","new_text":"b"})
            )
            .expect("edit"),
            AdminFile::Edit {
                path: "n.txt".to_owned(),
                edits: edits(&[("a", "b")])
            }
        );
    }
}
