//! The Administrator tool handlers. The root has already checked that the run belongs to
//! the session and routed Source Control and Code Index adapter calls elsewhere.
use crate::{promotion, trusted, workspace_host, AdministrationHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_accounts_host::AccountsStore;
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
        agent,
        name,
        args,
        pending_id,
        policy_version,
    } = call;
    match name {
        "file_list" | "file_read" | "file_search" | "file_edit" | "file_write" => {
            if let Some(draft) = arguments::broker_file(sid, pending_id, name, args)? {
                broker_call(host, sid, run_id, name, &draft, cancel).await
            } else {
                let live = arguments::live_file(args)?;
                host.worker(name, &live, sid, codebase_revision(run), cancel)
                    .await
            }
        }
        // The direct live-checkout writer never becomes a broker draft.
        "checkout_edit" => {
            host.worker(name, args, sid, codebase_revision(run), cancel)
                .await
        }
        "build_work" => {
            let mut request = args.clone();
            if request["request_id"].is_null() {
                request["request_id"] = json!(arguments::retry_id(sid, pending_id));
            }
            broker_call(host, sid, run_id, name, &request, cancel).await
        }
        "performance_work" => performance(host, sid, run_id, args, cancel).await,
        "repository_access" => repository_access(host, sid, args, cancel).await,
        "read_diagnostics" => Ok(match arguments::diagnostics_target(args, run_id) {
            Some(target) if target == run_id => diagnostics(host, Some(run), sid),
            Some(target) => {
                let selected = host.run(target).await.ok();
                diagnostics(host, selected.as_ref(), sid)
            }
            None => diagnostics(host, None, sid),
        }),
        "dev_browser_url" => {
            let path = args["path"]
                .as_str()
                .filter(|value| !value.is_empty())
                .map_or_else(|| format!("/app?session_id={sid}"), str::to_owned);
            if !arguments::valid_development_path(&path, sid, agent) {
                bail!("Development path unavailable");
            }
            let url = format!("{}{path}", arguments::public_origin());
            Ok(json!({"status":"success","output":{"url":url,"authentication_required":true}}))
        }
        "login_as_current_user"
        | "login_as_current_user_v3"
        | "dev_browser_action"
        | "dev_browser_record" => {
            let authority = browser_authority(host, sid).await?;
            host.worker(name, args, sid, Some(authority), cancel).await
        }
        "v3_workspace_receipt" => {
            let id = arguments::request_id(args)?;
            let receipt = host
                .store()
                .get(&receipt_key(sid, id))
                .await?
                .ok_or_else(|| anyhow!("Receipt not found in current session"))?;
            Ok(json!({"status":receipt["status"],
                "uncertain":receipt["result"]["uncertain"]==true,"output":receipt}))
        }
        "v3_workspace_action" => {
            let request = Receipted {
                sid,
                run_id,
                pending_id,
                policy_version,
            };
            receipt_action(host, request, args, cancel).await
        }
        "v3_build_status"
        | "v3_build_worker_restart"
        | "v3_build_image"
        | "v3_worker_submit"
        | "v3_worker_job"
        | "v3_build_work"
        | "v3_source_list"
        | "v3_source_read"
        | "v3_source_prepare"
        | "v3_source_apply"
        | "v3_source_apply_prepared"
        | "v3_source_receipt" => broker_call(host, sid, run_id, name, args, cancel).await,
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

fn receipt_key(sid: &str, id: &str) -> String {
    format!("administrator_receipt:{sid}:{id}")
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

fn diagnostics(host: &dyn AdministrationHost, selected: Option<&Value>, sid: &str) -> Value {
    match selected.filter(|run| run["session_id"].as_str() == Some(sid)) {
        Some(run) => json!({"status":"success","output":host.progress(run)}),
        None => json!({"status":"error","error_kind":"diagnostics_unavailable",
            "output":"Diagnostics unavailable in this session; omit run_id or use current"}),
    }
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
    let mut forwarded = if name == "v3_build_work" {
        arguments::build_work(args)?
    } else {
        args.clone()
    };
    if let Some(object) = forwarded.as_object_mut() {
        object.retain(|_, value| !value.is_null());
    }
    let identity = trusted(host.actor(), sid, &json!(fingerprint));
    host.broker(name, &forwarded, sid, Some(identity), cancel)
        .await
}

/// A short-lived browser session for the caller, reused while it stays valid.
async fn browser_authority(host: &dyn AdministrationHost, sid: &str) -> Result<Value> {
    let store = host.store();
    let uid = host.actor();
    let account = store
        .account_by_id(uid)
        .await?
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let key = format!("administrator_browser:{sid}");
    if let Some(old) = store.get(&key).await? {
        if old["user_id"] == uid
            && old["generation"] == account["generation"]
            && old["expires_at"].as_u64().unwrap_or(0) > now() + 30
        {
            return Ok(old);
        }
    }
    let generation = account["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Caller unavailable"))?;
    let session = store
        .accounts_repository()
        .issue_trusted_browser_session(
            uid,
            generation,
            now(),
            "V3 current-caller Administrator browser",
            600,
        )
        .await?;
    let agent = host.session_agent(sid).await?;
    let authority = json!({"session_id":sid,"user_id":uid,"cookie":session.token,
        "expires_at":session.expires,"generation":account["generation"],"agent_id":agent,
        "allowed_session_ids":[sid]});
    store.put(&key, &authority).await?;
    Ok(authority)
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

/// A `v3_workspace_action` call's identity: one receipt per session and request ID.
struct Receipted<'a> {
    sid: &'a str,
    run_id: &'a str,
    pending_id: &'a str,
    policy_version: u64,
}

/// Run a workspace mutation at most once per request ID. A repeated ID replays its
/// receipt; an interrupted one is reported uncertain and never re-run automatically.
async fn receipt_action(
    host: &dyn AdministrationHost,
    request: Receipted<'_>,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let Receipted {
        sid,
        run_id,
        pending_id,
        policy_version,
    } = request;
    let forwarded = arguments::workspace_action(args)?;
    let id = arguments::request_id(args)?;
    let key = receipt_key(sid, id);
    let fingerprint = digest(args);
    {
        let _guard = host.gate().lock().await;
        if let Some(old) = host.store().get(&key).await? {
            if old["request_sha256"] != fingerprint {
                bail!("request_id already used with different arguments");
            }
            let interrupted = old["status"] == "executing" || old["status"] == "uncertain";
            let status = if interrupted {
                "uncertain"
            } else {
                old["status"].as_str().unwrap_or("error")
            };
            return Ok(json!({"status":status,"uncertain":interrupted,"output":old,
                "replayed_receipt":true}));
        }
        let claim = json!({"request_id":id,"session_id":sid,"request_sha256":fingerprint,
            "status":"executing","created_at":now(),
            "note":"If interrupted, reconcile this receipt; never replay the mutation automatically"});
        host.store().put(&key, &claim).await?;
    }
    let action = args["action"].as_str().unwrap_or("");
    let result = if action == "propose" {
        let proposal = json!({"expected_review_sha256":args["expected_review_sha256"],
            "summary":args["summary"]});
        let request = promotion::Proposal {
            sid,
            request_id: pending_id,
            run_id,
            policy_version,
        };
        promotion::propose(host, request, &proposal, cancel).await
    } else if matches!(action, "check" | "test") {
        let timeout = args
            .get("timeout_seconds")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or(json!(600));
        let task = json!({"task":action,"expected_review_sha256":args["expected_review_sha256"],
            "timeout_seconds":timeout});
        workspace_host(host, sid, "v3_cargo_task", &task, cancel).await
    } else {
        workspace_host(host, sid, "v3_workspace_action", &forwarded, cancel).await
    };
    let value = result.unwrap_or_else(|_| {
        json!({"status":"uncertain","uncertain":true,
            "output":"Operation interrupted; inspect source and receipt before another request"})
    });
    let receipt = json!({"request_id":id,"session_id":sid,"request_sha256":fingerprint,
        "status":value["status"],"result":value,"finished_at":now()});
    host.store().put(&key, &receipt).await?;
    Ok(
        json!({"status":receipt["status"],"uncertain":receipt["result"]["uncertain"]==true,
        "output":receipt}),
    )
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
