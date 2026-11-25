//! Administrator-only metrics routes; the parent router owns authentication and authorization.
use super::*;
use axum::extract::Query;
use serde::Deserialize;
use webagent_system_host::{DashboardHost, Pending};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct Window {
    window: Option<String>,
}

/// The caller's `App` as the System dashboard host.
struct Dashboard(App);

impl DashboardHost for Dashboard {
    fn store(&self) -> &webagent_host_core::StoreCore {
        &self.0.store
    }
    fn session_page(
        &self,
        offset: usize,
    ) -> Pending<'_, anyhow::Result<(Vec<Value>, Option<usize>)>> {
        Box::pin(self.0.store.session_statistics_page(false, offset, 100))
    }
    fn admitted<'a>(&'a self, agent: &'a str) -> Pending<'a, bool> {
        Box::pin(async move {
            self.0
                .store
                .live_access(self.0.store.current_user_id(), agent)
                .await
                .is_ok_and(|a| a.admitted())
        })
    }
    fn active_runs(&self) -> Pending<'_, anyhow::Result<Vec<Value>>> {
        Box::pin(self.0.store.active_runs())
    }
    /// The broker reads private checkout metadata; the app never gains source filesystem permissions.
    fn git_status(&self) -> Pending<'_, Value> {
        Box::pin(async move {
            match super::build_dashboard::list(State(self.0.clone())).await {
                Ok(view) => view.0.get("source_git").cloned().unwrap_or_else(
                    || json!({"available":false,"reason":"source_status_bridge_unavailable"}),
                ),
                Err(_) => json!({"available":false,"reason":"source_status_bridge_unavailable"}),
            }
        })
    }
    fn runtime_config(&self) -> anyhow::Result<Value> {
        Ok(serde_json::to_value(loop_runtime::Config::read(
            &self.0.runtime_path,
        )?)?)
    }
}

/// Administrator middleware supplies the caller's existing Store; no caller-selected state paths.
async fn dashboard(State(app): State<App>, Query(args): Query<Window>) -> Api {
    let value = webagent_system_host::read(&Dashboard(app), args.window.as_deref()).await?;
    Ok(Json(value))
}

/// Fixed section names project the same authenticated snapshot and window contract.
async fn section(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(args): Query<Window>,
) -> Api {
    let value =
        webagent_system_host::section(&Dashboard(app), &name, args.window.as_deref()).await?;
    Ok(Json(value))
}

/// Collect bounded proc counters on the blocking pool instead of the HTTP executor.
async fn summary() -> Api {
    let sample = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(sample))
}

/// Select only the fixed counter groups; requests cannot supply filesystem paths.
async fn metric(Path(name): Path<String>) -> Api {
    if !["cpu", "memory", "disks", "network", "uptime"].contains(&name.as_str()) {
        return Err(bad("Unknown system metric"));
    }
    let sample = tokio::task::spawn_blocking(webagent_system::snapshot)
        .await
        .map_err(anyhow::Error::from)?;
    let value =
        webagent_system::select(sample, &name).ok_or_else(|| bad("System metrics unavailable"))?;
    Ok(Json(value))
}

/// Revision shadow counts (stream B4): per check kind, how often the revision answer agreed
/// with the full recomputation, and the newest disagreement's ids and parts. Content-free.
async fn revision_shadow(State(app): State<App>) -> Api {
    Ok(Json(
        webagent_engine::runtime::revision_shadow::report(&app.store.app_pool).await,
    ))
}

/// A loop row not renewed within this many seconds is stale (the loop renews every 2 s;
/// `webagent_engine::repository::MEMBER_FRESH_SECONDS` is the readiness limit).
const LOOP_FRESH_SECONDS: i64 = 10;

/// One loop process as stored: index, count it was started with, start and last-beat times,
/// whether past start-up recovery.
type LoopRow = (i64, i64, i64, i64, bool);

/// The Dashboard and Builds pages' loop view: for each loop `0..n` its state, heartbeat age,
/// runs in progress and accounts assigned. Pure: `now` and every input come from the caller.
fn loop_view(rows: &[LoopRow], running: &[(i64, i64)], accounts: &[String], now: i64) -> Value {
    // The count the freshest row was started with; rows of an older count are stale leftovers.
    let loops = rows.iter().max_by_key(|row| row.3).map_or(0, |row| row.1);
    let count = u32::try_from(loops).unwrap_or(0);
    let mut assigned = vec![0u64; count as usize];
    for account in accounts {
        if let Some(slot) = assigned.get_mut(webagent_engine::partition::loop_for(account, count) as usize) {
            *slot += 1;
        }
    }
    let entries: Vec<Value> = (0..loops)
        .map(|index| {
            let row = rows.iter().find(|row| row.0 == index && row.1 == loops);
            let age = row.map(|row| (now - row.3).max(0));
            let state = match (row, age) {
                (Some(row), Some(age)) if age <= LOOP_FRESH_SECONDS => {
                    if row.4 { "serving" } else { "starting" }
                }
                (Some(_), _) => "stale",
                _ => "missing",
            };
            let runs = running.iter().find(|r| r.0 == index).map_or(0, |r| r.1);
            json!({
                "index": index,
                "state": state,
                "heartbeat_age_seconds": age,
                "started_at": row.map(|row| row.2),
                "runs_in_progress": runs,
                "accounts": assigned.get(index as usize).copied().unwrap_or(0),
            })
        })
        .collect();
    json!({
        "schema_version": 1,
        "loops": loops,
        "accounts_total": accounts.len(),
        "runs_in_progress": running.iter().map(|r| r.1).sum::<i64>(),
        "members": entries,
    })
}

