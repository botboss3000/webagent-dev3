//! App Default routes' work: read an editable ability file from the checkout, keep one
//! administrator's drafts, and Save them. Drafts are checked and written to the live ability
//! files at once; a draft set no live file can hold would go through one broker task. The
//! router admits only administrators before these run. A successful release is committed
//! and closed by the broker itself; a failed one keeps the drafts and the task for repair.
use crate::{trusted, AdministrationHost};
use axum::http::StatusCode;
use serde_json::{json, Value};
use std::path::Path;
use tokio_util::sync::CancellationToken;
use webagent_administration::app_defaults::{
    checkout_file, draft_problem, drafts_file, saves_live, sha256, summary, task_label, Draft,
    DraftSet, Release, MAX_DRAFT_FILES, SESSION, SESSION_NAME,
};
use webagent_host_core::{bad, digest, now, ApiError};

/// One administrator's drafts change under this lock, so the revision fence cannot race.
/// It is this module's own: a Save holds it for seconds of broker calls.
static DRAFTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn conflict(message: impl Into<String>) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}

/// The broker identity of one administrator's Saves: their own user, the App Default
/// workspace and a fingerprint that renewals repeat.
fn identity(user: &str) -> Value {
    let mut value = trusted(
        user,
        SESSION,
        &json!(digest(&json!(["app_defaults", user]))),
        true,
    );
    value["session_name"] = json!(format!("{SESSION_NAME} ({user})"));
    value
}

async fn broker(host: &dyn AdministrationHost, name: &str, args: Value) -> Result<Value, ApiError> {
    let user = host.store().current_user_id().to_owned();
    let reply = host
        .broker(
            name,
            &args,
            SESSION,
            Some(identity(&user)),
            CancellationToken::new(),
        )
        .await
        .map_err(|error| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("Build broker unavailable: {error}"),
            )
        })?;
    if reply["status"] != "success" {
        let message = reply["output"]
            .as_str()
            .or_else(|| reply["error"].as_str())
            .unwrap_or("Build broker refused the request");
        return Err(conflict(message.chars().take(600).collect::<String>()));
    }
    Ok(reply["output"].clone())
}

/// One editable file as the checkout holds it now, with its revision.
pub async fn read(path: &str) -> Result<Value, ApiError> {
    let file = checkout_file(path).ok_or_else(|| bad("Not an App Default file"))?;
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let content = String::from_utf8(bytes.clone()).map_err(|_| bad("File is not text"))?;
            Ok(json!({"path":path,"exists":true,"content":content,"sha256":sha256(&bytes)}))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(json!({"path":path,"exists":false,"content":null,"sha256":null}))
        }
        Err(_) => Err(ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "File unavailable".into(),
        )),
    }
}

fn checkout_sha(path: &str) -> Option<String> {
    checkout_file(path)
        .and_then(|file| std::fs::read(file).ok())
        .map(|bytes| sha256(&bytes))
}

fn load(user: &str) -> DraftSet {
    let mut set = std::fs::read(drafts_file(user))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<DraftSet>(&bytes).ok())
        .unwrap_or_default();
    set.user = user.to_owned();
    set
}

fn store(set: &DraftSet) -> Result<(), ApiError> {
    let file = drafts_file(&set.user);
    let failed = |_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Drafts could not be saved".into(),
        )
    };
    if let Some(folder) = file.parent() {
        std::fs::create_dir_all(folder).map_err(failed)?;
    }
    // Replace atomically: a crash leaves the old drafts, never a half-written file.
    let temporary = file.with_extension("json.tmp");
    std::fs::write(
        &temporary,
        serde_json::to_vec(set).map_err(|_| bad("Drafts not serializable"))?,
    )
    .map_err(failed)?;
    std::fs::rename(&temporary, &file).map_err(failed)
}

/// Each other administrator's drafted paths, for the "another admin is drafting" badge.
fn others(user: &str) -> Vec<Value> {
    let Some(folder) = drafts_file(user).parent().map(Path::to_path_buf) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in std::fs::read_dir(folder).into_iter().flatten().flatten() {
        let Ok(set) =
            serde_json::from_slice::<DraftSet>(&std::fs::read(entry.path()).unwrap_or_default())
        else {
            continue;
        };
        if set.user != user && !set.files.is_empty() {
            rows.push(json!({"user":set.user,"paths":set.files.keys().collect::<Vec<_>>()}));
        }
    }
    rows
}

