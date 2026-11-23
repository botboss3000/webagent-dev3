//! Host Admin: the administrator's side of the root host helper. The helper (a root service
//! outside the app) holds host-action requests until an administrator decides them here; it
//! trusts decisions only from the app's own uid, and this adapter only forwards signed-in,
//! CSRF-checked administrator decisions. Routes sit under the administrator router.
use axum::extract::Path;
use axum::{Extension, Json};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{bad, Api, App, State};

/// Largest helper reply read back.
const MAX_REPLY: usize = 2 * 1024 * 1024;

/// One request to the helper's peer-checked socket under the runtime root.
async fn helper(message: Value) -> Result<Value, crate::ApiError> {
    let value = call("host/helper.sock", "Host helper", &message).await?;
    if value["status"] != "success" {
        return Err(bad(value["output"]
            .as_str()
            .unwrap_or("Host helper refused the request")));
    }
    Ok(value["output"].clone())
}

/// One newline-delimited JSON exchange with a root host service's socket at `socket` under
/// the runtime root; returns the whole reply (`status`, `output`, optional `code`).
pub(crate) async fn call(
    socket: &str,
    service: &str,
    message: &Value,
) -> Result<Value, crate::ApiError> {
    let path = webagent_configuration::paths::runtime().join(socket);
    let unavailable = |_| bad(&format!("{service} unavailable"));
    let mut stream = UnixStream::connect(&path).await.map_err(unavailable)?;
    let mut line = serde_json::to_vec(message).map_err(|_| bad("Invalid host request"))?;
    line.push(b'\n');
    stream.write_all(&line).await.map_err(unavailable)?;
    let mut reply = Vec::new();
    let mut reader = BufReader::new(stream.take(MAX_REPLY as u64));
    reader
        .read_until(b'\n', &mut reply)
        .await
        .map_err(unavailable)?;
    serde_json::from_slice(&reply).map_err(|_| bad(&format!("{service} reply unreadable")))
}

/// File a File Manager grant request (`files_grant` or `terminal_grant`) for `admin`; the helper
/// keeps one waiting request per administrator and action.
pub(crate) async fn request_grant(action: &str, admin: &str) -> Result<Value, crate::ApiError> {
    helper(
        json!({"op": "request", "action": action, "args": {"admin": admin},
        "requester": {"user": admin}}),
    )
    .await
}

/// End one administrator's File Manager grant: `{"admin": "<name>", "kind": "files" | "terminal"}`
/// (files when omitted).
/// Narrowing access needs no passphrase.
pub(crate) async fn files_revoke(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let admin = body["admin"].as_str().unwrap_or_default();
    if admin.is_empty() || admin.len() > 80 {
        return Err(bad("Administrator required"));
    }
    let kind = body["kind"].as_str().unwrap_or("files");
    if !matches!(kind, "files" | "terminal") {
        return Err(bad("Kind must be files or terminal"));
    }
    let by = identity["_user_id"].as_str().unwrap_or_default();
    Ok(Json(
        helper(json!({"op": "files_revoke", "admin": admin, "by": by, "kind": kind})).await?,
    ))
}

/// Raise one administrator alert for each host request that starts waiting, so the
/// Notifications page (and later its push channels) shows it; polls the helper every 15 s.
pub(crate) async fn watch(app: App) {
    let mut seen = std::collections::HashSet::new();
    // Settled app requests already reported (or settled before this process started).
    let mut settled: Option<std::collections::HashSet<String>> = None;
    loop {
        if app.stop.is_cancelled() {
            break;
        }
        if let Ok(page) = helper(json!({"op": "list"})).await {
            let pending: std::collections::HashSet<String> = page["pending"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str().map(str::to_owned))
                .collect();
            let config = webagent_notifications::alerts::Config::live();
            for _ in pending.difference(&seen) {
                if let Some(alert) =
                    webagent_notifications::alerts::single(&config, "host_approval")
                {
                    if webagent_notifications::alerts::deliver(
                        &app.store.app_pool,
                        &alert,
                        crate::now() as i64,
                    )
                    .await
                    .is_err()
                    {
                        tracing::error!("Host approval alert not recorded");
                    }
                }
            }
            seen = pending;
            let finals: Vec<&Value> = page["recent"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["requester"]["channel"] == "app")
                .filter(|r| {
                    matches!(
                        r["state"].as_str(),
                        Some("done" | "failed" | "denied" | "expired")
                    )
                })
                .collect();
            let known = settled.get_or_insert_with(|| {
                finals
                    .iter()
                    .filter_map(|r| r["id"].as_str().map(str::to_owned))
                    .collect()
            });
            for request in finals {
                let id = request["id"].as_str().unwrap_or_default();
                if known.insert(id.to_owned()) && wake(&app, request).await.is_err() {
                    tracing::error!("Host request notice deferred");
                    known.remove(id);
                }
            }
        }
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
        }
    }
}

