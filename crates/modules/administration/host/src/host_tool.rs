//! The `host_admin` tool: files host-action requests with the root host helper on behalf of
//! the calling chat, and reads their state. The helper holds every change until an
//! administrator decides it on the Host Admin page; this tool never decides anything.
use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

/// Largest helper reply read back.
const MAX_REPLY: u64 = 2 * 1024 * 1024;

/// The helper's socket under the runtime root the app shares with the host.
fn socket() -> std::path::PathBuf {
    std::path::PathBuf::from(std::env::var("V3_RUNTIME_ROOT").unwrap_or_else(|_| "/runtime".into()))
        .join("host/helper.sock")
}

async fn helper(message: &Value) -> Result<Value> {
    let mut stream = UnixStream::connect(socket())
        .await
        .map_err(|_| anyhow!("The host helper is unavailable"))?;
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    let mut reply = Vec::new();
    BufReader::new(stream.take(MAX_REPLY))
        .read_until(b'\n', &mut reply)
        .await?;
    let value: Value = serde_json::from_slice(&reply)?;
    if value["status"] != "success" {
        bail!(
            "{}",
            value["output"]
                .as_str()
                .unwrap_or("The host helper refused the request")
        );
    }
    Ok(value["output"].clone())
}

/// `host_admin`'s arguments; `host_action`/`host_args` are for `request` only and
/// `request_id` for `check` only.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arguments {
    action: String,
    #[serde(default)]
    host_action: Option<String>,
    #[serde(default)]
    host_args: Option<Map<String, Value>>,
    #[serde(default)]
    request_id: Option<String>,
}

/// One `host_admin` call: `status` (read host status now), `request` (file `host_action`
/// with `host_args`) or `check` (a request's state by `request_id`). `user` and `sid` are
/// the trusted caller, recorded as the requester so the decision wakes this chat.
pub async fn call(args: &Value, sid: &str, user: &str) -> Result<Value> {
    let args: Arguments = Arguments::deserialize(args)
        .map_err(|error| anyhow!("host_admin: invalid arguments: {error}"))?;
    let requester = json!({"session": sid, "user": user});
    let output = match args.action.as_str() {
        "status" => {
            helper(&json!({"op":"request","action":"host_status","args":{},"requester":requester}))
                .await?
        }
        "request" => {
            let action = args
                .host_action
                .filter(|a| !a.is_empty())
                .ok_or_else(|| anyhow!("request needs host_action"))?;
            let host_args = Value::Object(args.host_args.unwrap_or_default());
            helper(&json!({"op":"request","action":action,"args":host_args,"requester":requester}))
                .await?
        }
        "check" => {
            let id = args
                .request_id
                .ok_or_else(|| anyhow!("check needs request_id"))?;
            helper(&json!({"op":"status","id":id})).await?
        }
        other => bail!("Unknown host_admin action {other:?}; use status, request or check"),
    };
    let note = match output["state"].as_str() {
        Some("pending") => "Waiting for an administrator on the Host Admin page. End your turn; a background notice wakes this chat when it is decided and finished.",
        Some("running") => "Approved and running. A background notice wakes this chat when it finishes.",
        _ => "",
    };
    Ok(json!({"status":"success","output":output,"note":note}))
}
