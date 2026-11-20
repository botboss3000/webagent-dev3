//! Storage limits for each account: the cleanup panel's actions (preview, then run), the
//! saved auto-cleanup policy, and the service that adds an app notification at the storage
//! policy's warning and critical shares, runs the saved policy when usage reaches the limit
//! and marks the account full so no new run starts until space is freed.
//!
//! Actions never touch pinned sessions or main chats. Deleting is permanent: an active
//! session is archived first, then deleted with its sub agents, as the Sessions page does.
use super::*;
use crate::api::uploads::{live_policy, owner_tier};
use crate::store::storage_limits::{POLICY_KEY, STATE_KEY};
use std::collections::{BTreeMap, BTreeSet, HashMap};

const DAY: u64 = 86_400;
const MAX_IDS: usize = 500;
const MAX_DAYS: u64 = 3_650;
/// Kinds a saved policy may hold; `sessions` (a hand-picked list) is panel-only.
const SAVED_KINDS: [&str; 4] = [
    "archived_sessions",
    "inactive_sessions",
    "session_files",
    "workspace_folders",
];
/// Shown as each new run is refused while the account is full.
pub(crate) const STORAGE_FULL: &str = "Storage is full. Free space on the Sessions page (storage meter) before starting new messages, automations or scheduled tasks.";
/// Usage is checked this often, or every minute once past the critical share.
const CHECK_SECONDS: u64 = 600;
const CRITICAL_CHECK_SECONDS: u64 = 60;
const MIB: u64 = 1024 * 1024;

/// One cleanup action as the panel or a saved policy states it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Action {
    kind: String,
    /// Sessions idle at least this many days; 0 means any age (archived sessions only).
    days: u64,
    ids: Vec<String>,
}

fn parse_actions(value: &Value, saved: bool) -> Result<Vec<Action>, ApiError> {
    let list = value
        .as_array()
        .filter(|list| !list.is_empty() && list.len() <= 8)
        .ok_or_else(|| bad("actions must list 1 to 8 cleanup actions"))?;
    let mut actions = Vec::new();
    let mut kinds = BTreeSet::new();
    for item in list {
        let fields = item
            .as_object()
            .ok_or_else(|| bad("Invalid cleanup action"))?;
        if fields
            .keys()
            .any(|key| !matches!(key.as_str(), "kind" | "days" | "ids"))
        {
            return Err(bad("Unknown cleanup action field"));
        }
        let kind = fields
            .get("kind")
            .and_then(Value::as_str)
            .filter(|kind| SAVED_KINDS.contains(kind) || (!saved && *kind == "sessions"))
            .ok_or_else(|| bad("Unknown cleanup action"))?;
        if !kinds.insert(kind) {
            return Err(bad("Each cleanup action may appear once"));
        }
        let days = match fields.get("days") {
            None => 0,
            Some(value) => value
                .as_u64()
                .filter(|days| *days <= MAX_DAYS)
                .ok_or_else(|| bad("days must be a whole number of days"))?,
        };
        if days == 0
            && matches!(
                kind,
                "inactive_sessions" | "session_files" | "workspace_folders"
            )
        {
            return Err(bad("Choose how many days a session must be unused"));
        }
        let ids = match fields.get("ids") {
            None => Vec::new(),
            Some(value) => value
                .as_array()
                .filter(|ids| kind == "sessions" && ids.len() <= MAX_IDS)
                .and_then(|ids| {
                    ids.iter()
                        .map(|id| id.as_str().map(str::to_owned))
                        .collect()
                })
                .ok_or_else(|| bad("ids lists sessions for the sessions action only"))?,
        };
        if kind == "sessions" && ids.is_empty() {
            return Err(bad("Pick the sessions to delete"));
        }
        actions.push(Action {
            kind: kind.to_owned(),
            days,
            ids,
        });
    }
    Ok(actions)
}

fn action_json(action: &Action) -> Value {
    let mut value = json!({"kind": action.kind});
    if action.days > 0 {
        value["days"] = json!(action.days);
    }
    if !action.ids.is_empty() {
        value["ids"] = json!(action.ids);
    }
    value
}

