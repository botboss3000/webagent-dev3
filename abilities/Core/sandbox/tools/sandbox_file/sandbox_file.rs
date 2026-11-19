//! `sandbox_file`: read, write, list or delete one path in the session's sandbox folder.
//! The worker resolves the path beneath the folder without following any symlink.
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// Largest `content` one write carries; it must fit the worker's 1 MiB request.
const MAX_CONTENT: usize = 512 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FileArgs {
    action: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    content: Option<String>,
}

/// The worker request for `args` (`action`, `path`, `content` for a write).
pub(super) fn request(args: &Value) -> Result<Value> {
    let file: FileArgs = webagent_abilities::tool_arguments::read("sandbox_file", args)?;
    let action = file.action.as_str();
    let path = file.path.as_deref().unwrap_or("");
    if path.len() > 512 {
        bail!("path must be text of at most 512 bytes");
    }
    match action {
        "list" => Ok(json!({"action":action,"path":path})),
        "read" | "delete" if !path.is_empty() => Ok(json!({"action":action,"path":path})),
        "write" if !path.is_empty() => {
            let content = file
                .content
                .as_deref()
                .ok_or_else(|| anyhow!("write needs content as text"))?;
            if content.len() > MAX_CONTENT {
                bail!("content must be at most 512 KiB per write");
            }
            Ok(json!({"action":action,"path":path,"content":content}))
        }
        "read" | "delete" | "write" => bail!("{action} needs a path"),
        _ => bail!("action must be read, write, list or delete"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_action_takes_only_its_own_arguments() {
        assert_eq!(
            request(&json!({"action":"list"})).expect("valid request"),
            json!({"action":"list","path":""})
        );
        assert!(request(&json!({"action":"read"})).is_err());
        assert!(request(&json!({"action":"write","path":"a"})).is_err());
        assert!(request(&json!({"action":"chmod","path":"a"})).is_err());
        assert!(request(&json!({"action":"read","path":"a","mode":"0777"})).is_err());
        let write = request(&json!({"action":"write","path":"a/b.html","content":"x"}))
            .expect("valid request");
        assert_eq!(write["content"], "x");
    }
}