/// Follow a running Save: a completed release clears the drafts it carried (the broker
/// already committed and closed it); a failed one keeps them and records why.
async fn follow(host: &dyn AdministrationHost, set: &mut DraftSet) -> Result<bool, ApiError> {
    let Some(release) = set.release.clone().filter(|r| r.status == "releasing") else {
        return Ok(false);
    };
    let Some(job_id) = release.job_id.clone() else {
        return Ok(false);
    };
    let Ok(job) = broker(host, "v3_worker_job", json!({"job_id":job_id})).await else {
        return Ok(false);
    };
    match job["status"].as_str() {
        Some("completed") => {
            set.files.retain(|path, draft| {
                !release.paths.contains(path) || draft.updated_at > release.started_at
            });
            set.release = None;
        }
        Some("failed" | "cancelled" | "stale" | "uncertain" | "interrupted" | "needs_decision") => {
            let reason = job["reason"]
                .as_str()
                .map(|r| r.chars().take(400).collect());
            set.release = Some(Release {
                status: "failed".into(),
                reason,
                ..release
            });
        }
        _ => return Ok(false),
    }
    set.revision += 1;
    Ok(true)
}

/// The caller's drafts and Save state, plus what other administrators are drafting.
pub async fn drafts(host: &dyn AdministrationHost) -> Result<Value, ApiError> {
    let _held = DRAFTS.lock().await;
    let user = host.store().current_user_id().to_owned();
    let mut set = load(&user);
    if follow(host, &mut set).await? {
        store(&set)?;
    }
    // Each draft says whether the checkout moved since drafting began (Save then overwrites).
    let files: serde_json::Map<String, Value> = set
        .files
        .iter()
        .map(|(path, draft)| {
            let stale = checkout_sha(path) != draft.base_sha256;
            (path.clone(), json!({"draft":draft,"stale":stale}))
        })
        .collect();
    Ok(json!({"revision":set.revision,"files":files,"release":set.release,"others":others(&user)}))
}

/// Put, revert (`draft` None) or replace one file's draft, fenced by the set revision.
pub async fn put(
    host: &dyn AdministrationHost,
    path: &str,
    draft: Option<Draft>,
    expected_revision: u64,
) -> Result<Value, ApiError> {
    let _held = DRAFTS.lock().await;
    let user = host.store().current_user_id().to_owned();
    let mut set = load(&user);
    if set.revision != expected_revision {
        return Err(conflict(
            "Drafts changed since you opened them; reload and try again",
        ));
    }
    match draft {
        Some(mut draft) => {
            if let Some(problem) = draft_problem(path, &draft) {
                return Err(bad(&problem));
            }
            if !set.files.contains_key(path) && set.files.len() >= MAX_DRAFT_FILES {
                return Err(bad(&format!(
                    "Up to {MAX_DRAFT_FILES} drafted files; save or discard some first"
                )));
            }
            // The base is the checkout when drafting began; later edits keep it.
            draft.base_sha256 = set
                .files
                .get(path)
                .map_or_else(|| checkout_sha(path), |old| old.base_sha256.clone());
            draft.updated_at = now();
            set.files.insert(path.to_owned(), draft);
        }
        None => {
            set.files.remove(path);
        }
    }
    set.revision += 1;
    store(&set)?;
    Ok(json!({"revision":set.revision}))
}

/// Discard every draft (never a running Save's record).
pub async fn discard(
    host: &dyn AdministrationHost,
    expected_revision: u64,
) -> Result<Value, ApiError> {
    let _held = DRAFTS.lock().await;
    let user = host.store().current_user_id().to_owned();
    let mut set = load(&user);
    if set.revision != expected_revision {
        return Err(conflict(
            "Drafts changed since you opened them; reload and try again",
        ));
    }
    set.files.clear();
    if set.release.as_ref().is_some_and(|r| r.status == "failed") {
        set.release = None;
    }
    set.revision += 1;
    store(&set)?;
    Ok(json!({"revision":set.revision}))
}

