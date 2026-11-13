//! The Administrator tool handlers. The root has already checked that the run belongs to
//! the session and routed Source Control adapter calls elsewhere.
use crate::{trusted, workspace_host, AdministrationHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_administration::arguments;
use webagent_administration::tools::{authority_name, renews};
use webagent_host_core::{digest, now};

/// One Administrator tool call in a run.
pub struct Call<'a> {
    pub sid: &'a str,
    pub run_id: &'a str,
    /// The current run, already checked against the session and agent.
    pub run: &'a Value,
    pub agent: &'a str,
    pub name: &'a str,
    pub args: &'a Value,
    /// The pending tool call's ID; retries of one call reuse it.
    pub pending_id: &'a str,
    pub policy_version: u64,
}

/// Run one Administrator tool call.
pub async fn execute(
    host: &dyn AdministrationHost,
    call: Call<'_>,
    cancel: CancellationToken,
) -> Result<Value> {
    let Call {
        sid,
        run_id,
        run,
        agent: _,
        name,
        args,
        pending_id,
        policy_version: _,
    } = call;
    match name {
        "file_list" | "file_read" | "file_search" | "file_edit" | "file_write" => {
            if let Some(draft) = arguments::broker_file(sid, pending_id, name, args)? {
                let mut reply = broker_call(host, sid, run_id, name, &draft, cancel).await?;
                // A refused claim stays queued; say how to be woken when it is granted.
                if reply.to_string().contains("ownership_conflict") {
                    reply["next"] = json!(
                        "Your task is now queued for these files behind the \
                        blockers named here. Call build_work wait with your broker_id and end \
                        your turn; a background notice wakes you when the claim is granted."
                    );
                }
                Ok(reply)
            } else {
                // An argument mistake is refused before any effect, so it must not read as an
                // unknown write outcome.
                let live = match arguments::live_file(args) {
                    Ok(live) => live,
                    Err(error) => {
                        return Ok(json!({"status":"rejected","error_kind":"invalid_arguments",
                            "output":format!("Not run; nothing changed. {error}")}))
                    }
                };
                let trusted = host.checkout(sid, run, name, codebase_revision(run)).await;
                host.worker(name, &live, sid, trusted, cancel).await
            }
        }
        // The direct live-checkout writer never becomes a broker draft.
        "checkout_edit" => {
            let trusted = host.checkout(sid, run, name, codebase_revision(run)).await;
            host.worker(name, args, sid, trusted, cancel).await
        }
        "build_work" => {
            let mut request = args.clone();
            if request["request_id"].is_null() {
                request["request_id"] = json!(arguments::retry_id(sid, pending_id));
            }
            let request_id = request["request_id"].as_str().unwrap_or_default();
            match args["action"].as_str() {
                // A read of the job; the run then follows it like a finish (engine recovery).
                // A wait on a Broker ID follows the task's queued claim request instead.
                Some("wait") if args["job_id"].is_null() && args["broker_id"].is_string() => {
                    let task = args["broker_id"].as_str().unwrap_or_default();
                    let read = json!({"action":"reservations","task_id":task});
                    let reply = broker_call(host, sid, run_id, name, &read, cancel).await?;
                    Ok(claim_wait(task, reply))
                }
                Some("wait") => {
                    let job_id = job_argument(args)?;
                    let status = json!({"action":"status","job_id":job_id,"request_id":request_id});
                    broker_call(host, sid, run_id, name, &status, cancel).await
                }
                Some("cancel") => {
                    let job_id = job_argument(args)?;
                    stop_job(host, sid, run_id, job_id, request_id, cancel).await
                }
                _ => broker_call(host, sid, run_id, name, &request, cancel).await,
            }
        }
        "performance_work" => performance(host, sid, run_id, args, cancel).await,
        "repository_access" => repository_access(host, sid, args, cancel).await,
        "git_tool" | "resolve_conflict" | "commit_and_push" => {
            git(host, sid, name, args, cancel).await
        }
        _ => host.worker(name, args, sid, None, cancel).await,
    }
}

