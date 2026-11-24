//! The agent owner's Dashboard page and the Plan panel's tier (plus tier spec 3.6, step 4).
//!
//! Read-only. One page open reads plain counters and index columns (`usage_agent`,
//! `usage_daily`, the run slot table, `agent_users`, `agent_directory`, `account_tiers`,
//! `owner_alerts`) and the agent's own settings; nothing is decrypted to count. There is no
//! timer: the page fetches on open and on its Refresh button.
//!
//! Who may call: `GET /agents/{id}/dashboard` the agent's owner and administrators;
//! `GET /plan` any signed-in person for their own account. Guests, widgets and agent
//! browsers are refused (the credential table refuses the last two before this code runs).
//! Both take the request's `App` extension, whose store the authentication middleware scoped
//! to the caller's own database (the router state's store is not the caller's).
use crate::{Api, ApiError, App};
use axum::{extract::Path, http::StatusCode, routing::get, Extension, Json, Router};
use serde_json::{json, Value};
use webagent_engine::usage_ledger as ledger;

/// The caller as a person with their own account: no guest, widget or agent browser.
fn person(identity: &Value) -> Result<String, ApiError> {
    let forbidden = || ApiError(StatusCode::FORBIDDEN, "Sign in to see your plan".into());
    if crate::access::identity::CredentialKind::restricted_document(identity)
        || identity["_role"] == "guest"
    {
        return Err(forbidden());
    }
    identity["_user_id"]
        .as_str()
        .filter(|user| !user.is_empty())
        .map(str::to_owned)
        .ok_or_else(forbidden)
}

/// The tier `user` is on (`tier`), its name, when it ends (`until`, Unix seconds; null while
/// it does not end) and where it comes from (`grant`, `paid`, `both` or `none`), from the
/// plain `account_tiers` index. Accounts on Free (or not yet indexed) have no end.
async fn plan_of(app: &App, user: &str, tier: &str) -> anyhow::Result<Value> {
    let policy = app.store.read_access_policy()?;
    let row: Option<(String, String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT tier_id,admin_tier,grant_until,paid_until FROM account_tiers WHERE user_id=?",
    )
    .bind(user)
    .fetch_optional(&app.store.app_pool)
    .await
    .unwrap_or(None);
    let now = i64::try_from(crate::now()).unwrap_or(i64::MAX);
    let (until, source) = match row.filter(|row| row.0 == tier) {
        Some((tier, admin_tier, grant_until, paid_until)) => {
            let paid = paid_until.filter(|until| *until > now);
            // The administrator's choice gives this tier: no end, or its end (a paid grant
            // running longer keeps the tier until then).
            let until = if admin_tier == tier {
                grant_until.map(|grant| grant.max(paid.unwrap_or(0)))
            } else {
                paid
            };
            (
                until,
                crate::access::repository::source_of(&admin_tier, paid.is_some()),
            )
        }
        None => (None, "none"),
    };
    Ok(
        json!({"tier":tier,"name":policy["tier_names"][tier].as_str().unwrap_or(tier),
        "until":until,"source":source}),
    )
}

/// The signed-in person's own tier for the Profile page's Plan panel.
async fn plan(Extension(app): Extension<App>, Extension(identity): Extension<Value>) -> Api {
    let user = person(&identity)?;
    let tier = identity["_tier_id"].as_str().unwrap_or("").to_owned();
    Ok(Json(plan_of(&app, &user, &tier).await?))
}

/// One limit as the page shows it: what is used now and the limit (null: no limit).
fn meter(used: impl Into<Value>, limit: Option<impl Into<Value>>) -> Value {
    json!({"used":used.into(),"limit":limit.map_or(Value::Null, Into::into)})
}

/// Unix time of the first second of the UTC month after `day`.
fn next_month_start(day: i64) -> i64 {
    let month = ledger::month_of_day(day);
    let mut next = day + 1;
    while ledger::month_of_day(next) == month {
        next += 1;
    }
    next * 86_400
}