/// Save every draft: live when every file is a live one, else as one broker task whose
/// release starts here. Locked while a release runs.
pub async fn save(
    host: &dyn AdministrationHost,
    note: &str,
    expected_revision: u64,
) -> Result<Value, ApiError> {
    let _held = DRAFTS.lock().await;
    let user = host.store().current_user_id().to_owned();
    let mut set = load(&user);
    follow(host, &mut set).await?;
    if set.revision != expected_revision {
        return Err(conflict(
            "Drafts changed since you opened them; reload and try again",
        ));
    }
    if set.releasing() {
        return Err(conflict("A Save is still releasing; it must finish first"));
    }
    if set.files.is_empty() {
        return Err(bad("Nothing to save"));
    }
    for (path, draft) in &set.files {
        if let Some(problem) = draft_problem(path, draft) {
            return Err(bad(&problem));
        }
    }
    let live = set.files.keys().all(|path| saves_live(path));
    if live {
        // Refuse before ending anything: a refused live Save changes nothing.
        check_live(host, &set).await?;
    }
    if let Some(failed) = set.release.clone().filter(|r| r.status == "failed") {
        end_failed(host, &failed.task_id).await?;
    }
    if live {
        return save_live(host, &mut set).await;
    }
    let paths: Vec<String> = set.files.keys().cloned().collect();
    let started = now();
    let task_id = task_label(&user, started);
    let mut release = Release {
        task_id: task_id.clone(),
        job_id: None,
        started_at: started,
        status: "releasing".into(),
        reason: None,
        paths: paths.clone(),
    };
    let result = start(host, &task_id, &summary(&paths, note, &user), &set).await;
    let failure = match result {
        Ok(job_id) => {
            release.job_id = Some(job_id);
            None
        }
        Err(error) => {
            // The task (if it began) stays for an agent to repair; the drafts stay too.
            release.status = "failed".into();
            release.reason = Some(error.1.chars().take(400).collect());
            Some(error)
        }
    };
    set.release = Some(release);
    set.revision += 1;
    store(&set)?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(json!({"revision":set.revision,"release":set.release}))
}

/// Each draft as a live edit: its new text, or `None` to delete.
fn live_edits(set: &DraftSet) -> Vec<(String, Option<String>)> {
    set.files
        .iter()
        .map(|(path, draft)| {
            (
                path.clone(),
                draft.content.clone().filter(|_| !draft.delete),
            )
        })
        .collect()
}

/// Refuse a live Save the release check would refuse, naming each problem.
async fn check_live(host: &dyn AdministrationHost, set: &DraftSet) -> Result<(), ApiError> {
    let problems = host
        .live_ability_problems(live_edits(set))
        .await
        .map_err(|error| {
            ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("The drafts could not be checked: {error:#}"),
            )
        })?;
    if problems.is_empty() {
        return Ok(());
    }
    let mut shown: Vec<String> = problems.iter().take(8).cloned().collect();
    if problems.len() > shown.len() {
        shown.push(format!("{} more", problems.len() - shown.len()));
    }
    let message: String = format!("Not saved, nothing changed: {}", shown.join("; "))
        .chars()
        .take(1500)
        .collect();
    Err(bad(&message))
}

/// Write each checked draft to the live ability files; agents see them from their next model
/// call and the broker commits them. A file another writer changed since the check stops the
/// Save there; the drafts already written are cleared, the rest stay.
async fn save_live(host: &dyn AdministrationHost, set: &mut DraftSet) -> Result<Value, ApiError> {
    let mut saved = Vec::new();
    let mut failure = None;
    for (path, body) in live_edits(set) {
        let current = checkout_file(&path).and_then(|file| std::fs::read(file).ok());
        if (body.is_none() && current.is_none())
            || body.as_deref().map(str::as_bytes) == current.as_deref()
        {
            saved.push(path);
            continue;
        }
        if let Err(error) = host.write_live_ability(&path, body, current).await {
            failure = Some(conflict(format!(
                "{path} was not saved ({error:#}); the drafts before it were saved"
            )));
            break;
        }
        saved.push(path);
    }
    set.files.retain(|path, _| !saved.contains(path));
    set.release = None;
    set.revision += 1;
    store(set)?;
    if let Some(error) = failure {
        return Err(error);
    }
    Ok(json!({"revision":set.revision,"release":null,"live":saved}))
}