/// Live-checkout reads see the codebase revision the run was admitted with.
fn codebase_revision(run: &Value) -> Option<Value> {
    run["asset_revision"]
        .as_str()
        .map(|revision| json!({"codebase_revision":revision}))
}

fn renewal_key(sid: &str) -> String {
    format!("administrator_host:{sid}")
}

/// Pin a renewal record so the watcher keeps the job's lease while its authority holds.
async fn register_renewal(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    authority: &str,
    fingerprint: &str,
) -> Result<()> {
    let record = json!({"session_id":sid,"run_id":run_id,"tool_name":authority,
        "access_fingerprint":fingerprint});
    host.store().put(&renewal_key(sid), &record).await
}

/// A broker request admitted under the call's current authority.
async fn broker_call(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let action = args["action"].as_str();
    let authority = authority_name(name, action);
    let fingerprint = host.admit(sid, run_id, authority, args).await?;
    if renews(name, action) {
        register_renewal(host, sid, run_id, authority, &fingerprint).await?;
    }
    let mut forwarded = args.clone();
    if let Some(object) = forwarded.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    let mut identity = trusted(host.actor(), sid, &json!(fingerprint));
    // New work records the chat's title as its owning session on the Builds page.
    if action == Some("begin") {
        if let Some(title) = host
            .session_title(sid)
            .await?
            .filter(|t| !t.trim().is_empty())
        {
            identity["session_name"] = json!(title);
        }
    }
    host.broker(name, &forwarded, sid, Some(identity), cancel)
        .await
}

/// What a `wait` on a Broker ID returns: `waiting_claims` while the task has a queued
/// claim request, so the run follows it like a job; otherwise the task's state.
fn claim_wait(task: &str, reply: Value) -> Value {
    if reply["status"] != "success" {
        return reply;
    }
    let row = reply["output"]["tasks"].as_array().and_then(|rows| {
        rows.iter()
            .find(|row| row["label"] == task && row["owned_by_caller"] == true)
    });
    match row {
        Some(row) if row["requested"].as_array().is_some_and(|q| !q.is_empty()) => {
            json!({"status":"success","output":{"status":"waiting_claims","broker_id":task,
                "requested":row["requested"]}})
        }
        Some(row) => json!({"status":"success","output":{"status":row["state"],"broker_id":task,
            "scopes":row["scopes"],"message":"No queued claim request: nothing to wait for."}}),
        None => json!({"status":"error","error_kind":"invalid_arguments",
            "output":format!("No open task of this session is named {task}")}),
    }
}