/// Deliver production release events (build-service/production.py: approved, denied, slow,
/// fallback, installers_failed, published, failed, interrupted) to the chat that requested the
/// release, as background notices like host-request outcomes; polls the broker every 15 s.
/// Events already recorded when this process starts count as delivered.
pub(crate) async fn production_watch(app: App) {
    let mut known: Option<std::collections::HashSet<String>> = None;
    loop {
        if app.stop.is_cancelled() {
            break;
        }
        if let Ok(page) = production_list(&app).await {
            let releases = page["releases"].as_array().cloned().unwrap_or_default();
            // Owned values: the loop awaits between notices, so nothing borrowed may cross it.
            let mut events: Vec<(String, Value, Value)> = Vec::new();
            for release in &releases {
                let id = release["id"].as_str().unwrap_or_default();
                for (n, event) in release["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                {
                    events.push((
                        format!("production:{id}:{n}"),
                        release.clone(),
                        event.clone(),
                    ));
                }
            }
            match known.as_mut() {
                None => known = Some(events.into_iter().map(|(key, _, _)| key).collect()),
                Some(seen) => {
                    for (key, release, event) in events {
                        if seen.contains(&key) {
                            continue;
                        }
                        match production_notice(&app, &key, &release, &event).await {
                            Ok(()) => {
                                seen.insert(key);
                            }
                            Err(_) => tracing::error!("Production release notice deferred"),
                        }
                    }
                }
            }
        }
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(std::time::Duration::from_secs(15)) => {}
        }
    }
}

/// The broker's production release list, read with the Builds page's administrator identity.
#[cfg(feature = "administrator")]
async fn production_list(app: &App) -> anyhow::Result<Value> {
    if !webagent_administration::tools::installed() {
        anyhow::bail!("Administrator pack not installed");
    }
    let mut caller = app.clone();
    caller.store = app.store.for_user(&watch_administrator(app).await?).await?;
    webagent_administration_host::dashboard::production(&crate::tools::administrator::Host(&caller))
        .await
        .map_err(|error| anyhow::anyhow!(error.1))
}

/// The account the watch reads as: an active administrator by role. The one used last time
/// (else the account named `admin`) is re-checked with one account read per tick; only when
/// it is no longer an active administrator are the other accounts read, in id order.
#[cfg(feature = "administrator")]
pub(crate) async fn watch_administrator(app: &App) -> anyhow::Result<String> {
    use webagent_accounts_host::AccountsStore;
    static CHOSEN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    let remembered = CHOSEN.lock().ok().and_then(|chosen| chosen.clone());
    let administrator = |id: String| async move {
        let account = app.store.account_by_id(&id).await?;
        anyhow::Ok(account.is_some_and(|account| {
            crate::access::Principal::from_account(&account).is_ok_and(|p| p.administrator())
        }))
    };
    let mut candidates: Vec<String> = remembered.into_iter().chain(["admin".to_owned()]).collect();
    for listed in [false, true] {
        if listed {
            candidates = app.store.all_user_ids().await?;
        }
        for id in candidates.drain(..) {
            if administrator(id.clone()).await? {
                if let Ok(mut chosen) = CHOSEN.lock() {
                    *chosen = Some(id.clone());
                }
                return Ok(id);
            }
        }
    }
    anyhow::bail!("No active administrator account")
}

/// Builds without the Administrator pack have no broker to read.
#[cfg(not(feature = "administrator"))]
async fn production_list(_app: &App) -> anyhow::Result<Value> {
    anyhow::bail!("Administrator pack not installed")
}