/// What the actions would do: sessions to delete, sessions whose files or folders go.
struct Plan {
    delete: BTreeMap<String, String>,
    files: BTreeMap<String, String>,
    folders: BTreeMap<String, String>,
}

/// A session any action may touch: not deleted, pinned or a main chat.
fn removable(session: &Value) -> bool {
    matches!(session["status"].as_str(), Some("active" | "archived"))
        && session["pinned"] != true
        && session["main_chat"] != true
        && session["id"].as_str().is_some()
}

fn idle_since(session: &Value) -> u64 {
    session["last_active_at"]
        .as_u64()
        .or_else(|| session["created_at"].as_u64())
        .unwrap_or(0)
}

async fn plan(app: &App, actions: &[Action], now: u64) -> Result<Plan, ApiError> {
    let sessions = app.store.list("session:").await?;
    let files = app.store.session_file_bytes().await?;
    let folders = app.store.folder_bytes().await?;
    let mut plan = Plan {
        delete: BTreeMap::new(),
        files: BTreeMap::new(),
        folders: BTreeMap::new(),
    };
    for session in sessions.iter().filter(|session| removable(session)) {
        let sid = session["id"].as_str().unwrap_or_default().to_owned();
        let title = session["title"].as_str().unwrap_or("").to_owned();
        for action in actions {
            let idle = idle_since(session) + action.days * DAY <= now;
            let chosen = match action.kind.as_str() {
                "archived_sessions" => session["status"] == "archived" && idle,
                "inactive_sessions" => idle,
                "sessions" => action.ids.contains(&sid),
                _ => false,
            };
            if chosen {
                plan.delete.insert(sid.clone(), title.clone());
            }
            if action.kind == "session_files" && idle && files.contains_key(&sid) {
                plan.files.insert(sid.clone(), title.clone());
            }
            if action.kind == "workspace_folders" && idle && folders.contains_key(&sid) {
                plan.folders.insert(sid.clone(), title.clone());
            }
        }
    }
    // A deleted session takes its files and folders with it.
    for sid in plan.delete.keys() {
        plan.files.remove(sid);
        plan.folders.remove(sid);
    }
    Ok(plan)
}

/// Archive (when active) then delete one session, as the Sessions page's lifecycle does.
async fn delete_session(app: &App, sid: &str) -> Result<(), ApiError> {
    use crate::store::session::SessionLifecycle;
    let read = |app: App| async move {
        app.store
            .get(&format!("session:{sid}"))
            .await?
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Session not found".into()))
    };
    let mut session = read(app.clone()).await?;
    if session["status"] == "active" {
        let body = SessionLifecycle {
            client_request_id: format!("storage-cleanup-archive-{}", uuid::Uuid::new_v4()),
            expected_revision: session["metadata_revision"].as_u64().unwrap_or(0),
        };
        crate::api::chat_sessions::archive_session_api(
            State(app.clone()),
            axum::extract::Path(sid.to_owned()),
            Json(body),
        )
        .await?;
        session = read(app.clone()).await?;
    }
    let body = SessionLifecycle {
        client_request_id: format!("storage-cleanup-delete-{}", uuid::Uuid::new_v4()),
        expected_revision: session["metadata_revision"].as_u64().unwrap_or(0),
    };
    crate::api::chat_sessions::delete_session_api(
        State(app.clone()),
        axum::extract::Path(sid.to_owned()),
        Json(body),
    )
    .await?;
    Ok(())
}

/// Carry out `plan`; returns what was removed. A failed session is reported, not retried.
async fn run_plan(app: &App, plan: &Plan) -> Value {
    let mut deleted = 0u64;
    let mut failed = Vec::new();
    for (sid, title) in &plan.delete {
        match delete_session(app, sid).await {
            Ok(()) => deleted += 1,
            Err(error) => failed.push(json!({"id":sid,"title":title,"error":error.1})),
        }
    }
    let (mut files, mut file_bytes) = (0u64, 0u64);
    for (sid, title) in &plan.files {
        match app.store.remove_session_files(sid).await {
            Ok((count, bytes)) => {
                files += count;
                file_bytes += bytes;
            }
            Err(_) => {
                failed.push(json!({"id":sid,"title":title,"error":"Files could not be removed"}))
            }
        }
    }
    for sid in plan.folders.keys() {
        crate::phase3_api::purge_browser_folder(app, sid).await;
    }
    json!({"sessions_deleted":deleted,"files_removed":files,"file_bytes_removed":file_bytes,
        "folders_cleared":plan.folders.len(),"failed":failed})
}

