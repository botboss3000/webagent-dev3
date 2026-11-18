//! `sandbox_file`: read, write, list or delete one path in the session's sandbox folder.
//! The worker resolves the path beneath the folder without following any symlink.
use super::only;
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

/// Largest `content` one write carries; it must fit the worker's 1 MiB request.
const MAX_CONTENT: usize = 512 * 1024;

/// The worker request for `args` (`action`, `path`, `content` for a write).
pub(super) fn request(args: &Value) -> Result<Value> {
    only(args, &["action", "path", "content"])?;
    let action = args["action"]
        .as_str()
        .ok_or_else(|| anyhow!("action must be read, write, list or delete"))?;
    let path = match &args["path"] {
        Value::Null => "",
        Value::String(path) if path.len() <= 512 => path,
        _ => bail!("path must be text of at most 512 bytes"),
    };
    match action {
        "list" => Ok(json!({"action":action,"path":path})),
        "read" | "delete" if !path.is_empty() => Ok(json!({"action":action,"path":path})),
        "write" if !path.is_empty() => {
            let content = args["content"]
                .as_str()
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
