//! Source promotion: a reviewed session patch is proposed by the model, then applied by
//! an administrator through the protected promotion broker. Every state change is a
//! durable record plus an event, so an interrupted dispatch is reported, never retried.
use crate::{workspace_host, AdministrationHost};
use anyhow::{bail, Context, Result};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;
use webagent_administration::review::{
    public_promotion, sha256, valid_review_digest, verified, ReviewError, MAX_PATCH,
};
use webagent_host_core::{bad, conflict, digest, key, now, ApiError};

fn record_key(sid: &str, request_id: &str) -> String {
    format!("promotion:{sid}:{request_id}")
}

fn not_found(message: &str) -> ApiError {
    ApiError(StatusCode::NOT_FOUND, message.into())
}

fn review_error(error: ReviewError) -> ApiError {
    match error {
        ReviewError::Changed(message) => conflict(message),
        ReviewError::Invalid(message) => bad(message),
    }
}

/// Send one reviewed patch to the promotion broker and check its reply belongs to it.
pub async fn apply(socket: &Path, patch: &str, cancel: CancellationToken) -> Result<Value> {
    if patch.is_empty() || patch.len() > MAX_PATCH {
        bail!("Invalid promotion patch length");
    }
    let id = uuid::Uuid::new_v4().to_string();
    let patch_sha256 = sha256(patch.as_bytes());
    let request = json!({"id":id,"patch":patch,"patch_sha256":patch_sha256});
    let mut encoded = serde_json::to_vec(&request)?;
    if encoded.len() > 3 * MAX_PATCH {
        bail!("Promotion request exceeds limit");
    }
    encoded.push(b'\n');
    let mut stream = UnixStream::connect(socket)
        .await
        .context("Promotion broker unavailable")?;
    let exchange = async {
        stream.write_all(&encoded).await?;
        let mut reader = BufReader::new(stream).take((3 * MAX_PATCH) as u64);
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.is_empty() {
            bail!("Promotion broker disconnected");
        }
        let reply: Value = serde_json::from_str(&line)?;
        if reply["id"].as_str() != Some(id.as_str()) {
            bail!("Promotion broker response identifier mismatch");
        }
        if !matches!(
            reply["status"].as_str(),
            Some("success" | "denied" | "uncertain")
        ) {
            bail!("Invalid promotion broker status");
        }
        if reply["status"] == "success" && reply["patch_sha256"] != patch_sha256 {
            bail!("Promotion broker patch digest mismatch");
        }
        Ok(reply)
    };
    tokio::select! {
        () = cancel.cancelled() => bail!("Promotion cancelled"),
        result = tokio::time::timeout(Duration::from_secs(90), exchange) =>
            result.context("Promotion broker timed out")?,
    }
}

/// Whether the installation still allows proposals under the policy version the run was
/// admitted with.
async fn proposals_allowed(host: &dyn AdministrationHost, policy_version: u64) -> Result<bool> {
    let policy = host.policy().await?;
    Ok(!host.shutoff().await?
        && policy["version"] == policy_version
        && host.rule(&policy, "source_review") == "allow"
        && host.rule(&policy, "promotion_propose") != "deny")
}

/// A model's proposal from one run.
pub struct Proposal<'a> {
    pub sid: &'a str,
    /// The tool call's ID, which names the proposal.
    pub request_id: &'a str,
    pub run_id: &'a str,
    pub policy_version: u64,
}