/// Usage, limit and percent for the signed-in account; no limit means `None`.
pub(crate) async fn usage(app: &App) -> Result<(u64, Option<u64>), ApiError> {
    let used = app.store.storage_used().await?;
    let limit = match owner_tier(app).await? {
        Some(tier) => live_policy()?.user_limit(&tier),
        None => None,
    };
    Ok((used, limit))
}

fn percent(used: u64, limit: u64) -> u64 {
    if limit == 0 {
        return 100;
    }
    (u128::from(used) * 100 / u128::from(limit)) as u64
}

fn mb(bytes: u64) -> u64 {
    bytes.div_ceil(MIB)
}

fn removed_summary(report: &Value) -> String {
    let mut parts = Vec::new();
    let count = |key: &str| report[key].as_u64().unwrap_or(0);
    if count("sessions_deleted") > 0 {
        parts.push(format!("{} sessions deleted", count("sessions_deleted")));
    }
    if count("files_removed") > 0 {
        parts.push(format!(
            "{} files removed ({} MB)",
            count("files_removed"),
            mb(count("file_bytes_removed"))
        ));
    }
    if count("folders_cleared") > 0 {
        parts.push(format!(
            "{} browser or Sandbox folders cleared",
            count("folders_cleared")
        ));
    }
    if parts.is_empty() {
        "nothing matched".into()
    } else {
        parts.join(", ")
    }
}

/// Measure the account against its limit, record the level reached, add a notification on
/// each new level, and, when usage newly reaches the limit, run the saved auto-cleanup.
/// The account is full while usage stays at or over the limit.
pub(crate) async fn evaluate(app: &App, automatic: bool) -> Result<Value, ApiError> {
    let policy = live_policy()?;
    let (mut used, limit) = usage(app).await?;
    let mut state = app.store.get(STATE_KEY).await?.unwrap_or_else(|| json!({}));
    let previous = state["level"].as_u64().unwrap_or(0);
    let was_full = state["full"] == true;
    let Some(limit) = limit else {
        let fresh = json!({"level":0,"full":false,"used_bytes":used,"checked_at":now()});
        if state["level"] != fresh["level"] || state["full"] != fresh["full"] {
            app.store.put(STATE_KEY, &fresh).await?;
        }
        return Ok(json!({"state":fresh,"cleanup":null}));
    };
    let warning = u64::from(policy.warning_percent);
    let critical = u64::from(policy.critical_percent).max(warning);
    let level_of = |used: u64| match percent(used, limit) {
        share if share >= 100 => 100,
        share if share >= critical => critical,
        share if share >= warning => warning,
        _ => 0,
    };
    let mut level = level_of(used);
    let mut cleanup = Value::Null;
    if level == 100 && previous < 100 && automatic {
        if let Some(saved) = app.store.get(POLICY_KEY).await? {
            if let Ok(actions) = parse_actions(&saved["actions"], true) {
                let plan = plan(app, &actions, now()).await?;
                let before = used;
                cleanup = run_plan(app, &plan).await;
                used = app.store.storage_used().await?;
                level = level_of(used);
                app.store
                    .add_notice(
                        "storage_cleanup",
                        "info",
                        &format!(
                            "Storage reached its {} MB limit, so your automatic cleanup ran: {}. Freed about {} MB; now {} MB used ({}%).",
                            mb(limit),
                            removed_summary(&cleanup),
                            mb(before.saturating_sub(used)),
                            mb(used),
                            percent(used, limit)
                        ),
                    )
                    .await?;
            }
        }
    }
    let full = used >= limit;
    if level > previous && level < 100 {
        app.store
            .add_notice(
                "storage_level",
                if level >= critical { "critical" } else { "warning" },
                &format!(
                    "Storage is {}% full ({} MB of {} MB). Free space from the storage meter on the Sessions page, or save an automatic cleanup there.",
                    percent(used, limit),
                    mb(used),
                    mb(limit)
                ),
            )
            .await?;
    }
    if full && !was_full {
        app.store
            .add_notice(
                "storage_full",
                "critical",
                &format!(
                    "Storage is full ({} MB of {} MB, {}%). Runs already going finish, but new messages, automations and scheduled tasks will not start until you free space from the storage meter on the Sessions page.",
                    mb(used),
                    mb(limit),
                    percent(used, limit)
                ),
            )
            .await?;
    }
    let fresh = json!({"level":level,"full":full,"used_bytes":used,"limit_bytes":limit,
        "percent":percent(used, limit),"checked_at":now()});
    // Write only on a change, so the record does not churn every check.
    if state["level"] != fresh["level"]
        || state["full"] != fresh["full"]
        || state["used_bytes"] != fresh["used_bytes"]
    {
        state = fresh;
        app.store.put(STATE_KEY, &state).await?;
    }
    Ok(json!({"state":state,"cleanup":cleanup}))
}

