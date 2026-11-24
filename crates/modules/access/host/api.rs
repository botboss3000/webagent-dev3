//! Administrator routes for people, tiers, and agent membership. When access
//! is reduced, active work is stopped or held for review so an old grant cannot
//! continue running in the separate loop process.
use crate::account_management;
use crate::State;
use crate::{
    access::{AccountStatus, Relationship, Tier},
    Api, ApiError, App,
};
use axum::{
    extract::{Path, Query},
    http::StatusCode,
    routing::get,
    Extension, Json, Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_accounts_host::AccountsStore;

fn invalid(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.into())
}
fn stale(message: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, message.into())
}
pub(crate) async fn administrator(app: &App, identity: &Value) -> Result<(), ApiError> {
    // Widgets and agent browsers never carry administrator power (AUTH-9, BROWSER-4).
    if crate::access::identity::CredentialKind::restricted_document(identity) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Administrator access required".into(),
        ));
    }
    let id = identity["_user_id"].as_str().unwrap_or("");
    let account = app
        .store
        .account_by_id(id)
        .await?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "Sign in required".into()))?;
    crate::access::require_administrator(&account).map_err(|_| {
        ApiError(
            StatusCode::FORBIDDEN,
            "Administrator access required".into(),
        )
    })?;
    Ok(())
}
async fn agent_operator(app: &App, identity: &Value, agent: &str) -> Result<(), ApiError> {
    if !app
        .store
        .agent_authority(identity["_user_id"].as_str().unwrap_or(""), agent)
        .await
        .is_ok_and(|rights| rights["can_manage"] == true)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
    }
    Ok(())
}
/// Stop `caller`'s active runs in `statuses` (for `agent_filter` only, when given). An
/// in-flight or uncertain write goes to recovery instead, so its effect can be reconciled.
/// `cause` names the change in the run's message, for example "this agent's tool permissions
/// changed"; a stop that only withdraws approvals says so instead of blaming access.
async fn stop_runs(
    caller: &App,
    agent_filter: Option<&str>,
    statuses: &[&str],
    cause: &str,
) -> Result<(), ApiError> {
    let approvals_only = statuses == ["waiting_approval"];
    for view in caller.store.active_runs().await? {
        let id = view["id"].as_str().unwrap_or("");
        let mut run = caller.run(id).await?;
        let agent_id = run["agent_id"].as_str().unwrap_or("shared_default");
        if agent_filter.is_some_and(|filter| filter != agent_id) {
            continue;
        }
        if !statuses.contains(&crate::status(&run)) {
            continue;
        }
        if let Some(token) = caller.active.lock().await.get(id) {
            token.cancel();
        }
        // Low-risk parallel writes settle as outcome unknown; only high-risk ones need review.
        if run["uncertain"] == true
            || (run["pending"]["state"] == "executing"
                && !crate::tools::is_read(run["pending"]["name"].as_str().unwrap_or(""))
                && run["pending"]["external_job"]["kind"] != "v3_build")
            || (crate::parallel_batch::executing_write(&run)
                && crate::parallel_batch::mark_uncertain_writes(&mut run))
        {
            run["status"] = json!("recovery");
            run["uncertain"] = json!(true);
            // Access revocation preserves reconciliation for every in-flight write.
            crate::parallel_batch::mark_uncertain_writes(&mut run);
            run["error"] = json!(format!(
                "Access changed ({cause}) during an uncertain tool effect; review required"
            ));
        } else if approvals_only {
            run["status"] = json!("stopped");
            run["error"] = json!(format!(
                "Approval withdrawn: {cause} while it was waiting; send a message to continue"
            ));
        } else {
            run["status"] = json!("stopped");
            run["error"] = json!(format!(
                "Access changed: {cause}; review permissions before resuming"
            ));
        }
        caller.save_override(&mut run).await?;
    }
    Ok(())
}

/// Stop runs waiting for an approval after a configuration change: the approval was bound to
/// the old settings. Running and queued work continues and reads the change at its next step.
pub(crate) async fn revoke_waiting_approvals(
    app: &App,
    agent_filter: Option<&str>,
    cause: &str,
) -> Result<(), ApiError> {
    for user_id in app.store.all_user_ids().await? {
        let mut caller = app.clone();
        caller.store = app.store.for_user(&user_id).await?;
        stop_runs(&caller, agent_filter, &["waiting_approval"], cause).await?;
    }
    Ok(())
}

