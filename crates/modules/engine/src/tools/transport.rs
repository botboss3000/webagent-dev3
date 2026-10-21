//! Existing bounded Unix worker/protected-host RPC transport; trusted metadata is supplied by Rust composition.
use anyhow::{bail,Result};
use serde_json::{json,Value};
use tokio::{io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt,BufReader},net::UnixStream};
use tokio_util::sync::CancellationToken;
use super::{failure,outcome};
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
            | "workspace_seed"
            | "source_review"
            | "source_patch"
            | "shell"
            | "python"
            | "code_index_maintenance"
            | "code_index_scan"
            | "code_index_lookup"
            | "code_index_coverage"
            | "code_index_trace"
            | "code_index_progress"
            | "git_tool"
            | "resolve_conflict"
            | "commit_and_push"
            | "terminal_open"
            | "terminal_read"
            | "terminal_send"
            | "terminal_wait"
            | "terminal_list"
            | "terminal_close"
            | "v3_workspace_status"
            | "v3_workspace_action"
            | "v3_cargo_task"
    ) {
        bail!("unknown tool");
    }
    execute_trusted(name,args,session,socket,None,cancel).await
}

// Only Rust supplies trusted browser/host metadata; it is not a model schema.
pub async fn execute_trusted(
    name:&str,args:&Value,session:&str,socket:&std::path::Path,
    trusted:Option<Value>,cancel:CancellationToken,
)->Result<Value> {
    execute_rpc(name,args,session,socket,trusted,cancel,false).await
}
/// Explicit protected-host protocol; this endpoint is not the low-level worker.
pub async fn execute_host_trusted(name:&str,args:&Value,session:&str,socket:&std::path::Path,
 trusted:Option<Value>,cancel:CancellationToken)->Result<Value> {
 execute_rpc(name,args,session,socket,trusted,cancel,true).await
}
async fn execute_rpc(name:&str,args:&Value,session:&str,socket:&std::path::Path,
 trusted:Option<Value>,cancel:CancellationToken,host:bool)->Result<Value> {
    use failure::{Code, Delivery, Failure, Stage};
    let start = std::time::Instant::now();
    let request_id = uuid::Uuid::new_v4().to_string();
    let fail = |stage, code, delivery, io: Option<&std::io::Error>| {
        let mut error=Failure::transport(stage, code, delivery, start, &request_id, io);
        error.request_id=args["request_id"].as_str().filter(|id|!id.is_empty()&&id.len()<=128&&id.bytes().all(|b|b.is_ascii_alphanumeric()||b"_-:.".contains(&b))).map(str::to_owned);
        error
    };
    uuid::Uuid::parse_str(session).map_err(|_| fail(Stage::Preparation, Code::InvalidRequest, Delivery::NotSent, None))?;
    if cancel.is_cancelled() { return Err(fail(Stage::Preparation, Code::Cancelled, Delivery::NotSent, None).into()); }
    let mut stream = UnixStream::connect(socket).await.map_err(|error| {
        let code = match error.kind() { std::io::ErrorKind::NotFound => Code::SocketUnavailable, std::io::ErrorKind::ConnectionRefused => Code::ConnectionRefused, _ => Code::IoFailure };
        fail(Stage::Connect, code, Delivery::NotSent, Some(&error))
    })?;
    let timeout = args["timeout_seconds"].as_u64().unwrap_or(120).clamp(1, 600);
    let mut request = json!({"id":request_id,"name":name,"arguments":args,"workspace":session,"timeout_seconds":timeout,"trusted":trusted});
    if !host {request["version"]=json!(webagent_context_contracts::execution::WORKER_PROTOCOL);}
    let mut encoded = serde_json::to_vec(&request).map_err(|_| fail(Stage::Preparation, Code::InvalidRequest, Delivery::NotSent, None))?;
    if encoded.len() > 1024 * 1024 { return Err(fail(Stage::Preparation, Code::InvalidRequest, Delivery::NotSent, None).into()); }
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
    if response.is_empty() { return Err(fail(Stage::ResponseRead, Code::DisconnectedResponse, Delivery::PossiblySent, None).into()); }
    let value: Value = serde_json::from_str(&response).map_err(|_| {
        let code=if response.len()>=2*1024*1024&&!response.ends_with('\n'){Code::ResponseTooLarge}else{Code::MalformedResponse};
        fail(Stage::ResponseValidation,code,Delivery::ResponseReceived,None)
    })?;
    if value["id"].as_str() != Some(request_id.as_str()) { return Err(fail(Stage::ResponseValidation, Code::ResponseIdMismatch, Delivery::ResponseReceived, None).into()); }
    let content=if host {webagent_context_contracts::execution::host_reply(value,&request_id)}
        else {webagent_context_contracts::execution::worker_reply(value,&request_id)}
        .map_err(|_|fail(Stage::ResponseValidation,Code::MalformedResponse,Delivery::ResponseReceived,None))?;
    outcome::rpc(if host {"build-host"}else{"tool-worker"},&request_id)
        .map_err(|_|fail(Stage::ResponseValidation,Code::InternalError,Delivery::ResponseReceived,None))?;
    Ok(content)
}