/// The `job_id` a `wait` or `cancel` names: a broker Job ID, never a task ID.
fn job_argument(args: &Value) -> Result<&str> {
    args["job_id"]
        .as_str()
        .filter(|id| id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'))
        .ok_or_else(|| anyhow!("wait and cancel need the Broker Job ID as job_id"))
}

/// Stop one broker job this session owns, under `build_work` authority in run `run_id`.
/// The task stays, so it can be reopened and fixed; a job that already started deploying
/// is recorded by the broker as uncertain rather than stopped.
pub async fn stop_job(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    job_id: &str,
    request_id: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let args = json!({"action":"cancel","job_id":job_id});
    let fingerprint = host.admit(sid, run_id, "build_work", &args).await?;
    let identity = trusted(host.actor(), sid, &json!(fingerprint));
    // Unbatched jobs stop alone; the broker refuses if the job joined a batch meanwhile.
    let request = json!({"job_id":job_id,"request_id":request_id,"expected_job_ids":[job_id]});
    host.broker("v3_build_cancel", &request, sid, Some(identity), cancel)
        .await
}

/// Job states after which a task's job no longer runs.
const STOPPED: &[&str] = &[
    "completed",
    "failed",
    "stale",
    "cancelled",
    "uncertain",
    "interrupted",
];

/// Cancel a whole broker task from the chat: stop its running or queued job, then abandon
/// the task, discarding its unreleased edits and freeing its reservations. A job that
/// takes longer than about 40 s to stop is left `stopping` (repeat the request once it
/// has stopped); an uncertain job is never abandoned, since its effects need reconciling.
pub async fn cancel_task(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    task_id: &str,
    job_id: Option<&str>,
    request_id: &str,
) -> Result<Value> {
    if let Some(job_id) = job_id {
        let read = |job_id: &str| {
            let status = json!({"action":"status","job_id":job_id,"request_id":request_id});
            async move {
                let reply = broker_call(
                    host,
                    sid,
                    run_id,
                    "build_work",
                    &status,
                    CancellationToken::new(),
                )
                .await?;
                Ok::<_, anyhow::Error>(reply["output"]["status"].as_str().map(str::to_owned))
            }
        };
        let mut state = read(job_id).await?;
        if !state.as_deref().is_some_and(|s| STOPPED.contains(&s)) {
            let stop_id = format!("{request_id}-stop");
            let reply = stop_job(
                host,
                sid,
                run_id,
                job_id,
                &stop_id,
                CancellationToken::new(),
            )
            .await?;
            if reply["status"] != "success" {
                return Ok(reply);
            }
            for _ in 0..20 {
                state = read(job_id).await?;
                if state.as_deref().is_some_and(|s| STOPPED.contains(&s)) {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
        match state.as_deref() {
            Some("uncertain") => {
                return Ok(json!({"status":"success","output":{"state":"uncertain",
                    "job_id":job_id,"reason":"The job had started deploying; reconcile it before abandoning"}}));
            }
            Some(s) if STOPPED.contains(&s) => {}
            _ => {
                return Ok(json!({"status":"success","output":{"state":"stopping",
                    "job_id":job_id,"reason":"Stop requested; cancel the task again once it has stopped"}}));
            }
        }
    }
    let abandon = json!({"action":"abandon","task_id":task_id,"request_id":request_id});
    let reply = broker_call(
        host,
        sid,
        run_id,
        "build_work",
        &abandon,
        CancellationToken::new(),
    )
    .await?;
    if reply["status"] != "success" {
        return Ok(reply);
    }
    Ok(json!({"status":"success","output":{"state":"abandoned","task_id":task_id}}))
}

async fn git(
    host: &dyn AdministrationHost,
    sid: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let transport = host.repository(sid).await?;
    if !arguments::live_git_operation(name, args)? {
        return host.worker(name, args, sid, Some(transport), cancel).await;
    }
    if !host.administrator(sid).await? {
        bail!("Live checkout Git requires current administrator authority");
    }
    let mut request = args.clone();
    request["action"] = json!("git_tool");
    let repository = Some(json!({"repository":transport}));
    host.worker("administrator_file_git", &request, sid, repository, cancel)
        .await
}

/// Cancel or reconcile an existing performance request by its original identity. It uses
/// the admitted fingerprint rather than granting fresh authority after the caller has
/// cancelled or lost access.
async fn performance_cancel_existing(
    host: &dyn AdministrationHost,
    sid: &str,
    forwarded: &Value,
    fingerprint: &str,
    last: Value,
) -> Value {
    let identity = || trusted(host.actor(), sid, &json!(fingerprint));
    let mut args = forwarded.clone();
    args["action"] = json!("cancel");
    let cancel = host.broker(
        "performance_work",
        &args,
        sid,
        Some(identity()),
        CancellationToken::new(),
    );
    let _ = tokio::time::timeout(Duration::from_secs(3), cancel).await;
    args["action"] = json!("status");
    for _ in 0..3 {
        let status = host.broker(
            "performance_work",
            &args,
            sid,
            Some(identity()),
            CancellationToken::new(),
        );
        if let Ok(Ok(receipt)) = tokio::time::timeout(Duration::from_secs(2), status).await {
            if receipt["status"] == "success" && !arguments::performance_pending(&receipt) {
                return receipt;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    json!({"status":"uncertain","uncertain":true,
        "output":{"receipt":last,"cancel_requested":true,
            "reason":"Performance interruption requires inspection of the existing request; never re-admit it."}})
}

/// A performance experiment request. Effect actions wait in the foreground while the
/// caller's authority holds, and are cancelled by their original identity otherwise.
async fn performance(
    host: &dyn AdministrationHost,
    sid: &str,
    run_id: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if args.get("settings").is_some() || args.get("plugin_generation").is_some() {
        bail!("Effective performance settings are runtime owned")
    }
    let run = host.run(run_id).await?;
    let requested = match run
        .get("optimization_request")
        .and_then(|v| v.get("settings"))
    {
        Some(settings) => Some(settings.clone()),
        None => host
            .access(sid, &[])
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?
            .settings
            .get("optimization")
            .cloned(),
    };
    let mut forwarded = args.clone();
    forwarded["settings"] = host.optimization_settings(requested)?;
    let generation = host
        .optimization_generation()
        .ok_or_else(|| anyhow!("Optimization plugin unavailable"))?;
    forwarded["plugin_generation"] = json!(generation);
    let fingerprint = host.fingerprint(sid, run_id, "performance_work").await?;
    let wait = arguments::performance_effect_action(args);
    // Receipt reads and explicit cancellation cannot replace the renewal record
    // belonging to an independently executing performance job.
    if wait {
        register_renewal(host, sid, run_id, "performance_work", &fingerprint).await?;
    }
    let admitted = broker_call(
        host,
        sid,
        run_id,
        "performance_work",
        &forwarded,
        cancel.clone(),
    )
    .await;
    let mut receipt = match admitted {
        Ok(value) => value,
        Err(_) if wait => {
            // Submission may have reached the broker. Reconcile or cancel by the
            // original identity rather than repeat an admission with new IDs.
            tracing::warn!("Performance admission interrupted; existing receipt retained");
            let unknown = json!({"status":"unknown","reason":"Performance admission acknowledgement interrupted"});
            return Ok(
                performance_cancel_existing(host, sid, &forwarded, &fingerprint, unknown).await,
            );
        }
        Err(error) => return Err(error),
    };
    if !wait {
        return Ok(receipt);
    }
    let mut status_args = forwarded.clone();
    status_args["action"] = json!("status");
    while arguments::performance_pending(&receipt) {
        tokio::select! {
            () = cancel.cancelled() => {
                return Ok(performance_cancel_existing(host, sid, &forwarded, &fingerprint, receipt).await);
            }
            () = tokio::time::sleep(Duration::from_secs(1)) => {}
        }
        let fresh = host.fingerprint(sid, run_id, "performance_work").await;
        if !fresh.is_ok_and(|value| value == fingerprint) {
            return Ok(
                performance_cancel_existing(host, sid, &forwarded, &fingerprint, receipt).await,
            );
        }
        // Status uses the same request, pinned inputs and runtime settings. The broker
        // never schedules effects for a status request.
        let status = broker_call(
            host,
            sid,
            run_id,
            "performance_work",
            &status_args,
            cancel.clone(),
        );
        match tokio::time::timeout(Duration::from_secs(5), status).await {
            Ok(Ok(value)) if value["status"] == "success" => receipt = value,
            _ => {
                return Ok(performance_cancel_existing(
                    host,
                    sid,
                    &forwarded,
                    &fingerprint,
                    receipt,
                )
                .await)
            }
        }
    }
    Ok(receipt)
}

/// Read up to 64 KiB of a response body, or nothing if it is larger or fails.
async fn bounded_body(
    mut response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let chunk = tokio::select! {
            () = cancel.cancelled() => bail!("Repository metadata cancelled"),
            chunk = response.chunk() => chunk,
        };
        match chunk {
            Ok(Some(chunk)) if bytes.len() + chunk.len() <= 65536 => {
                bytes.extend_from_slice(&chunk)
            }
            Ok(None) => return Ok(bytes),
            _ => return Ok(Vec::new()),
        }
    }
}

/// GitHub's reported roles for one repository connection. Roles never establish token
/// write scope or branch protection permission.
async fn repository_metadata(
    client: &reqwest::Client,
    connection: &Value,
    url: &reqwest::Url,
    cancel: &CancellationToken,
) -> Result<Value> {
    let mut metadata = json!({"verified":false,"reason":"provider_metadata_unavailable",
        "reported_roles":Value::Null,"push_permission":"unverified"});
    let parts: Vec<&str> = url.path().trim_matches('/').split('/').collect();
    let [owner, name] = parts.as_slice() else {
        return Ok(metadata);
    };
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
    };
    if url.host_str() != Some("github.com") || !plain(owner) || !plain(name) {
        return Ok(metadata);
    }
    let repo = name.strip_suffix(".git").unwrap_or(name);
    let mut request = client
        .get(format!("https://api.github.com/repos/{owner}/{repo}"))
        .header("user-agent", "WebAgent-v3-repository-metadata")
        .header("accept", "application/vnd.github+json");
    if let Some(token) = connection["token"].as_str() {
        request = request.bearer_auth(token);
    }
    let response = tokio::select! {
        () = cancel.cancelled() => bail!("Repository metadata cancelled"),
        response = request.send() => response,
    };
    let Ok(response) = response else {
        return Ok(metadata);
    };
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > 65536)
    {
        metadata["reason"] = json!("metadata_too_large");
        return Ok(metadata);
    }
    let bytes = bounded_body(response, cancel).await?;
    if status != 200 {
        metadata["http_status"] = json!(status);
        metadata["reason"] = json!("metadata_read_unverified");
        return Ok(metadata);
    }
    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
        let permissions = &value["permissions"];
        let roles = json!({"pull":permissions["pull"].as_bool(),
            "push":permissions["push"].as_bool(),"admin":permissions["admin"].as_bool()});
        metadata = json!({"verified":true,"http_status":status,"reported_roles":roles,
            "push_permission":"unverified",
            "note":"Repository roles do not establish token write scope or branch protection permission"});
    }
    Ok(metadata)
}

/// The caller's repository connections, optionally checked against the provider. Never
/// mutates anything or returns credentials, and fails if grants change during the read.
async fn repository_access(
    host: &dyn AdministrationHost,
    sid: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let verify = arguments::repository_access(args)?;
    let transport = host.repository(sid).await?;
    let fingerprint = digest(&transport);
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let connections = transport["connections"]
        .as_array()
        .ok_or_else(|| anyhow!("Repository configuration invalid"))?;
    let mut rows = Vec::with_capacity(connections.len());
    for connection in connections {
        let raw = connection["url"].as_str().unwrap_or("");
        let url = reqwest::Url::parse(raw)?;
        let metadata = if verify {
            repository_metadata(&client, connection, &url, &cancel).await?
        } else {
            json!({"verified":false,"reason":"not_requested","reported_roles":Value::Null,
                "push_permission":"unverified"})
        };
        rows.push(json!({"connection_id":connection["id"],"url":raw,
            "credential_present":connection["token"].is_string(),
            "revision":connection["revision"],"metadata":metadata}));
    }
    if digest(&host.repository(sid).await?) != fingerprint {
        bail!("Repository grants or credential references changed during read");
    }
    Ok(
        json!({"status":"success","output":{"agent_id":transport["agent_id"],
        "revision":transport["revision"],"connections":rows,"mutations_performed":false,
        "credentials_exposed":false}}),
    )
}
