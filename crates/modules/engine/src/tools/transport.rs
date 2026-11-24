//! Existing bounded Unix worker/protected-host RPC transport; trusted metadata is supplied by Rust composition.
use super::{failure, outcome};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;
pub async fn execute(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    cancel: CancellationToken,
) -> Result<Value> {
    if !matches!(
        name,
        "file_list"
            | "file_read"
            | "file_search"
            | "file_write"
            | "file_edit"
            | "checkout_edit"
            | "workspace_seed"
            | "source_review"
            | "source_patch"
            | "shell"
            | "python"
            | "git_tool"
            | "resolve_conflict"
            | "commit_and_push"
            | "terminal_open"
            | "terminal_read"
            | "terminal_send"
            | "terminal_wait"
            | "terminal_list"
            | "terminal_close"
    ) {
        bail!("unknown tool");
    }
    execute_trusted(name, args, session, socket, None, cancel).await
}

// Only Rust supplies trusted browser/host metadata; it is not a model schema.
pub async fn execute_trusted(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
) -> Result<Value> {
    execute_rpc(name, args, session, socket, trusted, cancel, false).await
}
/// Explicit protected-host protocol; this endpoint is not the low-level worker.
pub async fn execute_host_trusted(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
) -> Result<Value> {
    execute_rpc(name, args, session, socket, trusted, cancel, true).await
}
/// The live limits a worker call carries in `trusted.limits` (stream D1): the command limits
/// (`config/worker/command-limits.json`) for commands and terminals, the search budgets of
/// `config/context/context-compaction.json` for searches. The worker applies them instead of
/// reading the checkout, so a worker without it keeps the live values. `None` (the worker then
/// reads its own copy, as before) for other tools or when the file is unreadable or too large;
/// the worker validates what it gets exactly as it validates the file.
fn call_limits(name: &str, checkout: &std::path::Path) -> Option<Value> {
    let read = |relative: &str, max_bytes: u64| -> Option<Value> {
        let path = checkout.join(relative);
        if std::fs::metadata(&path).ok()?.len() > max_bytes {
            return None;
        }
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    };
    match name {
        "shell" | "python" | "terminal_open" => {
            Some(json!({"command": read("config/worker/command-limits.json", 16 * 1024)?}))
        }
        "file_search" | "file_list" => {
            let config = read("config/context/context-compaction.json", 256 * 1024)?;
            let search: serde_json::Map<String, Value> =
                ["search_output_bytes", "search_line_bytes"]
                    .into_iter()
                    .filter_map(|key| Some((key.to_owned(), config.get(key)?.clone())))
                    .collect();
            Some(json!({ "search": search }))
        }
        _ => None,
    }
}