/// Called while holding App.gate after an access mutation.
pub(crate) async fn stop_user_runs(
    app: &App,
    user_id: &str,
    agent_filter: Option<&str>,
    cause: &str,
) -> Result<(), ApiError> {
    let mut caller = app.clone();
    caller.store = app.store.for_user(user_id).await?;
    stop_runs(
        &caller,
        agent_filter,
        &["queued", "running", "waiting_approval", "waiting_external"],
        cause,
    )
    .await?;
    let mut closure_error = None;
    for session in caller.store.list("session:").await? {
        let agent = session["agent_id"].as_str().unwrap_or("shared_default");
        if agent_filter.is_some_and(|filter| filter != agent) {
            continue;
        }
        if let Some(sid) = session["id"]
            .as_str()
            .filter(|sid| uuid::Uuid::parse_str(sid).is_ok())
        {
            if let Err(error) = stop_session_runs(&caller, sid).await {
                closure_error = Some(error);
            }
        }
    }
    if let Some(error) = closure_error {
        return Err(error);
    }
    Ok(())
}

/// Close the caller-owned session's persistent resources after a durable restriction.
pub(crate) async fn stop_session_runs(app: &App, sid: &str) -> Result<(), ApiError> {
    stop_session_resources(app, sid, false).await
}

/// As `stop_session_runs`; with `keep_builds` (the user's session stop) an admitted broker
/// job keeps its build authority and runs to its end. The lease watcher still revokes it
/// if access, policy or mode change, and new build requests need fresh authority.
pub(crate) async fn stop_session_resources(
    app: &App,
    sid: &str,
    keep_builds: bool,
) -> Result<(), ApiError> {
    if uuid::Uuid::parse_str(sid).is_err() {
        return Err(invalid("Invalid session identifier"));
    }
    if app.store.get(&crate::key("session", sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let host = app.store.get(&format!("administrator_host:{sid}")).await?;
    let browser = app
        .store
        .get(&format!("administrator_browser:{sid}"))
        .await?;
    let mut failed = false;
    if let Some(job) = host.as_ref().filter(|_| !keep_builds) {
        let trusted = json!({"user_id":app.store.current_user_id(),"session_id":sid,
            "lease_until":crate::now()+30,"access_fingerprint":job["access_fingerprint"]});
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            crate::tools::execute_host_trusted(
                "administrator_host_revoke",
                &json!({}),
                sid,
                &crate::tools::administrator::build_socket(&app),
                Some(trusted),
                crate::CancellationToken::new(),
            ),
        )
        .await;
        failed |= !matches!(result,Ok(Ok(ref value)) if value["status"]=="success");
        if matches!(result,Ok(Ok(ref value)) if value["status"]=="success") {
            app.store
                .delete(&format!("administrator_host:{sid}"))
                .await?;
        }
    }
    if let Some(browser) = browser.as_ref() {
        if let Some(token) = browser["cookie"].as_str() {
            app.store.accounts_repository().logout(token).await?;
        }
        app.store
            .delete(&format!("administrator_browser:{sid}"))
            .await?;
    }
    // Missing worker plus no authorization has no runnable persistent resources.
    // A live socket or known authorization must acknowledge bounded shutdown.
    if app.tool_socket.exists() || host.is_some() || browser.is_some() {
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            crate::tools::execute_trusted(
                "run_stop",
                &json!({}),
                sid,
                &app.tool_socket,
                None,
                crate::CancellationToken::new(),
            ),
        )
        .await;
        failed |= !matches!(result,Ok(Ok(ref value)) if value["status"]=="success");
    }
    app.store
        .event(
            sid,
            if failed {
                "run_stop_failed"
            } else {
                "run_stopped"
            },
            &json!({"reason":"session_access_changed"}),
        )
        .await?;
    if failed {
        return Err(ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Session restriction saved; persistent resource shutdown could not be confirmed".into(),
        ));
    }
    Ok(())
}

/// Delete a deleted chat session's private browser folder and its workspace; best effort,
/// since the session is already gone. The startup sweep removes browser folders left
/// behind, not workspaces.
pub(crate) async fn purge_browser_folder(app: &App, sid: &str) {
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        crate::tools::execute_trusted(
            "administrator_browser_purge",
            &json!({}),
            sid,
            &app.tool_socket,
            None,
            crate::CancellationToken::new(),
        ),
    )
    .await;
    if !matches!(result, Ok(Ok(ref value)) if value["status"] == "success") {
        tracing::warn!("Browser folder purge deferred to the next startup sweep");
    }
}

/// At startup, delete browser folders and workspaces whose chat session no longer exists
/// in any user's store. Retries while the worker starts; skips the sweep if any store cannot be listed.
pub(crate) async fn sweep_browser_folders(app: App) {
    let mut keep = Vec::new();
    let Ok(users) = app.store.all_user_ids().await else {
        return;
    };
    for user in users {
        let Ok(store) = app.store.for_user(&user).await else {
            return;
        };
        let Ok(sessions) = store.list("session:").await else {
            return;
        };
        keep.extend(
            sessions
                .iter()
                .filter(|session| session["status"] != "deleted")
                .filter_map(|session| session["id"].as_str().map(str::to_owned)),
        );
    }
    for _ in 0..30 {
        let result = crate::tools::execute_trusted(
            "administrator_browser_sweep",
            &json!({"keep":keep}),
            "00000000-0000-0000-0000-000000000000",
            &app.tool_socket,
            None,
            crate::CancellationToken::new(),
        )
        .await;
        if let Some(value) = result.ok().filter(|value| value["status"] == "success") {
            tracing::info!(removed = %value["output"]["removed"],
                workspaces = %value["output"]["workspaces_removed"], "Browser folder and workspace sweep");
            return;
        }
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    }
}