/// Agent loops and each loop's state (administrator only; read-only; no run or account ids).
async fn loops(State(app): State<App>) -> Api {
    let rows: Vec<LoopRow> = sqlx::query_as(
        "SELECT idx,n,started_at,updated_at,serving FROM loop_members ORDER BY idx",
    )
    .fetch_all(&app.store.app_pool)
    .await
    .map_err(anyhow::Error::from)?;
    let running: Vec<(i64, i64)> =
        sqlx::query_as("SELECT idx,count(*) FROM running_runs GROUP BY idx")
            .fetch_all(&app.store.app_pool)
            .await
            .map_err(anyhow::Error::from)?;
    let accounts = app.store.all_user_ids().await?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    Ok(Json(loop_view(&rows, &running, &accounts, now)))
}

/// Mount beneath the existing administrator authentication boundary.
pub(crate) fn router() -> Router<App> {
    Router::new()
        .route("/loops", get(loops))
        .route("/summary", get(summary))
        .route("/revision-shadow", get(revision_shadow))
        .route("/dashboard", get(dashboard))
        .route("/dashboard/{section}", get(section))
        .route("/{metric}", get(metric))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    #[test]
    fn loop_view_reports_each_loop_with_its_accounts_and_runs() {
        let accounts: Vec<String> = ["admin", "a", "b", "c", "d", "e"].map(String::from).to_vec();
        // Loop 0 beat 1 s ago and is serving; loop 1 beat 30 s ago.
        let rows = [(0, 2, 100, 999, true), (1, 2, 100, 970, true)];
        let view = loop_view(&rows, &[(0, 3), (1, 1)], &accounts, 1000);
        assert_eq!(view["loops"], 2);
        assert_eq!(view["accounts_total"], 6);
        assert_eq!(view["runs_in_progress"], 4);
        let members = view["members"].as_array().unwrap();
        assert_eq!(members[0]["state"], "serving");
        assert_eq!(members[0]["heartbeat_age_seconds"], 1);
        assert_eq!(members[0]["runs_in_progress"], 3);
        assert_eq!(members[1]["state"], "stale");
        let total: u64 = members.iter().map(|m| m["accounts"].as_u64().unwrap()).sum();
        assert_eq!(total, 6);
        // The administrator is always loop 0.
        assert!(members[0]["accounts"].as_u64().unwrap() >= 1);
    }

    #[test]
    fn loop_view_marks_a_missing_loop_and_a_starting_one() {
        let rows = [(1, 3, 100, 1000, false), (2, 3, 100, 1000, true)];
        let view = loop_view(&rows, &[], &[], 1000);
        let states: Vec<&str> = view["members"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["state"].as_str().unwrap())
            .collect();
        assert_eq!(states, ["missing", "starting", "serving"]);
        assert_eq!(loop_view(&[], &[], &[], 0)["loops"], 0);
    }

    #[tokio::test]
    async fn system_routes_require_admin_and_return_each_counter_group() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let token = crate::uuid();
        let key = format!("auth:{}", crate::digest(&json!(token)));
        app.store
            .put(
                &key,
                &json!({"csrf":"fixture","expires":crate::now()+3600,
            "generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let request = |path: &str, token: Option<&str>, method: &str| {
            let mut builder = Request::builder()
                .uri(format!("/api/v3/admin/system/{path}"))
                .method(method);
            if let Some(token) = token {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        for name in ["summary", "cpu", "memory", "disks", "network", "uptime"] {
            let denied = crate::router(app.clone())
                .oneshot(request(name, None, "GET"))
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
            let response = crate::router(app.clone())
                .oneshot(request(name, Some(&token), "GET"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "{name}");
            let body = axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["schema_version"], 1);
            assert_eq!(value["scope"]["cgroup_limits_included"], false);
            assert_eq!(
                value["metrics"].as_object().unwrap().len(),
                if name == "summary" { 5 } else { 1 }
            );
            if name != "summary" {
                assert!(value["metrics"][name].is_object());
            }
        }
        for name in [
            "dashboard",
            "dashboard/history",
            "dashboard/database",
            "dashboard/devices",
            "dashboard/git",
            "dashboard/loop",
            "dashboard/throughput",
            "dashboard/runtime",
            "dashboard/usage",
        ] {
            let denied = crate::router(app.clone())
                .oneshot(request(name, None, "GET"))
                .await
                .unwrap();
            assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);
            let allowed = crate::router(app.clone())
                .oneshot(request(name, Some(&token), "GET"))
                .await
                .unwrap();
            assert_eq!(allowed.status(), StatusCode::OK, "{name}");
            let body = axum::body::to_bytes(allowed.into_body(), 1048576)
                .await
                .unwrap();
            let value: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(value["schema_version"], 1);
        }
        let invalid = crate::router(app.clone())
            .oneshot(request("dashboard?window=invalid", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let unknown = crate::router(app.clone())
            .oneshot(request("processes", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
        let post = crate::router(app.clone())
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/system/summary")
                    .method("POST")
                    .header("cookie", format!("v3_session={token}"))
                    .header("x-csrf-token", "fixture")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post.status(), StatusCode::METHOD_NOT_ALLOWED);
        let member = app
            .store
            .create_user_account("system-metrics-member", "fixture-only-password-hash")
            .await
            .unwrap();
        let user_id = member["user_id"].as_str().unwrap();
        app.store
            .update_account_access(user_id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let account = app.store.account_by_id(user_id).await.unwrap().unwrap();
        app.store
            .put(
                &key,
                &json!({"csrf":"fixture","expires":crate::now()+3600,
            "generation":account["generation"],"user_id":user_id}),
            )
            .await
            .unwrap();
        let denied = crate::router(app.clone())
            .oneshot(request("summary", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
        let denied = crate::router(app)
            .oneshot(request("dashboard", Some(&token), "GET"))
            .await
            .unwrap();
        assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    }
}