/// The cleanup panel: `{"actions":[...], "dry_run":true}` previews, without `dry_run` runs.
pub(crate) async fn cleanup(app: &App, body: &Value) -> Api {
    let actions = parse_actions(&body["actions"], false)?;
    let dry_run = match body.get("dry_run") {
        None => false,
        Some(Value::Bool(flag)) => *flag,
        Some(_) => return Err(bad("dry_run must be true or false")),
    };
    let plan = plan(app, &actions, now()).await?;
    let sizes = app.store.session_bytes().await?;
    let files = app.store.session_file_bytes().await?;
    let folders = app.store.folder_bytes().await?;
    let estimate = plan
        .delete
        .keys()
        .map(|sid| sizes.get(sid).copied().unwrap_or(0))
        .sum::<u64>()
        + plan
            .files
            .keys()
            .map(|sid| files.get(sid).map_or(0, |f| f.1))
            .sum::<u64>()
        + plan
            .folders
            .keys()
            .map(|sid| folders.get(sid).copied().unwrap_or(0))
            .sum::<u64>();
    let listed = |set: &BTreeMap<String, String>| {
        set.iter()
            .take(50)
            .map(|(id, title)| json!({"id":id,"title":title,"bytes":sizes.get(id).copied().unwrap_or(0)}))
            .collect::<Vec<_>>()
    };
    let preview = json!({"delete":plan.delete.len(),"files":plan.files.len(),"folders":plan.folders.len(),
        "estimated_bytes":estimate,"sessions":listed(&plan.delete),
        "file_sessions":listed(&plan.files),"folder_sessions":listed(&plan.folders)});
    if dry_run {
        return Ok(Json(json!({"preview":preview})));
    }
    let before = app.store.storage_used().await?;
    let report = run_plan(app, &plan).await;
    let state = evaluate(app, false).await?;
    Ok(Json(
        json!({"preview":preview,"report":report,"used_bytes_before":before,
        "used_bytes_after":state["state"]["used_bytes"],
        "summary":removed_summary(&report)}),
    ))
}

/// The largest sessions, for picking by hand in the panel.
pub(crate) async fn largest(app: &App) -> Api {
    let sizes = app.store.session_bytes().await?;
    let mut rows: Vec<Value> = app
        .store
        .list("session:")
        .await?
        .into_iter()
        .filter(|session| removable(session))
        .map(|session| {
            let sid = session["id"].as_str().unwrap_or_default();
            json!({"id":sid,"title":session["title"],"status":session["status"],
                "last_active_at":idle_since(&session),"bytes":sizes.get(sid).copied().unwrap_or(0)})
        })
        .collect();
    rows.sort_by_key(|row| std::cmp::Reverse(row["bytes"].as_u64().unwrap_or(0)));
    rows.truncate(20);
    Ok(Json(json!({"sessions":rows})))
}

/// The saved auto-cleanup policy (`{"actions":[...]}`), or `{"actions":null}` when none.
pub(crate) async fn policy_get(app: &App) -> Api {
    Ok(Json(
        app.store
            .get(POLICY_KEY)
            .await?
            .unwrap_or_else(|| json!({"actions":null})),
    ))
}

