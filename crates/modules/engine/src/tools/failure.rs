//! Content-free failure evidence; observation never authorizes replay or changes recovery.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::time::Instant;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Preparation,
    Authorization,
    Connect,
    RequestWrite,
    ResponseRead,
    ResponseValidation,
    Execution,
    ExecutionTask,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Code {
    InternalError,
    InvalidRequest,
    AuthorizationRejected,
    ScopeChanged,
    AuthorityStopped,
    LegacyAuthorizationRejected,
    DeploymentAuthorizationRejected,
    SocketUnavailable,
    ConnectionRefused,
    IoFailure,
    DisconnectedResponse,
    MalformedResponse,
    ResponseTooLarge,
    ResponseIdMismatch,
    Cancelled,
    TaskPanicked,
    TaskTerminated,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    NotSent,
    PossiblySent,
    ResponseReceived,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Failure {
    pub version: u8,
    pub stage: Stage,
    pub code: Code,
    pub delivery: Delivery,
    pub elapsed_ms: u64,
    pub recorded_at: u64,
    pub transport_request_id: Option<String>,
    pub request_id: Option<String>,
    pub os_error: Option<i32>,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Tool failure: {:?}/{:?}", self.stage, self.code)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    pub fn new(stage: Stage, code: Code, delivery: Delivery) -> Self {
        Self {
            version: 1,
            stage,
            code,
            delivery,
            elapsed_ms: 0,
            recorded_at: crate::now(),
            transport_request_id: None,
            request_id: None,
            os_error: None,
        }
    }
    pub fn transport(
        stage: Stage,
        code: Code,
        delivery: Delivery,
        start: Instant,
        id: &str,
        io: Option<&std::io::Error>,
    ) -> Self {
        Self {
            elapsed_ms: millis(start),
            transport_request_id: Some(id.into()),
            os_error: io.and_then(std::io::Error::raw_os_error),
            ..Self::new(stage, code, delivery)
        }
    }
}
pub fn task_error(error: Option<&tokio::task::JoinError>, start: Instant) -> anyhow::Error {
    let mut failure = Failure::new(
        Stage::ExecutionTask,
        if error.is_some_and(|e| e.is_panic()) {
            Code::TaskPanicked
        } else {
            Code::TaskTerminated
        },
        Delivery::PossiblySent,
    );
    failure.elapsed_ms = millis(start);
    failure.into()
}
fn millis(start: Instant) -> u64 {
    start.elapsed().as_millis().min(u64::MAX as u128) as u64
}
pub fn boundary(
    error: anyhow::Error,
    stage: Stage,
    delivery: Delivery,
    start: Instant,
) -> anyhow::Error {
    let mut failure = error
        .downcast_ref::<Failure>()
        .cloned()
        .unwrap_or_else(|| Failure::new(stage, Code::InternalError, delivery));
    failure.elapsed_ms = millis(start);
    // Do not retain or stringify arbitrary error chains, panic payloads or tool data.
    failure.into()
}
pub fn request(error: anyhow::Error, args: &Value) -> anyhow::Error {
    let mut failure = error.downcast_ref::<Failure>().cloned().unwrap_or_else(|| {
        Failure::new(Stage::Authorization, Code::InternalError, Delivery::NotSent)
    });
    failure.request_id = identifier(&args["request_id"]).as_str().map(str::to_owned);
    failure.into()
}
// Preserve historical labels only for records without typed evidence. This is presentation metadata.
/// A refusal recorded before any request byte left the process proves the call had
/// no effect. Stop, cancellation and transport failures keep their interruption handling.
pub fn refused_before_dispatch(value: &Value) -> Option<Failure> {
    let failure = serde_json::from_value::<Failure>(value.clone()).ok()?;
    (failure.version == 1
        && matches!(failure.delivery, Delivery::NotSent)
        && matches!(
            failure.code,
            Code::AuthorizationRejected
                | Code::LegacyAuthorizationRejected
                | Code::DeploymentAuthorizationRejected
                | Code::InvalidRequest
        ))
    .then_some(failure)
}
/// Content-free tool result for a refusal; the fixed code is the only failure detail.
pub fn refused_result(failure: &Failure) -> Value {
    json!({"status":"error","error_kind":"refused_before_dispatch","code":failure.code,
        "output":"Refused before dispatch: this tool is not authorized or the request is invalid for this agent, mode or account. No effect occurred."})
}
pub fn interruption_reason(value: &Value) -> &'static str {
    if value.is_null() {
        return "transport_error";
    }
    let Ok(failure) = serde_json::from_value::<Failure>(value.clone()) else {
        return "uncertain";
    };
    if failure.version != 1 {
        return "uncertain";
    }
    match (failure.stage, failure.code) {
        (_, Code::Cancelled) => "cancelled",
        (
            Stage::Connect | Stage::RequestWrite | Stage::ResponseRead,
            Code::SocketUnavailable
            | Code::ConnectionRefused
            | Code::IoFailure
            | Code::DisconnectedResponse,
        ) => "transport_error",
        _ => "uncertain",
    }
}
fn identifier(value: &Value) -> Value {
    value
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-:.".contains(&b))
        })
        .map(|s| json!(s))
        .unwrap_or(Value::Null)
}
/// Reproject even stored records: unknown fields and invalid enum values never become telemetry.
pub fn project(value: &Value) -> Value {
    let Ok(mut failure) = serde_json::from_value::<Failure>(value.clone()) else {
        return Value::Null;
    };
    if failure.version != 1 {
        return Value::Null;
    }
    failure.transport_request_id = identifier(&json!(failure.transport_request_id))
        .as_str()
        .map(str::to_owned);
    let mut result = json!(failure);
    for field in [
        "session_id",
        "run_id",
        "call_id",
        "pending_id",
        "tool",
        "request_id",
        "broker_id",
        "job_id",
    ] {
        result[field] = identifier(&value[field]);
    }
    result
}
pub fn evidence(error: &anyhow::Error, run: &Value, pending: &Value) -> Value {
    let failure = error.downcast_ref::<Failure>().cloned().unwrap_or_else(|| {
        Failure::new(
            Stage::Preparation,
            Code::InternalError,
            Delivery::PossiblySent,
        )
    });
    let mut value = json!(failure);
    let operation_id = identifier(&pending["arguments"]["request_id"]);
    let operation_id = if operation_id.is_null() {
        value["request_id"].clone()
    } else {
        operation_id
    };
    for (field, source) in [
        ("session_id", &run["session_id"]),
        ("run_id", &run["id"]),
        ("call_id", &pending["call_id"]),
        ("pending_id", &pending["id"]),
        ("tool", &pending["name"]),
        ("request_id", &operation_id),
        ("broker_id", &pending["arguments"]["broker_id"]),
        ("job_id", &pending["external_job"]["job_id"]),
    ] {
        value[field] = identifier(source);
    }
    if pending["arguments"]["broker_id"].is_null() {
        value["broker_id"] = identifier(&pending["arguments"]["task_id"]);
    }
    if value["broker_id"] == "new" {
        value["broker_id"] = Value::Null;
    }
    project(&value)
}
pub fn record(run: &mut Value, error: &anyhow::Error, index: Option<usize>) {
    let pending = match index {
        Some(i) => &run["batch"][i]["pending"],
        None => &run["pending"],
    };
    let value = evidence(error, run, pending);
    if let Some(i) = index {
        run["batch"][i]["pending"]["failure"] = value.clone();
    } else {
        run["pending"]["failure"] = value.clone();
    }
    run["latest_tool_failure"] = value;
}