pub(crate) async fn stop_agent_runs(
    app: &App,
    agent_id: &str,
    cause: &str,
) -> Result<(), ApiError> {
    let mut closure_error = None;
    for user_id in app.store.all_user_ids().await? {
        if let Err(error) = stop_user_runs(app, &user_id, Some(agent_id), cause).await {
            closure_error = Some(error);
        }
    }
    if let Some(error) = closure_error {
        return Err(error);
    }
    Ok(())
}
async fn stop_tier_runs(app: &App, tier_id: &str) -> Result<(), ApiError> {
    let cause = format!("account tier \"{tier_id}\" changed");
    let policy = app.store.read_access_policy()?;
    let order = crate::access::account_ladder::order(&policy)?;
    let index = order
        .iter()
        .position(|id| id == tier_id)
        .ok_or_else(|| invalid("Tier not found"))?;
    let mut error = None;
    for tier in &order[index..] {
        for user in app.store.access_repository().tier_user_ids(tier).await? {
            if let Err(failure) = stop_user_runs(app, &user, None, &cause).await {
                error = Some(failure);
            }
        }
    }
    if let Some(error) = error {
        return Err(error);
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccountChange {
    expected_revision: u64,
    status: AccountStatus,
    tier_id: String,
    /// When the tier ends and the account returns to Free (Unix seconds); absent: no end.
    #[serde(default)]
    tier_until: Option<u64>,
}
async fn change_account(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<AccountChange>,
) -> Api {
    administrator(&app, &identity).await?;
    crate::access::validate_self_administration(
        identity["_user_id"].as_str().unwrap_or(""),
        &id,
        body.status,
        &body.tier_id,
    )
    .map_err(|_| invalid("You cannot restrict or demote your own administrator account"))?;
    if let Some(until) = body.tier_until {
        crate::access::account_ladder::tier_end_allowed(
            &app.store.read_access_policy()?,
            &body.tier_id,
            until,
            crate::now(),
        )
        .map_err(|error| invalid(&error.to_string()))?;
    }
    let _guard = app.gate.lock().await;
    let before = app
        .store
        .account_by_id(&id)
        .await?
        .map(|account| crate::access::Principal::from_account(&account))
        .transpose()
        .map_err(|_| stale("Account update rejected; reload the account and tier"))?;
    let account = set_account_tier(
        &app,
        &id,
        body.expected_revision,
        body.status,
        &body.tier_id,
        body.tier_until,
    )
    .await
    .map_err(|_| stale("Account update rejected; reload the account and tier"))?;
    // A lower tier lets running work finish (new work follows its limits, checked fresh at
    // every model call and tool); only disabling the account or removing administrator
    // power stops it now.
    let removes_administrator = before.as_ref().is_none_or(|principal| {
        principal.tier_id == crate::access::ADMINISTRATOR_TIER
            && body.tier_id != crate::access::ADMINISTRATOR_TIER
    });
    if body.status != crate::access::AccountStatus::Active || removes_administrator {
        stop_user_runs(
            &app,
            &id,
            None,
            "the account's status, role or tier changed",
        )
        .await?;
    }
    #[cfg(all(unix, feature = "server-bins"))]
    crate::billing::tier_changed(&app, &id);
    Ok(Json(json!({"account":account})))
}
/// The `account_due` kind whose time is when an account's tier ends (its `tier_until`).
pub(crate) const TIER_END_DUE: &str = "tier_end";
/// The `account_due` kind whose time is when an account's paid grant ends.
pub(crate) const PAID_END_DUE: &str = "paid_end";
/// How often the API looks for accounts whose tier has ended.
const TIER_END_TICK: std::time::Duration = std::time::Duration::from_secs(60);
/// How long a tier end that could not be written waits before it is tried again (seconds).
const TIER_END_RETRY: u64 = 3600;
/// Change an account's status and tier through the account writer, with an optional end
/// date. The `account_due` row comes first and is only brought earlier, so a write that
/// fails or races leaves at worst an early visit that finds nothing to do, never a missed
/// end; after the write it holds the stored end exactly (none: removed).
async fn set_account_tier(
    app: &App,
    id: &str,
    expected_revision: u64,
    status: AccountStatus,
    tier_id: &str,
    tier_until: Option<u64>,
) -> anyhow::Result<Value> {
    let pool = &app.store.app_pool;
    if let Some(until) = tier_until {
        webagent_engine::due::earlier(pool, id, TIER_END_DUE, until).await?;
    }
    let account = app
        .store
        .update_account_tier(id, expected_revision, status, tier_id, tier_until)
        .await?;
    webagent_engine::due::set(pool, id, TIER_END_DUE, tier_until).await?;
    Ok(account)
}
/// Return accounts whose tier has ended to Free, every minute until the API stops. Each pass
/// reads only the `account_due` rows that are due, never every account.
pub(crate) async fn tier_end_service(app: App) {
    loop {
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(TIER_END_TICK) => {}
        }
        if let Err(error) = end_due_tiers(&app, crate::now()).await {
            tracing::warn!(%error, "Tier end pass failed");
        }
        if let Err(error) = deliver_owner_alerts(&app, crate::now()).await {
            tracing::warn!(%error, "Owner alert pass failed");
        }
        if let Err(error) = resume_limited_runs(&app, crate::now()).await {
            tracing::warn!(%error, "Limit resume pass failed");
        }
    }
}
/// One pass at `now`: each account whose tier-end or paid-end row is due is read again, and
/// what has ended goes through the account writer: an administrator's grant back to Free, a
/// paid grant removed (the account keeps the higher of what is left; its `auth_epoch` rises;
/// running work finishes, settings and data stay). Both rows are then set to the stored ends
/// exactly. Returns how many accounts changed.
pub(crate) async fn end_due_tiers(app: &App, now: u64) -> anyhow::Result<usize> {
    let pool = &app.store.app_pool;
    let mut due = webagent_engine::due::passed(pool, TIER_END_DUE, now).await?;
    due.extend(webagent_engine::due::passed(pool, PAID_END_DUE, now).await?);
    due.sort();
    due.dedup();
    let mut ended = 0;
    for id in due {
        let _guard = app.gate.lock().await;
        let (tier_end, paid_end) = match app.store.end_passed_grants(&id, now).await {
            Ok(changed) => {
                if changed.is_some() {
                    ended += 1;
                    #[cfg(all(unix, feature = "server-bins"))]
                    crate::billing::tier_changed(app, &id);
                }
                match app.store.account_by_id(&id).await? {
                    Some(account) => {
                        let sources = crate::access::repository::TierSources::of(&account)?;
                        (
                            sources.admin_until.filter(|until| *until > now),
                            sources
                                .paid
                                .map(|paid| paid.until)
                                .filter(|until| *until > now),
                        )
                    }
                    None => (None, None),
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Tier end not written; trying again later");
                let retry = Some(now.saturating_add(TIER_END_RETRY));
                (retry, retry)
            }
        };
        webagent_engine::due::set(pool, &id, TIER_END_DUE, tier_end).await?;
        webagent_engine::due::set(pool, &id, PAID_END_DUE, paid_end).await?;
    }
    Ok(ended)
}
/// Turn each due owner's waiting limit alerts (80 and 100 percent of their plan's runs at
/// once and daily command time, their agents' people limits, people reaching their level's
/// limits) into notices on their Notifications page, naming the agent when there is one.
pub(crate) async fn deliver_owner_alerts(app: &App, now: u64) -> anyhow::Result<usize> {
    let pool = &app.store.app_pool;
    let mut delivered = 0;
    for owner in webagent_engine::due::passed(pool, webagent_engine::due::OWNER_ALERT, now).await? {
        let notices = webagent_engine::usage_ledger::take_owner_alerts(pool, &owner).await?;
        if let Ok(store) = app.store.for_user(&owner).await {
            for (kind, level, limit) in notices {
                let (base, agent) = kind.split_once('@').unwrap_or((kind.as_str(), ""));
                let name = agent_name(app, agent).await;
                if let Some(text) =
                    webagent_engine::usage_ledger::alert_text(&kind, level, limit, name.as_deref())
                {
                    let severity = if level >= 100 { "warning" } else { "info" };
                    store
                        .add_notice(&format!("plan_{base}_{level}"), severity, &text)
                        .await?;
                    delivered += 1;
                }
            }
        }
        webagent_engine::due::set(pool, &owner, webagent_engine::due::OWNER_ALERT, None).await?;
    }
    Ok(delivered)
}
/// An agent's name for a notice (its plain column), or none when it is gone.
async fn agent_name(app: &App, agent: &str) -> Option<String> {
    if agent.is_empty() {
        return None;
    }
    let view = app.store.for_agent(agent).await.ok()?;
    sqlx::query_scalar("SELECT name FROM agents WHERE id=?")
        .bind(agent)
        .fetch_optional(&view.agent_pool)
        .await
        .ok()
        .flatten()
}
/// Resume each due account's runs that paused at a daily limit, now that the day has turned,
/// through the normal resume control (it checks the limits again). Only paused runs are read
/// (plain run index), and only those the account itself requested.
pub(crate) async fn resume_limited_runs(app: &App, now: u64) -> anyhow::Result<usize> {
    let pool = &app.store.app_pool;
    let mut resumed = 0;
    for user in webagent_engine::due::passed(pool, webagent_engine::due::LIMIT_RESUME, now).await? {
        let mut next = None;
        match app.store.for_user(&user).await {
            Ok(store) => {
                let paused: Vec<String> =
                    sqlx::query_scalar("SELECT id FROM run_index WHERE status='paused'")
                        .fetch_all(&store.pool)
                        .await
                        .unwrap_or_default();
                let mut scoped = app.clone();
                scoped.store = store;
                for id in paused {
                    let Ok(run) = scoped.run(&id).await else {
                        continue;
                    };
                    let limited = run["error"]
                        .as_str()
                        .is_some_and(|error| error.starts_with("Daily usage limit reached"));
                    if !limited || run["requester_user_id"].as_str().is_some_and(|r| r != user) {
                        continue;
                    }
                    let request =
                        format!("limit-resume-{}", webagent_engine::usage_ledger::day(now));
                    match crate::command(
                        State(scoped.clone()),
                        Path((id.clone(), "resume".to_owned())),
                        Json(json!({"client_request_id":request})),
                    )
                    .await
                    {
                        Ok(_) => resumed += 1,
                        Err(error) => {
                            tracing::warn!(run = %id, error = %error.1, "Limited run not resumed");
                            next = Some(now.saturating_add(TIER_END_RETRY));
                        }
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "Limited runs not read; trying again later");
                next = Some(now.saturating_add(TIER_END_RETRY));
            }
        }
        webagent_engine::due::set(pool, &user, webagent_engine::due::LIMIT_RESUME, next).await?;
    }
    Ok(resumed)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogChange {
    /// On, off, or null for the tier's default.
    allowed: Option<bool>,
}
/// An administrator sets an owner's "catalog models allowed" value (the billing service sets
/// it the same way, through the account writer).
async fn change_catalog_models(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<CatalogChange>,
) -> Api {
    administrator(&app, &identity).await?;
    let (account, _) = {
        let _guard = app.gate.lock().await;
        app.store
            .set_catalog_models(&id, body.allowed)
            .await
            .map_err(|error| invalid(&error.to_string()))?
    };
    #[cfg(all(unix, feature = "server-bins"))]
    crate::billing::tier_changed(&app, &id);
    Ok(Json(json!({"account":account})))
}
async fn tiers(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    administrator(&app, &identity).await?;
    let policy = app.store.read_access_policy()?;
    let mut tiers = Vec::new();
    for tier in app.store.entitlement_tiers().await? {
        let mut value = serde_json::to_value(&tier).map_err(anyhow::Error::from)?;
        value["name"] = policy["tier_names"][&tier.id]
            .as_str()
            .map(|v| json!(v))
            .unwrap_or_else(|| json!(tier.id));
        tiers.push(value);
    }
    Ok(Json(
        json!({"tiers":tiers,"order":crate::access::account_ladder::order(&policy)?,"revision":policy["revision"]}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TierChange {
    expected_revision: u64,
    tier: Tier,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TierOrder {
    expected_revision: u64,
    order: Vec<String>,
    names: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    confirm_expansion: bool,
}
async fn tier_order(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<TierOrder>,
) -> Api {
    administrator(&app, &identity).await?;
    let _guard = app.gate.lock().await;
    {
        let _lock = app.store.access_repository().policy_lock()?;
        let mut policy = app.store.read_access_policy()?;
        if policy["revision"].as_u64() != Some(body.expected_revision) {
            return Err(stale("Account levels changed"));
        }
        let before = crate::access::account_ladder::order(&policy)?;
        if before != body.order && !body.confirm_expansion {
            return Err(stale("Preview and confirm account-level reordering"));
        }
        if body.names.len() > 128
            || body.names.iter().any(|(id, name)| {
                policy["tiers"].get(id).is_none()
                    || name.trim().is_empty()
                    || name.len() > 100
                    || name.chars().any(char::is_control)
            })
        {
            return Err(invalid("Invalid account level names"));
        }
        policy["tier_order"] = json!(body.order);
        crate::access::account_ladder::order(&policy)
            .map_err(|_| invalid("Invalid account ladder"))?;
        policy["tier_names"] = json!(body.names);
        policy["revision"] = json!(body
            .expected_revision
            .checked_add(1)
            .ok_or_else(|| invalid("Policy revision overflow"))?);
        app.store.access_repository().write_access_policy(&policy)?;
    }
    for user in app.store.all_user_ids().await? {
        stop_user_runs(&app, &user, None, "the account tier order changed").await?;
    }
    Ok(Json(json!({"saved":true})))
}
async fn change_tier(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<TierChange>,
) -> Api {
    administrator(&app, &identity).await?;
    if id != body.tier.id {
        return Err(invalid("Tier ID does not match route"));
    }
    let _guard = app.gate.lock().await;
    let tier = app
        .store
        .save_entitlement_tier(body.tier, body.expected_revision)
        .await
        .map_err(|_| stale("Tier update rejected; reload the tier and grants"))?;
    stop_tier_runs(&app, &id).await?;
    Ok(Json(json!({"tier":tier})))
}
async fn membership(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((user_id, agent_id)): Path<(String, String)>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &agent_id).await?;
    let value = app
        .store
        .agent_membership(&user_id, &agent_id)
        .await
        .map_err(|_| invalid("Invalid membership identity"))?;
    Ok(Json(json!({"membership":value})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MembershipChange {
    expected_revision: u64,
    relationship: Relationship,
    #[serde(default)]
    agent_administrator: bool,
}
async fn change_membership(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((user_id, agent_id)): Path<(String, String)>,
    Json(body): Json<MembershipChange>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &agent_id).await?;
    let _guard = app.gate.lock().await;
    // LEGACY-MEMBERSHIP: installation oversight uses the same owner and revision checks as tenant APIs.
    let value = app
        .store
        .assign_agent_membership(
            identity["_user_id"].as_str().unwrap_or(""),
            &user_id,
            &agent_id,
            body.relationship,
            body.agent_administrator,
            body.expected_revision,
        )
        .await
        .map_err(|_| stale("Membership update rejected; reload the account and agent"))?;
    stop_user_runs(
        &app,
        &user_id,
        Some(&agent_id),
        "membership in this agent changed",
    )
    .await?;
    Ok(Json(json!({"membership":value})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolPermissionsChange {
    expected_revision: u64,
    tool_permissions: std::collections::BTreeMap<String, crate::access::Permission>,
}
async fn agent_tool_permissions(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let value = app
        .store
        .agent_tool_permissions(&id)
        .await
        .map_err(|_| invalid("Agent permissions unavailable"))?;
    Ok(Json(value))
}
async fn change_agent_tool_permissions(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<ToolPermissionsChange>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let _guard = app.gate.lock().await;
    let value = app
        .store
        .set_agent_tool_permissions(&id, body.expected_revision, body.tool_permissions)
        .await
        .map_err(|_| stale("Agent permissions changed; reload before saving"))?;
    // Configuration applies at the next step: each tool call is re-decided against live
    // permissions, so a revoked tool is refused like a disabled one and running work is not
    // stopped; only approvals waiting under the old settings are revoked.
    revoke_waiting_approvals(&app, Some(&id), "this agent's tool permissions changed").await?;
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolCallersChange {
    expected_revision: u64,
    tool_callers: std::collections::BTreeMap<String, Vec<Relationship>>,
}
async fn agent_tool_callers(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let value = app
        .store
        .agent_tool_callers(&id)
        .await
        .map_err(|_| invalid("Agent caller restrictions unavailable"))?;
    Ok(Json(value))
}
async fn change_agent_tool_callers(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<ToolCallersChange>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let _guard = app.gate.lock().await;
    let value = app
        .store
        .set_agent_tool_callers(&id, body.expected_revision, body.tool_callers)
        .await
        .map_err(|_| stale("Agent caller restrictions changed; reload before saving"))?;
    // Configuration applies at the next step: each tool call is re-decided against live
    // permissions, so a revoked tool is refused like a disabled one and running work is not
    // stopped; only approvals waiting under the old settings are revoked.
    revoke_waiting_approvals(&app, Some(&id), "this agent's tool caller rules changed").await?;
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AbilityPermissionsChange {
    expected_revision: u64,
    ability_permissions: std::collections::BTreeMap<String, crate::access::Permission>,
}
async fn agent_ability_permissions(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let value = app
        .store
        .agent_ability_permissions(&id)
        .await
        .map_err(|_| invalid("Agent ability permissions unavailable"))?;
    Ok(Json(value))
}
async fn change_agent_ability_permissions(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<AbilityPermissionsChange>,
) -> Api {
    administrator(&app, &identity).await?;
    agent_operator(&app, &identity, &id).await?;
    let _guard = app.gate.lock().await;
    let value = app
        .store
        .set_agent_ability_permissions(&id, body.expected_revision, body.ability_permissions)
        .await
        .map_err(|_| stale("Agent ability permissions changed; reload before saving"))?;
    // Configuration applies at the next step: each tool call is re-decided against live
    // permissions, so a revoked tool is refused like a disabled one and running work is not
    // stopped; only approvals waiting under the old settings are revoked.
    revoke_waiting_approvals(&app, Some(&id), "this agent's ability permissions changed").await?;
    Ok(Json(value))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditArgs {
    limit: Option<u32>,
}
async fn access_audit(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Query(args): Query<AuditArgs>,
) -> Api {
    administrator(&app, &identity).await?;
    let events = app
        .store
        .access_audit(args.limit.unwrap_or(50))
        .await
        .map_err(|_| invalid("Audit limit must be 1-200"))?;
    Ok(Json(json!({"events":events})))
}
async fn page_policy(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    administrator(&app, &identity).await?;
    let policy = app.store.read_access_policy()?;
    Ok(Json(
        json!({"pages":policy.get("pages").cloned().unwrap_or_else(||json!({})),"revision":policy["revision"].as_u64().unwrap_or(0)}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PagePolicyChange {
    expected_revision: u64,
    pages: Value,
}
async fn change_page_policy(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<PagePolicyChange>,
) -> Api {
    administrator(&app, &identity).await?;
    crate::registration::validate_pages(&body.pages)
        .map_err(|_| invalid("Invalid page audience policy"))?;
    let _guard = app.gate.lock().await;
    let policy = app
        .store
        .save_page_policy(body.pages, body.expected_revision)
        .map_err(|_| stale("Policy changed; reload before saving"))?;
    for id in app.store.all_user_ids().await? {
        stop_user_runs(&app, &id, None, "the page access policy changed").await?;
    }
    app.store
        .event(
            "",
            "page_policy_changed",
            &json!({"revision":policy["revision"]}),
        )
        .await?;
    Ok(Json(policy))
}
pub fn router() -> Router<App> {
    Router::new()
        .route("/page-policy", get(page_policy).put(change_page_policy))
        .route(
            "/accounts",
            get(account_management::accounts).post(account_management::create_account),
        )
        .route("/accounts/{id}", axum::routing::put(change_account))
        .route(
            "/accounts/{id}/catalog-models",
            axum::routing::put(change_catalog_models),
        )
        .route(
            "/accounts/{id}/profile",
            axum::routing::put(account_management::change_profile),
        )
        .route(
            "/accounts/{id}/sessions",
            get(account_management::account_sessions),
        )
        .route(
            "/accounts/{id}/sessions/{session_id}",
            axum::routing::delete(account_management::revoke_account_session),
        )
        .route("/tiers", get(tiers))
        .route("/tiers/order", axum::routing::put(tier_order))
        .route("/tiers/{id}", axum::routing::put(change_tier))
        .route(
            "/agents/{id}/tool-callers",
            get(agent_tool_callers).put(change_agent_tool_callers),
        )
        .route(
            "/memberships/{user_id}/{agent_id}",
            get(membership).put(change_membership),
        )
        .route(
            "/agents/{id}/tool-permissions",
            get(agent_tool_permissions).put(change_agent_tool_permissions),
        )
        .route(
            "/agents/{id}/ability-permissions",
            get(agent_ability_permissions).put(change_agent_ability_permissions),
        )
        .route("/audit", get(access_audit))
}
#[cfg(test)]
#[path = "plus_tier_tests.rs"]
pub(crate) mod plus_tier_tests;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_tests::fixture;

    #[tokio::test]
    async fn account_management_adapter_requires_current_administrator() {
        let (_root, app) = fixture().await;
        let admin = json!({"_user_id":"admin"});
        let Json(created) = account_management::create_account(
            State(app.clone()),
            Extension(admin.clone()),
            Json(account_management::NewAccount {
                username: "new_user".into(),
                password: "test-password-only".into(),
            }),
        )
        .await
        .unwrap();
        let id = created["account"]["user_id"].as_str().unwrap();
        let pending = json!({"_user_id":id});
        assert!(
            account_management::accounts(State(app.clone()), Extension(pending.clone()))
                .await
                .is_err()
        );
        let Json(updated) = change_account(
            State(app.clone()),
            Extension(admin.clone()),
            Path(id.into()),
            Json(AccountChange {
                expected_revision: 1,
                status: AccountStatus::Active,
                tier_id: "free".into(),
                tier_until: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(updated["account"]["status"], "active");
        assert!(
            account_management::accounts(State(app.clone()), Extension(pending.clone()))
                .await
                .is_err()
        );
        let Json(promoted) = change_account(
            State(app.clone()),
            Extension(admin),
            Path(id.into()),
            Json(AccountChange {
                expected_revision: 2,
                status: AccountStatus::Active,
                tier_id: "administrator".into(),
                tier_until: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(promoted["account"]["role"], "administrator");
        assert!(
            account_management::accounts(State(app.clone()), Extension(pending))
                .await
                .is_ok()
        );
    }
    #[tokio::test]
    async fn session_run_stop_withdraws_only_own_browser_and_requires_worker_ack() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (_root, app) = fixture().await;
        let sid = crate::uuid();
        let other = crate::uuid();
        let cookie = crate::uuid();
        let other_cookie = crate::uuid();
        for (id, token) in [(&sid, &cookie), (&other, &other_cookie)] {
            app.store
                .put(
                    &crate::key("session", id),
                    &json!({"id":id,"agent_id":"shared_default"}),
                )
                .await
                .unwrap();
            app.store
                .put(
                    &format!("administrator_browser:{id}"),
                    &json!({"cookie":token,"session_id":id}),
                )
                .await
                .unwrap();
            app.store
                .put(
                    &format!("auth:{}", crate::digest(&json!(token))),
                    &json!({"user_id":"admin"}),
                )
                .await
                .unwrap();
        }
        app.store
            .put(
                &format!("auth:{}", crate::digest(&json!(cookie))),
                &json!({"user_id":"admin"}),
            )
            .await
            .unwrap();
        let listener = tokio::net::UnixListener::bind(&app.tool_socket).unwrap();
        let expected = sid.clone();
        let worker = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["name"], "run_stop");
            assert_eq!(request["workspace"], expected);
            let response = format!(
                "{}\n",
                json!({"version":2,"id":request["id"],"result":{"status":"success"}})
            );
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        });
        stop_session_runs(&app, &sid).await.unwrap();
        worker.await.unwrap();
        assert!(app
            .store
            .get(&format!("administrator_browser:{sid}"))
            .await
            .unwrap()
            .is_none());
        assert!(app
            .store
            .get(&format!("auth:{}", crate::digest(&json!(cookie))))
            .await
            .unwrap()
            .is_none());
        assert!(app
            .store
            .get(&format!("administrator_browser:{other}"))
            .await
            .unwrap()
            .is_some());
        assert!(app
            .store
            .get(&format!("auth:{}", crate::digest(&json!(other_cookie))))
            .await
            .unwrap()
            .is_some());
    }
    #[tokio::test]
    async fn user_session_stop_keeps_a_running_build_authority() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let (_root, app) = fixture().await;
        let sid = crate::uuid();
        app.store
            .put(
                &crate::key("session", &sid),
                &json!({"id":sid,"agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        let record = json!({"session_id":sid,"run_id":"r","tool_name":"build_work",
            "access_fingerprint":"authority:build_work"});
        app.store
            .put(&format!("administrator_host:{sid}"), &record)
            .await
            .unwrap();
        let listener = tokio::net::UnixListener::bind(&app.tool_socket).unwrap();
        let worker = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["name"], "run_stop");
            let response = format!(
                "{}\n",
                json!({"version":2,"id":request["id"],"result":{"status":"success"}})
            );
            reader
                .get_mut()
                .write_all(response.as_bytes())
                .await
                .unwrap();
        });
        stop_session_resources(&app, &sid, true).await.unwrap();
        worker.await.unwrap();
        assert_eq!(
            app.store
                .get(&format!("administrator_host:{sid}"))
                .await
                .unwrap(),
            Some(record),
            "the broker job keeps its lease until it ends"
        );
    }
    #[tokio::test]
    async fn failed_shutdown_keeps_restriction_durable_and_withdraws_authentication() {
        let (_root, app) = fixture().await;
        let sid = crate::uuid();
        let cookie = crate::uuid();
        app.store
            .put(
                &crate::key("session", &sid),
                &json!({"id":sid,"agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        app.store
            .put(
                &format!("administrator_browser:{sid}"),
                &json!({"cookie":cookie}),
            )
            .await
            .unwrap();
        let auth = format!("auth:{}", crate::digest(&json!(cookie)));
        app.store
            .put(&auth, &json!({"user_id":"admin"}))
            .await
            .unwrap();
        let result = crate::api::session_mode_set(
            State(app.clone()),
            Path(sid.clone()),
            Json(crate::api::SessionModeBody {
                mode: "plan".into(),
                expected_revision: None,
                confirm_write: false,
            }),
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError(StatusCode::SERVICE_UNAVAILABLE, _))
        ));
        assert_eq!(
            crate::session_mode(&app, &sid).await.unwrap()["execution_mode"],
            "plan"
        );
        assert!(app.store.get(&auth).await.unwrap().is_none());
        assert!(app
            .store
            .get(&format!("administrator_browser:{sid}"))
            .await
            .unwrap()
            .is_none());
        // A caller retry can confirm no-worker shutdown without widening the saved mode.
        let _ = crate::api::session_mode_set(
            State(app.clone()),
            Path(sid.clone()),
            Json(crate::api::SessionModeBody {
                mode: "plan".into(),
                expected_revision: None,
                confirm_write: false,
            }),
        )
        .await
        .unwrap();
        assert!(stop_session_runs(&app, &crate::uuid()).await.is_err());
    }
}