/// Save the panel's selection as the auto-cleanup policy; `{"actions":null}` turns it off.
pub(crate) async fn policy_put(app: &App, body: &Value) -> Api {
    if body["actions"].is_null() {
        app.store.delete(POLICY_KEY).await?;
        return Ok(Json(json!({"actions":null})));
    }
    let actions = parse_actions(&body["actions"], true)?;
    let saved =
        json!({"actions":actions.iter().map(action_json).collect::<Vec<_>>(),"saved_at":now()});
    app.store.put(POLICY_KEY, &saved).await?;
    Ok(Json(saved))
}

/// Check every account's usage: every ten minutes, or every minute once past the critical
/// share, and right after start.
pub(crate) async fn service(app: App) {
    let mut checked: HashMap<String, u64> = HashMap::new();
    loop {
        if let Ok(users) = app.store.all_user_ids().await {
            for user in users {
                if app.stop.is_cancelled() {
                    return;
                }
                let Ok(store) = app.store.for_user(&user).await else {
                    continue;
                };
                let critical = store
                    .get(STATE_KEY)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|state| state["level"].as_u64().unwrap_or(0) > 0);
                let due = if critical {
                    CRITICAL_CHECK_SECONDS
                } else {
                    CHECK_SECONDS
                };
                if checked
                    .get(&user)
                    .is_some_and(|at| now().saturating_sub(*at) < due)
                {
                    continue;
                }
                checked.insert(user.clone(), now());
                let mut scoped = app.clone();
                scoped.store = store;
                if evaluate(&scoped, true).await.is_err() {
                    tracing::warn!("Storage limit check deferred");
                }
            }
        }
        tokio::select! {_=app.stop.cancelled()=>return,_=tokio::time::sleep(std::time::Duration::from_secs(CRITICAL_CHECK_SECONDS))=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_validated() {
        assert!(parse_actions(&json!([{"kind":"archived_sessions"}]), true).is_ok());
        assert!(parse_actions(&json!([{"kind":"inactive_sessions","days":30}]), true).is_ok());
        assert!(parse_actions(&json!([{"kind":"inactive_sessions"}]), true).is_err());
        assert!(parse_actions(&json!([{"kind":"sessions","ids":["a"]}]), true).is_err());
        assert!(parse_actions(&json!([{"kind":"sessions","ids":["a"]}]), false).is_ok());
        assert!(parse_actions(&json!([{"kind":"sessions"}]), false).is_err());
        assert!(parse_actions(&json!([{"kind":"archived_sessions","x":1}]), true).is_err());
        assert!(parse_actions(
            &json!([{"kind":"archived_sessions"},{"kind":"archived_sessions"}]),
            true
        )
        .is_err());
        assert!(parse_actions(&json!([]), true).is_err());
    }

    #[tokio::test]
    async fn plan_skips_pinned_and_main_chats_and_counts_idle_days() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let now = 100 * DAY;
        for (id, status, pinned, main, active) in [
            ("old_archived", "archived", false, false, 1),
            ("new_archived", "archived", false, false, 99 * DAY),
            ("old_active", "active", false, false, 1),
            ("old_pinned", "archived", true, false, 1),
            ("old_main", "active", false, true, 1),
        ] {
            app.store
                .put(
                    &format!("session:{id}"),
                    &json!({"id":id,"status":status,"pinned":pinned,"created_at":0,
                    "main_chat":main,"last_active_at":active,"title":id}),
                )
                .await
                .unwrap();
        }
        let all = parse_actions(&json!([{"kind":"archived_sessions"}]), true).unwrap();
        let chosen = plan(&app, &all, now).await.unwrap();
        assert_eq!(
            chosen.delete.keys().collect::<Vec<_>>(),
            ["new_archived", "old_archived"]
        );
        let idle = parse_actions(&json!([{"kind":"inactive_sessions","days":30}]), true).unwrap();
        let chosen = plan(&app, &idle, now).await.unwrap();
        assert_eq!(
            chosen.delete.keys().collect::<Vec<_>>(),
            ["old_active", "old_archived"]
        );
    }
}