async fn execute_rpc(
    name: &str,
    args: &Value,
    session: &str,
    socket: &std::path::Path,
    trusted: Option<Value>,
    cancel: CancellationToken,
    host: bool,
) -> Result<Value> {
    use failure::{Code, Delivery, Failure, Stage};
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let fail = |stage, code, delivery, io: Option<&std::io::Error>| {
        let mut error = Failure::transport(stage, code, delivery, start, &request_id, io);
        error.request_id = args["request_id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
            })
            .map(str::to_owned);
        error
    };
    uuid::Uuid::parse_str(session).map_err(|_| {
        fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
    })?;
    if cancel.is_cancelled() {
        return Err(fail(Stage::Preparation, Code::Cancelled, Delivery::NotSent, None).into());
    }
    let mut stream = webagent_platform::ipc::connect(socket.as_ref())
        .await
        .map_err(|error| {
            let code = match error.kind() {
                std::io::ErrorKind::NotFound => Code::SocketUnavailable,
                std::io::ErrorKind::ConnectionRefused => Code::ConnectionRefused,
                _ => Code::IoFailure,
            };
            fail(Stage::Connect, code, Delivery::NotSent, Some(&error))
        })?;
    let timeout = args["timeout_seconds"]
        .as_u64()
        .unwrap_or(120)
        .clamp(1, 600);
    let mut request = json!({"id":request_id,"name":name,"arguments":args,"workspace":session,"timeout_seconds":timeout,"trusted":trusted});
    if !host {
        request["version"] = json!(webagent_context_contracts::execution::WORKER_PROTOCOL);
        if let Some(limits) = call_limits(name, &webagent_configuration::paths::checkout()) {
            let trusted = &mut request["trusted"];
            if trusted.is_null() {
                *trusted = json!({});
            }
            if let Some(trusted) = trusted.as_object_mut() {
                trusted.entry("limits").or_insert(limits);
            }
        }
    }
    let mut encoded = serde_json::to_vec(&request).map_err(|_| {
        fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
    })?;
    if encoded.len() > 1024 * 1024 {
        return Err(fail(
            Stage::Preparation,
            Code::InvalidRequest,
            Delivery::NotSent,
            None,
        )
        .into());
    }
    encoded.push(b'\n');
    // Cancellation may race a partial write. Delivery is deliberately conservative.
    tokio::select! {
        _ = cancel.cancelled() => Err(fail(Stage::RequestWrite, Code::Cancelled, Delivery::PossiblySent, None).into()),
        result = stream.write_all(&encoded) => result.map_err(|error| anyhow::Error::from(fail(Stage::RequestWrite, Code::IoFailure, Delivery::PossiblySent, Some(&error)))),
    }?;
    let mut reader = BufReader::new(stream).take(2 * 1024 * 1024);
    let mut response = String::new();
    tokio::select! {
        _ = cancel.cancelled() => Err(fail(Stage::ResponseRead, Code::Cancelled, Delivery::PossiblySent, None).into()),
        result = reader.read_line(&mut response) => result.map_err(|error| {
            let (stage,code,delivery)=if error.kind()==std::io::ErrorKind::InvalidData {(Stage::ResponseValidation,Code::MalformedResponse,Delivery::ResponseReceived)}else{(Stage::ResponseRead,Code::IoFailure,Delivery::PossiblySent)};
            anyhow::Error::from(fail(stage,code,delivery,Some(&error)))
        }),
    }?;
    if response.is_empty() {
        return Err(fail(
            Stage::ResponseRead,
            Code::DisconnectedResponse,
            Delivery::PossiblySent,
            None,
        )
        .into());
    }
    let value: Value = serde_json::from_str(&response).map_err(|_| {
        let code = if response.len() >= 2 * 1024 * 1024 && !response.ends_with('\n') {
            Code::ResponseTooLarge
        } else {
            Code::MalformedResponse
        };
        fail(
            Stage::ResponseValidation,
            code,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    if value["id"].as_str() != Some(request_id.as_str()) {
        return Err(fail(
            Stage::ResponseValidation,
            Code::ResponseIdMismatch,
            Delivery::ResponseReceived,
            None,
        )
        .into());
    }
    let content = if host {
        webagent_context_contracts::execution::host_reply(value, &request_id)
    } else {
        webagent_context_contracts::execution::worker_reply(value, &request_id)
    }
    .map_err(|_| {
        fail(
            Stage::ResponseValidation,
            Code::MalformedResponse,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    outcome::rpc(if host { "build-host" } else { "tool-worker" }, &request_id).map_err(|_| {
        fail(
            Stage::ResponseValidation,
            Code::InternalError,
            Delivery::ResponseReceived,
            None,
        )
    })?;
    Ok(content)
}

#[cfg(test)]
mod limit_tests {
    use super::call_limits;
    use serde_json::json;

    #[test]
    fn commands_and_searches_carry_the_live_limits_others_none() {
        let dir = tempfile::tempdir().expect("temporary directory");
        for folder in ["config/worker", "config/context"] {
            std::fs::create_dir_all(dir.path().join(folder)).expect("folder");
        }
        // Absent files: nothing is sent and the worker reads its own copy, as before D1.
        assert_eq!(call_limits("shell", dir.path()), None);
        assert_eq!(call_limits("file_search", dir.path()), None);
        let command = json!({"schema_version":1,"memory_mb":512,"processes":64,"cpu_seconds":60});
        std::fs::write(
            dir.path().join("config/worker/command-limits.json"),
            command.to_string(),
        )
        .expect("write");
        std::fs::write(
            dir.path().join("config/context/context-compaction.json"),
            json!({"search_output_bytes":8192,"search_line_bytes":512,"other":1}).to_string(),
        )
        .expect("write");
        for name in ["shell", "python", "terminal_open"] {
            assert_eq!(
                call_limits(name, dir.path()),
                Some(json!({ "command": command }))
            );
        }
        for name in ["file_search", "file_list"] {
            assert_eq!(
                call_limits(name, dir.path()),
                Some(json!({"search":{"search_output_bytes":8192,"search_line_bytes":512}}))
            );
        }
        for name in ["file_read", "file_write", "git_tool", "terminal_read"] {
            assert_eq!(call_limits(name, dir.path()), None);
        }
        // A file the worker would refuse as too large is not sent either.
        std::fs::write(
            dir.path().join("config/worker/command-limits.json"),
            " ".repeat(16 * 1024 + 1),
        )
        .expect("write");
        assert_eq!(call_limits("shell", dir.path()), None);
    }
}