/// Permit one content-free diagnostic attachment to an otherwise immutable terminal
/// checkpoint. The storage revision/generation/lease fences still apply; no control,
/// input, approval, result or execution metadata can change through this exception.
pub fn diagnostic_only_update(previous: &Value, next: &Value) -> bool {
    if !matches!(previous["status"].as_str(), Some("cancelled" | "failed"))
        || previous["pending"]["state"] != "executing"
        || !previous["pending"]["failure"].is_null()
    {
        return false;
    }
    let failure = &next["pending"]["failure"];
    if !failure.is_object() || project(failure) != *failure
        || next["latest_tool_failure"] != *failure
    {
        return false;
    }
    for (field, source) in [
        ("session_id", &previous["session_id"]),
        ("run_id", &previous["id"]),
        ("pending_id", &previous["pending"]["id"]),
        ("call_id", &previous["pending"]["call_id"]),
        ("tool", &previous["pending"]["name"]),
    ] {
        let expected = identifier(source);
        if expected.is_null() || failure[field] != expected { return false; }
    }
    let mut expected = previous.clone();
    expected["pending"]["failure"] = failure.clone();
    expected["latest_tool_failure"] = failure.clone();
    expected == *next
}


#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_typed_pre_dispatch_refusals_count_as_no_effect() {
        let refusal = |stage, code, delivery| serde_json::to_value(Failure::new(stage, code, delivery)).unwrap();
        let legacy = refusal(Stage::Authorization, Code::LegacyAuthorizationRejected, Delivery::NotSent);
        let failure = refused_before_dispatch(&legacy).unwrap();
        let result = refused_result(&failure);
        assert_eq!(result["code"], "legacy_authorization_rejected");
        assert_eq!(result["status"], "error");
        for value in [
            refusal(Stage::Authorization, Code::LegacyAuthorizationRejected, Delivery::PossiblySent),
            refusal(Stage::Preparation, Code::Cancelled, Delivery::NotSent),
            refusal(Stage::Authorization, Code::AuthorityStopped, Delivery::NotSent),
            refusal(Stage::Connect, Code::ConnectionRefused, Delivery::NotSent),
            json!({"version":2,"stage":"authorization","code":"authorization_rejected","delivery":"not_sent"}),
            Value::Null,
        ] {
            assert!(refused_before_dispatch(&value).is_none(), "{value}");
        }
    }
    #[test]
    fn late_single_tool_terminal_diagnostic_exception_rejects_every_other_mutation() {
        let previous = json!({"id":"run","session_id":"session","generation":"generation",
            "status":"cancelled","uncertain":false,"input":[],"run_revision":3,
            "pending":{"id":"attempt","call_id":"call","name":"file_list","state":"executing"}});
        let error = Failure::new(Stage::ResponseRead,Code::DisconnectedResponse,Delivery::PossiblySent).into();
        let mut next = previous.clone();
        record(&mut next,&error,None);
        assert!(diagnostic_only_update(&previous,&next));
        for (pointer,value) in [
            ("/status",json!("running")),("/uncertain",json!(true)),
            ("/generation",json!("replacement")),("/run_revision",json!(4)),
            ("/input",json!([{"type":"function_call_output"}])),
            ("/pending/id",json!("replacement")),("/pending/state",json!("requested")),
            ("/pending/failure/code",json!("private error")),
            ("/pending/failure/pending_id",json!("other")),
        ] {
            let mut invalid=next.clone();*invalid.pointer_mut(pointer).unwrap()=value;
            if pointer.starts_with("/pending/failure/") {
                invalid["latest_tool_failure"]=invalid["pending"]["failure"].clone();
            }
            assert!(!diagnostic_only_update(&previous,&invalid),"{pointer}");
        }
        let mut invalid=next.clone();invalid["pending"]["failure"]["raw_error"]=json!("private error");
        invalid["latest_tool_failure"]=invalid["pending"]["failure"].clone();
        assert!(!diagnostic_only_update(&previous,&invalid));
        assert!(!diagnostic_only_update(&next,&next),"diagnostics cannot overwrite prior evidence");
        let mut completed=previous.clone();completed["status"]=json!("completed");
        let mut completed_next=next.clone();completed_next["status"]=json!("completed");
        assert!(!diagnostic_only_update(&completed,&completed_next));
    }

    #[test]
    fn typed_boundaries_preserve_cause_without_secret_text() {
        let start = Instant::now();
        let error = anyhow::anyhow!("SECRET_CANARY password cookie /private/socket");
        let error = boundary(error, Stage::Authorization, Delivery::NotSent, start);
        let error = boundary(error, Stage::Preparation, Delivery::PossiblySent, start);
        let mut run = json!({"id":"run-1", "session_id":"session-1", "pending":{"id":"attempt-1", "name":"build_work", "call_id":"call-1", "arguments":{"request_id":"github-fields-begin", "secret":"SECRET_CANARY"}}});
        record(&mut run, &error, None);
        assert_eq!(run["latest_tool_failure"]["stage"], "authorization");
        assert_eq!(run["latest_tool_failure"]["delivery"], "not_sent");
        assert_eq!(
            interruption_reason(&run["latest_tool_failure"]),
            "uncertain"
        );
        assert_eq!(interruption_reason(&Value::Null), "transport_error");
        assert_eq!(
            run["latest_tool_failure"]["request_id"],
            "github-fields-begin"
        );
        assert!(!run["latest_tool_failure"]
            .to_string()
            .contains("SECRET_CANARY"));
        let reloaded: Value = serde_json::from_str(&run.to_string()).unwrap();
        assert_eq!(
            project(&reloaded["latest_tool_failure"]),
            run["latest_tool_failure"]
        );
    }
    #[test]
    fn parallel_evidence_and_projection_are_scoped_and_bounded() {
        let mut run = json!({"id":"r", "session_id":"s", "batch":[{"pending":{"id":"a", "call_id":"one"}},{"pending":{"id":"b", "call_id":"two"}}]});
        let error: anyhow::Error = Failure::new(
            Stage::ExecutionTask,
            Code::TaskPanicked,
            Delivery::PossiblySent,
        )
        .into();
        record(&mut run, &error, Some(1));
        assert!(run["batch"][0]["pending"]["failure"].is_null());
        assert_eq!(run["batch"][1]["pending"]["failure"]["call_id"], "two");
        let mut value = run["latest_tool_failure"].clone();
        value["raw_error"] = json!("SECRET_CANARY");
        value["request_id"] = json!("bad\nidentifier");
        assert!(project(&value)["raw_error"].is_null());
        assert!(project(&value)["request_id"].is_null());
        value["code"] = json!("SECRET_CANARY");
        assert!(project(&value).is_null());
        assert!(project(&json!({})).is_null());
    }
    async fn transport_fixture(mode: &str, args: Value) -> anyhow::Result<Value> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("worker.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let mode = mode.to_owned();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            if mode == "write_failure" {
                drop(stream);
                return;
            }
            let mut reader = BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            let request: Value = serde_json::from_str(&request).unwrap();
            let mut stream = reader.into_inner();
            if mode == "invalid_utf8" {
                let _ = stream.write_all(&[0xff, b'\n']).await;
                return;
            }
            let response = match mode.as_str() {
                "eof" => return,
                "malformed" => "SECRET_CANARY not json\n".to_owned(),
                "incomplete" => "{\"id\":".to_owned(),
                "valid_eof" => {
                    json!({"version":2,"id":request["id"],"result":{"status":"success","output":"ok"}}).to_string()
                }
                "oversized" => "x".repeat(2 * 1024 * 1024 + 1),
                "wrong_id" => json!({"id":"wrong","status":"success"}).to_string() + "\n",
                _ => {
                    json!({"version":2,"id":request["id"],"result":{"status":"success","output":"ok"}}).to_string() + "\n"
                }
            };
            let _ = stream.write_all(response.as_bytes()).await;
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            crate::tools::transport::execute_trusted(
                "build_work",
                &args,
                "00000000-0000-4000-8000-000000000001",
                &socket,
                None,
                tokio_util::sync::CancellationToken::new(),
            ),
        )
        .await
        .unwrap();
        server.await.unwrap();
        result
    }
    #[tokio::test]
    async fn fault_injected_transport_failures_preserve_specific_evidence() {
        for (mode, stage, code, delivery) in [
            (
                "invalid_utf8",
                "response_validation",
                "malformed_response",
                "response_received",
            ),
            (
                "eof",
                "response_read",
                "disconnected_response",
                "possibly_sent",
            ),
            (
                "malformed",
                "response_validation",
                "malformed_response",
                "response_received",
            ),
            (
                "incomplete",
                "response_validation",
                "malformed_response",
                "response_received",
            ),
            (
                "oversized",
                "response_validation",
                "response_too_large",
                "response_received",
            ),
            (
                "wrong_id",
                "response_validation",
                "response_id_mismatch",
                "response_received",
            ),
            (
                "write_failure",
                "request_write",
                "io_failure",
                "possibly_sent",
            ),
        ] {
            let args = if mode == "write_failure" {
                json!({"test_data":"x".repeat(900_000)})
            } else {
                json!({})
            };
            let error = transport_fixture(mode, args).await.unwrap_err();
            let failure = error.downcast_ref::<Failure>().unwrap();
            let value = json!(failure);
            assert_eq!(value["stage"], stage, "{mode}");
            assert_eq!(value["code"], code, "{mode}");
            assert_eq!(value["delivery"], delivery, "{mode}");
            assert!(
                uuid::Uuid::parse_str(failure.transport_request_id.as_deref().unwrap()).is_ok()
            );
            assert!(!value.to_string().contains("SECRET_CANARY"));
        }
        assert_eq!(
            transport_fixture("success", json!({})).await.unwrap()["output"],
            "ok"
        );
        assert_eq!(
            transport_fixture("valid_eof", json!({})).await.unwrap()["output"],
            "ok"
        );
    }
    #[tokio::test]
    async fn missing_refused_socket_and_cancellation_are_distinct() {
        let root = tempfile::tempdir().unwrap();
        let socket = root.path().join("missing.sock");
        let cancelled = tokio_util::sync::CancellationToken::new();
        cancelled.cancel();
        for (token, code) in [
            (cancelled, "cancelled"),
            (
                tokio_util::sync::CancellationToken::new(),
                "socket_unavailable",
            ),
        ] {
            let error = crate::tools::transport::execute_trusted(
                "build_work",
                &json!({}),
                "00000000-0000-4000-8000-000000000001",
                &socket,
                None,
                token,
            )
            .await
            .unwrap_err();
            let value = json!(error.downcast_ref::<Failure>().unwrap());
            assert_eq!(value["code"], code);
            assert_eq!(value["delivery"], "not_sent");
        }
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        drop(listener);
        let error = crate::tools::transport::execute_trusted(
            "build_work",
            &json!({}),
            "00000000-0000-4000-8000-000000000001",
            &socket,
            None,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap_err();
        let value = json!(error.downcast_ref::<Failure>().unwrap());
        assert_eq!(value["code"], "connection_refused");
        assert!(value["os_error"].is_number());
    }
    #[tokio::test]
    async fn cancellation_after_send_retains_uncertain_delivery() {
        use tokio::io::AsyncBufReadExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("waiting.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        let token = tokio_util::sync::CancellationToken::new();
        let (release, released) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let mut request = String::new();
            reader.read_line(&mut request).await.unwrap();
            sent.send(()).unwrap();
            let _ = released.await;
        });
        let mut calls = tokio::task::JoinSet::new();
        let call_token = token.clone();
        calls.spawn(async move {
            crate::tools::transport::execute_trusted(
                "build_work",
                &json!({"request_id":"cancel-after-send"}),
                "00000000-0000-4000-8000-000000000001",
                &path,
                None,
                call_token,
            )
            .await
        });
        received.await.unwrap();
        token.cancel();
        let error = tokio::time::timeout(std::time::Duration::from_secs(5), calls.join_next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .unwrap_err();
        let value = json!(error.downcast_ref::<Failure>().unwrap());
        assert_eq!(value["code"], "cancelled");
        assert_eq!(value["delivery"], "possibly_sent");
        assert_eq!(value["request_id"], "cancel-after-send");
        release.send(()).unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn task_panics_and_termination_have_distinct_safe_codes() {
        let mut tasks = tokio::task::JoinSet::new();
        let start = Instant::now();
        tasks.spawn(async { panic!("synthetic task failure") });
        let error = tasks.join_next().await.unwrap().unwrap_err();
        let value = json!(task_error(Some(&error), start)
            .downcast_ref::<Failure>()
            .unwrap());
        assert_eq!(value["code"], "task_panicked");
        tasks.spawn(std::future::pending::<()>());
        tasks.abort_all();
        let error = tasks.join_next().await.unwrap().unwrap_err();
        let value = json!(task_error(Some(&error), start)
            .downcast_ref::<Failure>()
            .unwrap());
        assert_eq!(value["code"], "task_terminated");
        assert!(!value.to_string().contains("synthetic"));
    }


}