/// The agent's Dashboard: the owner's shared limits across all their agents, this agent's
/// usage per model today and this month, its people against its limits, today's limit
/// notices and the owner's plan.
async fn dashboard(
    Extension(app): Extension<App>,
    Extension(identity): Extension<Value>,
    Path(agent): Path<String>,
) -> Api {
    let user = person(&identity)?;
    let not_found = || ApiError(StatusCode::NOT_FOUND, "Agent not found".into());
    if !crate::access::valid_agent_id(&agent) {
        return Err(not_found());
    }
    let pool = &app.store.app_pool;
    let owner: String = sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
        .bind(&agent)
        .fetch_optional(pool)
        .await
        .map_err(anyhow::Error::from)?
        .ok_or_else(not_found)?;
    let viewer_is_owner = owner == user;
    if !viewer_is_owner {
        crate::phase3_api::administrator(&app, &identity)
            .await
            .map_err(|_| {
                ApiError(
                    StatusCode::FORBIDDEN,
                    "Only the agent's owner sees its dashboard".into(),
                )
            })?;
    }
    // The owner's own view of the agent: their tier, limits and the agent's settings.
    let access = app.store.live_access(&owner, &agent).await?;
    let totals = access.owner_totals();
    let tier = access.owner_tier.as_ref();
    let now = crate::now();
    let day = ledger::day(now);
    let (owner_runs, at_once, per_level) = app.store.dashboard_counts(&agent, &owner).await?;
    let compute_ms = ledger::owner_compute_ms(pool, &owner, day).await?;
    let owned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agent_directory WHERE owner_id=?")
        .bind(&owner)
        .fetch_one(pool)
        .await
        .map_err(anyhow::Error::from)?;
    let storage_policy = webagent_files::storage_policy::StoragePolicy::live().ok();
    let tier_id = access.owner_tier_id.as_str();
    // Storage is the owner's own account; another viewer never opens it.
    let storage = if viewer_is_owner {
        let state = app
            .store
            .get(crate::store::storage_limits::STATE_KEY)
            .await?
            .unwrap_or(Value::Null);
        json!({"used":state["used_bytes"],"checked_at":state["checked_at"],
            "limit":storage_policy.as_ref().and_then(|policy| policy.user_limit(tier_id))})
    } else {
        Value::Null
    };
    let (today, month) = ledger::agent_usage(pool, &agent, day).await?;
    let view = app.store.for_agent(&agent).await?;
    let member_access = view.access_repository().member_access(&agent).await?;
    let levels = view.capability_levels(&agent).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM agents WHERE id=?")
        .bind(&agent)
        .fetch_optional(&view.agent_pool)
        .await
        .map_err(anyhow::Error::from)?;
    // The agent's people limits as `live_access` sets them for a member or visitor.
    let (users_limit, at_once_limit) = match tier {
        Some(tier) => (
            crate::access::run_limits::lower(
                tier.owner_limits.agent_users,
                member_access.max_users,
            ),
            crate::access::run_limits::lower(
                tier.owner_limits.agent_users_at_once,
                member_access.max_users_at_once,
            ),
        ),
        None => (Some(0), Some(0)),
    };
    let level_rows: Vec<Value> = levels
        .order
        .iter()
        .filter_map(|id| levels.levels.get(id))
        .map(|level| {
            let users = per_level
                .iter()
                .find(|(id, _)| id.as_deref() == Some(level.id.as_str()))
                .map_or(0, |(_, count)| *count);
            let runs = tier.map(|tier| {
                level
                    .runs_at_once
                    .map_or(tier.max_active_runs, |runs| runs.min(tier.max_active_runs))
            });
            json!({"id":level.id,"name":level.name,"users":users,
                "operator":level.id == "operator","runs_at_once":runs,
                "daily_turns":level.daily_turns,"monthly_turns":level.monthly_turns})
        })
        .collect();
    let users_30_days: i64 = per_level.iter().map(|(_, count)| count).sum();
    let mine = format!("@{agent}");
    let notices: Vec<Value> = ledger::current_owner_alerts(pool, &owner, day)
        .await?
        .into_iter()
        .filter(|(kind, _, _)| !kind.contains('@') || kind.ends_with(&mine))
        .filter_map(|(kind, level, limit)| {
            ledger::alert_text(&kind, level, limit, name.as_deref()).map(|text| {
                json!({"kind":kind.split('@').next(),"level":level,
                    "severity":if level >= 100 { "warning" } else { "info" },"message":text})
            })
        })
        .collect();
    Ok(Json(json!({
        "agent_id":agent,"name":name,"viewer_is_owner":viewer_is_owner,
        "plan":plan_of(&app, &owner, tier_id).await?,
        "shared":{
            "runs_at_once":meter(owner_runs, totals.runs_at_once),
            "compute_seconds_today":meter(compute_ms / 1000, totals.compute_seconds_per_day),
            "automations_per_person":{"limit":totals.automations},
            "storage_bytes":storage,
            "agents_owned":meter(owned, storage_policy.as_ref().and_then(|policy| policy.max_agents(tier_id))),
        },
        "usage":{"today":today,"month":month},
        "users":{
            "last_30_days":meter(users_30_days, users_limit),
            "at_once":meter(at_once, at_once_limit),
            "levels":level_rows,
        },
        "notices":notices,
        "resets":{"day":ledger::next_reset(now),"month":next_month_start(day)},
    })))
}

pub(crate) fn router() -> Router<App> {
    Router::new()
        .route("/agents/{id}/dashboard", get(dashboard))
        .route("/plan", get(plan))
}

#[cfg(test)]
#[path = "dashboard_tests.rs"]
mod dashboard_tests;