/// One production release event as a background notice to the requesting chat (none for
/// releases the operator requested).
async fn production_notice(
    app: &App,
    event_key: &str,
    release: &Value,
    event: &Value,
) -> anyhow::Result<()> {
    let (Some(user), Some(sid)) = (
        release["requester"]["user"].as_str(),
        release["requester"]["session"].as_str(),
    ) else {
        return Ok(());
    };
    let key = format!("background_notice:{sid}:{event_key}");
    let store = app.store.for_user(user).await?;
    if store.get(&key).await?.is_some() {
        return Ok(());
    }
    let kind = event["kind"].as_str().unwrap_or("event");
    let status = match kind {
        "published" => "done",
        "failed" | "interrupted" | "denied" => "failed",
        _ => "running",
    };
    store
        .put(
            &key,
            &json!({"source":"tool","title":format!("Production release v{} {kind}", release["version"].as_str().unwrap_or("?")),
            "status":status,"task_id":release["id"],"reply":event["text"],"key":key,"event_key":event_key,
            "session_id":sid,"created_at":crate::now()}),
        )
        .await?;
    crate::wake::notify(app).await;
    Ok(())
}

/// Wake the chat that filed `request` with a background notice of its outcome, the same
/// delivery that reports a finished build or background tool call.
async fn wake(app: &App, request: &Value) -> anyhow::Result<()> {
    let (Some(user), Some(sid), Some(id)) = (
        request["requester"]["user"].as_str(),
        request["requester"]["session"].as_str(),
        request["id"].as_str(),
    ) else {
        return Ok(());
    };
    let state = request["state"].as_str().unwrap_or("settled");
    let event = format!("host:{id}:{state}");
    let key = format!("background_notice:{sid}:{event}");
    let store = app.store.for_user(user).await?;
    if store.get(&key).await?.is_some() {
        return Ok(());
    }
    let reply = match state {
        "done" => format!("Result: {}", request["result"]),
        "failed" => format!("Error: {}", request["error"].as_str().unwrap_or("unknown")),
        "denied" => format!(
            "Denied by {}. Ask the user before trying again.",
            request["decided_by"].as_str().unwrap_or("an administrator")
        ),
        _ => "Expired without a decision. Ask the user before filing it again.".into(),
    };
    store
        .put(
            &key,
            &json!({"source":"tool","title":format!("host_admin {}", request["action"].as_str().unwrap_or("")),
            "status":state,"task_id":id,"reply":reply,"key":key,"event_key":event,
            "session_id":sid,"created_at":crate::now()}),
        )
        .await?;
    crate::wake::notify(app).await;
    Ok(())
}

/// Pending requests (with their exact effect and undo class) and the recent decided ones.
pub(crate) async fn requests(State(_app): State<App>) -> Api {
    Ok(Json(helper(json!({"op": "list"})).await?))
}

/// Approve or deny one request: `{"decision": "approve" | "deny"}`. The helper records the
/// deciding administrator and runs an approved action itself.
pub(crate) async fn decide(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let decision = body["decision"].as_str().unwrap_or_default();
    if !matches!(decision, "approve" | "deny") {
        return Err(bad("Decision must be approve or deny"));
    }
    let admin = identity["_user_id"].as_str().unwrap_or_default();
    let mut message = json!({"op": "decide", "id": id, "decision": decision, "admin": admin});
    // Root-equivalent, boot and disk approvals carry the host approval passphrase; only the
    // helper checks it.
    if let Some(passphrase) = body["passphrase"].as_str() {
        if passphrase.len() > 256 {
            return Err(bad("Passphrase too long"));
        }
        message["passphrase"] = json!(passphrase);
    }
    // File Manager grants last the time the approver sets; the helper checks the range.
    if let Some(seconds) = body["seconds"].as_i64() {
        message["seconds"] = json!(seconds);
    }
    Ok(Json(helper(message).await?))
}

/// Open (or end, with 0 seconds) an allow-all window for one requester: `{"requester":
/// "<session or ssh:identity>", "seconds": n, "passphrase": "..."}`. Opening needs the host
/// approval passphrase and a host whose `window_openers` lists the app; the helper checks
/// both, caps the window and never lets it cover boot, disk or irreversible actions.
pub(crate) async fn allow(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let requester = body["requester"].as_str().unwrap_or_default();
    let seconds = body["seconds"]
        .as_i64()
        .ok_or_else(|| bad("Seconds required"))?;
    if requester.is_empty() || requester.len() > 200 {
        return Err(bad("Requester required"));
    }
    let admin = identity["_user_id"].as_str().unwrap_or_default();
    let mut message =
        json!({"op": "allow", "session": requester, "seconds": seconds, "admin": admin});
    if let Some(passphrase) = body["passphrase"].as_str() {
        if passphrase.len() > 256 {
            return Err(bad("Passphrase too long"));
        }
        message["passphrase"] = json!(passphrase);
    }
    Ok(Json(helper(message).await?))
}