/// Record the session's current reviewed patch as a proposal for administrator approval.
/// Repeating the same request returns the existing proposal.
pub async fn propose(
    host: &dyn AdministrationHost,
    proposal: Proposal<'_>,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let Proposal {
        sid,
        request_id,
        run_id,
        policy_version,
    } = proposal;
    let Some(expected) = args["expected_review_sha256"].as_str() else {
        return Ok(json!({"status":"denied","output":"Review digest is required"}));
    };
    let Some(summary) = args["summary"].as_str() else {
        return Ok(json!({"status":"denied","output":"Change summary is required"}));
    };
    if !valid_review_digest(expected) || summary.trim().is_empty() || summary.len() > 4096 {
        return Ok(json!({"status":"denied","output":"Invalid review digest or summary"}));
    }
    let receipt_key = record_key(sid, request_id);
    let request_digest = digest(&json!({"client_request_id":request_id,
        "expected_review_sha256":expected}));
    let existing = |record: &Value| {
        if record["request_digest"] == request_digest {
            json!({"status":"success","proposal_id":request_id,"state":record["state"],
                "review_sha256":expected})
        } else {
            json!({"status":"denied","output":"Proposal identifier conflict"})
        }
    };
    let unavailable =
        json!({"status":"denied","output":"Proposal unavailable under current controls"});
    {
        let _guard = host.gate().lock().await;
        if let Some(record) = host.store().get(&receipt_key).await? {
            return Ok(existing(&record));
        }
        if !proposals_allowed(host, policy_version).await?
            || host.store().get(&key("session", sid)).await?.is_none()
        {
            return Ok(unavailable);
        }
    }
    let review = workspace_host(host, sid, "source_review", &json!({}), cancel.clone()).await?;
    let (patch, baseline) = match verified(&review, expected) {
        Ok(value) => value,
        Err(error) => return Ok(json!({"status":"denied","output":error.message()})),
    };
    let patch_sha256 = sha256(patch.as_bytes());
    let record = json!({"session_id":sid,"client_request_id":request_id,
        "request_digest":request_digest,"state":"proposed","review_sha256":expected,
        "baseline_commit":baseline,"patch_sha256":patch_sha256,
        "patch":patch,"status_output":review["status_output"],"summary":summary,
        "proposed_by_run":run_id,"paths":[],"created_at":now(),"updated_at":now()});
    let _guard = host.gate().lock().await;
    if let Some(record) = host.store().get(&receipt_key).await? {
        return Ok(existing(&record));
    }
    if cancel.is_cancelled() || !proposals_allowed(host, policy_version).await? {
        return Ok(unavailable);
    }
    let event = json!({"client_request_id":request_id,"review_sha256":expected,
        "patch_sha256":patch_sha256});
    host.record(
        None,
        vec![(receipt_key, record)],
        sid,
        "promotion_proposed",
        &event,
    )
    .await?;
    Ok(
        json!({"status":"success","proposal_id":request_id,"state":"proposed",
        "review_sha256":expected,"patch_sha256":patch_sha256}),
    )
}

async fn require_session(host: &dyn AdministrationHost, sid: &str) -> Result<(), ApiError> {
    if host.store().get(&key("session", sid)).await?.is_none() {
        return Err(not_found("Session not found"));
    }
    Ok(())
}

async fn promotion_record(
    host: &dyn AdministrationHost,
    sid: &str,
    request_id: &str,
) -> Result<Value, ApiError> {
    require_session(host, sid).await?;
    host.store()
        .get(&record_key(sid, request_id))
        .await?
        .ok_or_else(|| not_found("Promotion not found"))
}

/// The session's promotions, without patches.
pub async fn list(host: &dyn AdministrationHost, sid: &str) -> Result<Value, ApiError> {
    require_session(host, sid).await?;
    let records = host.store().list(&format!("promotion:{sid}:")).await?;
    let public: Vec<Value> = records.iter().map(public_promotion).collect();
    Ok(json!({"promotions":public}))
}

/// One promotion with the patch an administrator reviews before applying it.
pub async fn review(
    host: &dyn AdministrationHost,
    sid: &str,
    request_id: &str,
) -> Result<Value, ApiError> {
    let record = promotion_record(host, sid, request_id).await?;
    Ok(json!({"promotion":public_promotion(&record),
        "baseline_commit":record["baseline_commit"],"status_output":record["status_output"],
        "diff":record["patch"]}))
}