/// End a failed Save's task so its reservations free before the next Save's task begins. The
/// broker refuses while that release may have changed anything live; a task the Diagnose chat
/// took over or that already ended is no longer this Save's to end.
async fn end_failed(host: &dyn AdministrationHost, task_id: &str) -> Result<(), ApiError> {
    let ended = broker(
        host,
        "build_work",
        json!({"action":"abandon","task_id":task_id,"reason":"replaced by a new App Default Save",
            "request_id":format!("{task_id}-abandon")}),
    )
    .await;
    match ended {
        Err(error) if !gone(&error.1) => Err(conflict(format!(
            "The failed Save's broker task could not be ended: {}",
            error.1
        ))),
        _ => Ok(()),
    }
}

/// Whether a broker refusal says this Save no longer has the task (it was taken over or ended).
fn gone(message: &str) -> bool {
    message.contains("Unknown Broker ID")
}

/// Begin the task, write each draft into it, check and finish: returns the job ID.
async fn start(
    host: &dyn AdministrationHost,
    task_id: &str,
    summary: &str,
    set: &DraftSet,
) -> Result<String, ApiError> {
    let request = |step: &str| format!("{task_id}-{step}");
    broker(
        host,
        "build_work",
        json!({"action":"begin","task_id":task_id,"summary":summary,"request_id":request("begin")}),
    )
    .await?;
    let status = broker(
        host,
        "v3_build_work",
        json!({"action":"status","task_id":task_id}),
    )
    .await?;
    let mut inputs = status["own"]
        .as_array()
        .and_then(|own| own.iter().find(|item| item["task_id"] == task_id))
        .and_then(|item| item["workspace_inputs_sha256"].as_str())
        .map(str::to_owned);
    for (index, (path, draft)) in set.files.iter().enumerate() {
        let exists = checkout_file(path).is_some_and(|file| file.exists());
        if draft.delete && !exists {
            continue;
        }
        let mut args =
            json!({"task_id":task_id,"path":path,"request_id":request(&format!("w{index}"))});
        if draft.delete {
            args["delete"] = json!(true);
        } else {
            args["content"] = json!(draft.content);
            if exists {
                args["mode"] = json!("replace");
            }
        }
        if let Some(digest) = &inputs {
            args["expected_inputs_sha256"] = json!(digest);
        }
        let written = broker(host, "file_write", args).await?;
        inputs = written["workspace_inputs_sha256"]
            .as_str()
            .map(str::to_owned)
            .or(inputs);
    }
    let inputs = inputs.ok_or_else(|| conflict("Broker task inputs unavailable"))?;
    let preflight = broker(
        host,
        "build_work",
        json!({"action":"preflight","task_id":task_id,"request_id":request("preflight")}),
    )
    .await?;
    if let Some(refusal) = preflight["release_plan"]["refusal"]
        .as_str()
        .filter(|r| !r.is_empty())
    {
        return Err(conflict(format!("Release refused: {refusal}")));
    }
    let finished = broker(host, "build_work", json!({"action":"finish","task_id":task_id,"expected_inputs_sha256":inputs,"request_id":request("finish")})).await?;
    finished["job_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| conflict("Release did not start"))
}

/// Keep a running Save's broker job authorized while its administrator still is one; the
/// broker caps app leases at seconds. The root runs this with the other renewals.
pub async fn renew(host: &dyn AdministrationHost) -> anyhow::Result<()> {
    let user = host.store().current_user_id().to_owned();
    let set = load(&user);
    if !set.releasing() || !host.account_administrator().await? {
        return Ok(());
    }
    let empty = json!({});
    let request = host.broker(
        "administrator_host_lease",
        &empty,
        SESSION,
        Some(identity(&user)),
        CancellationToken::new(),
    );
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), request).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::gone;

    #[test]
    fn a_task_already_ended_or_taken_over_is_gone() {
        assert!(gone("Unknown Broker ID in this caller session"));
        assert!(!gone("only an open task, or a closed task whose release failed without effects, may be abandoned"));
    }
}