/// One promotion's public receipt.
pub async fn receipt(
    host: &dyn AdministrationHost,
    sid: &str,
    request_id: &str,
) -> Result<Value, ApiError> {
    Ok(public_promotion(
        &promotion_record(host, sid, request_id).await?,
    ))
}

/// Reject a proposed promotion; rejecting it again returns the same record.
pub async fn reject(
    host: &dyn AdministrationHost,
    sid: &str,
    request_id: &str,
) -> Result<Value, ApiError> {
    if uuid::Uuid::parse_str(request_id).is_err() {
        return Err(bad("Invalid promotion identifier"));
    }
    let _guard = host.gate().lock().await;
    let mut record = promotion_record(host, sid, request_id).await?;
    if record["state"] == "rejected" {
        return Ok(public_promotion(&record));
    }
    if record["state"] != "proposed" {
        return Err(conflict("Only a proposed promotion can be rejected"));
    }
    record["state"] = json!("rejected");
    record["updated_at"] = json!(now());
    let event = json!({"client_request_id":request_id,"patch_sha256":record["patch_sha256"]});
    let docs = vec![(record_key(sid, request_id), record.clone())];
    host.record(None, docs, sid, "promotion_rejected", &event)
        .await?;
    Ok(public_promotion(&record))
}

fn required<'a>(body: &'a Value, name: &str, max: usize) -> Result<&'a str, ApiError> {
    let value = body[name]
        .as_str()
        .ok_or_else(|| bad(&format!("{name} is required")))?;
    if value.is_empty() || value.len() > max {
        return Err(bad(&format!("{name} has invalid length")));
    }
    Ok(value)
}

/// Fail unless source review is allowed and the shutoff is off.
async fn promotion_open(host: &dyn AdministrationHost) -> Result<(), ApiError> {
    let policy = host.policy().await?;
    if host.rule(&policy, "source_review") != "allow" {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Source review is unavailable under current policy".into(),
        ));
    }
    if host.shutoff().await? {
        return Err(conflict("Global shutoff is active"));
    }
    Ok(())
}

/// Apply session `sid`'s reviewed patch through the promotion broker. The request names
/// the review digest the administrator saw; a changed review is refused, and a repeated
/// request returns the recorded outcome.
pub async fn promote(
    host: &dyn AdministrationHost,
    sid: &str,
    body: &Value,
) -> Result<Value, ApiError> {
    let request_id = required(body, "client_request_id", 128)?.to_owned();
    if uuid::Uuid::parse_str(&request_id).is_err() {
        return Err(bad("client_request_id must be a UUID"));
    }
    let expected = required(body, "expected_review_sha256", 64)?.to_owned();
    if !valid_review_digest(&expected) {
        return Err(bad("Invalid review digest"));
    }
    let receipt_key = record_key(sid, &request_id);
    let request_digest = digest(body);
    {
        let _guard = host.gate().lock().await;
        if let Some(record) = host.store().get(&receipt_key).await? {
            if record["request_digest"] != request_digest {
                return Err(conflict("Promotion identifier reused with different input"));
            }
            if record["state"] != "proposed" {
                return Ok(public_promotion(&record));
            }
        }
        require_session(host, sid).await?;
        promotion_open(host).await?;
    }
    let review = workspace_host(
        host,
        sid,
        "source_review",
        &json!({}),
        CancellationToken::new(),
    )
    .await?;
    let (patch, _baseline) = verified(&review, &expected).map_err(review_error)?;
    let patch_sha256 = sha256(patch.as_bytes());
    let mut record = json!({"session_id":sid,"client_request_id":request_id,
        "request_digest":request_digest,"state":"dispatching","review_sha256":expected,
        "baseline_commit":review["baseline_commit"],"patch_sha256":patch_sha256,
        "patch":patch,"paths":[],"created_at":now(),"updated_at":now()});
    {
        let _guard = host.gate().lock().await;
        if let Some(existing) = host.store().get(&receipt_key).await? {
            if existing["request_digest"] != request_digest {
                return Err(conflict("Promotion identifier reused with different input"));
            }
            if existing["state"] != "proposed" {
                return Ok(public_promotion(&existing));
            }
            if existing["patch_sha256"] != patch_sha256 || existing["review_sha256"] != expected {
                return Err(conflict("Proposed patch changed since review"));
            }
            record = existing;
            record["state"] = json!("dispatching");
            record["updated_at"] = json!(now());
        }
        promotion_open(host).await?;
        let event = json!({"client_request_id":request_id,"review_sha256":expected,
            "patch_sha256":patch_sha256});
        let docs = vec![(receipt_key.clone(), record.clone())];
        host.record(None, docs, sid, "promotion_requested", &event)
            .await?;
    }
    let result = apply(host.promotion_socket(), patch, host.stopping()).await;
    record["state"] = json!(match &result {
        Ok(reply) if reply["status"] == "success" => "applied",
        Ok(reply) if reply["status"] == "denied" => "denied",
        _ => "uncertain",
    });
    if let Ok(reply) = result {
        record["paths"] = reply["paths"].clone();
    }
    record["updated_at"] = json!(now());
    {
        let _guard = host.gate().lock().await;
        let event = json!({"client_request_id":request_id,"state":record["state"],
            "patch_sha256":patch_sha256});
        let docs = vec![(receipt_key, record.clone())];
        host.record(None, docs, sid, "promotion_result", &event)
            .await?;
    }
    Ok(public_promotion(&record))
}

/// At startup, mark every promotion left dispatching as uncertain: it may or may not have
/// reached the broker, and only an administrator can reconcile it.
pub async fn recover(host: &dyn AdministrationHost) -> Result<()> {
    let _guard = host.gate().lock().await;
    for mut record in host.store().list("promotion:").await? {
        if record["state"] != "dispatching" {
            continue;
        }
        record["state"] = json!("uncertain");
        record["updated_at"] = json!(now());
        let Some(sid) = record["session_id"].as_str().map(str::to_owned) else {
            bail!("Invalid promotion session");
        };
        let Some(rid) = record["client_request_id"].as_str().map(str::to_owned) else {
            bail!("Invalid promotion request");
        };
        let event = json!({"client_request_id":rid,"state":"uncertain",
            "patch_sha256":record["patch_sha256"]});
        let docs = vec![(record_key(&sid, &rid), record)];
        host.record(None, docs, &sid, "promotion_recovered", &event)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    /// A one-shot broker that answers with `reply(request)`.
    fn broker(path: &Path, reply: fn(&Value) -> Value) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(path).expect("bind");
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("request");
            let request: Value = serde_json::from_str(&line).expect("json request");
            let answer = format!("{}\n", reply(&request));
            reader
                .get_mut()
                .write_all(answer.as_bytes())
                .await
                .expect("reply");
        })
    }

    #[tokio::test]
    async fn checks_broker_reply_identifier_and_digest() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("promote.sock");
        let server = broker(&path, |request| {
            json!({"id":request["id"],"status":"success",
                "patch_sha256":request["patch_sha256"],"paths":["src/main.rs"]})
        });
        let reply = apply(&path, "patch", CancellationToken::new())
            .await
            .expect("reply");
        assert_eq!(reply["status"], "success");
        server.await.expect("broker");
    }

    #[tokio::test]
    async fn rejects_wrong_broker_reply() {
        let root = tempfile::tempdir().expect("root");
        let path = root.path().join("promote.sock");
        let server = broker(&path, |_| json!({"id":"wrong","status":"success"}));
        assert!(apply(&path, "patch", CancellationToken::new())
            .await
            .is_err());
        server.await.expect("broker");
        assert!(apply(&path, "", CancellationToken::new()).await.is_err());
    }
}
