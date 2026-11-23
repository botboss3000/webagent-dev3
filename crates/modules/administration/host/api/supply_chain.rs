//! The dependency (supply-chain) scan for administrators: the Dashboard card's route and the
//! scan watch that raises administrator alerts. Report-only: nothing here updates anything.
//!
//! The build broker reads the host scanner's summary file with its own guards
//! (`v3_supply_chain`); the app container gains no new mount or file access. The projection is
//! parsed again strictly by `webagent_administration::supply_chain`. The route answers 404 to
//! any caller who is not a signed-in administrator, so its existence is not disclosed.
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::Value;

use axum::Router;

use crate::{Api, ApiError, App, State};

/// `/admin/supply-chain` behind the 404-on-refusal administrator check; the caller adds the
/// sign-in layer outside it (an unauthenticated caller gets the usual sign-in refusal).
pub(crate) fn router(app: &App) -> Router<App> {
    Router::new()
        .route("/admin/supply-chain", get(read))
        .route_layer(middleware::from_fn_with_state(app.clone(), hidden))
}

/// The administrator check of `admin_auth`, with every refusal reported as not found.
async fn hidden(State(app): State<App>, request: Request, next: Next) -> Response {
    let allowed = match request.extensions().get::<Value>() {
        Some(identity) => crate::phase3_api::administrator(&app, identity)
            .await
            .is_ok(),
        None => false,
    };
    if !allowed {
        return ApiError(StatusCode::NOT_FOUND, "Not found".into()).into_response();
    }
    next.run(request).await
}

#[cfg(feature = "administrator")]
fn limits() -> (webagent_administration::supply_chain::Thresholds, u64) {
    let scan = webagent_notifications::alerts::Config::live().supply_chain;
    (
        webagent_administration::supply_chain::Thresholds {
            stale_days: scan.stale_days,
            missed_check_hours: scan.missed_check_hours,
        },
        scan.check_seconds,
    )
}

#[cfg(feature = "administrator")]
#[allow(clippy::cast_precision_loss)]
fn now() -> f64 {
    crate::now() as f64
}

/// The Dashboard card: the newest scan's view, or `unavailable` when the broker cannot be
/// reached and `failed` when its report is refused.
#[cfg(feature = "administrator")]
async fn read(State(app): State<App>) -> Api {
    use webagent_administration::supply_chain;
    let (thresholds, _) = limits();
    let output = webagent_administration_host::dashboard::supply_chain(
        &crate::tools::administrator::Host(&app),
    )
    .await;
    let view = match output {
        Ok(output) => match supply_chain::parse(&output) {
            Ok(report) => supply_chain::view(&report, thresholds, now()),
            Err(_) => serde_json::json!({"schema_version":1,"state":"failed","available":false,
                "reason":"malformed","stale_days":thresholds.stale_days,"targets":[]}),
        },
        Err(_) => serde_json::json!({"schema_version":1,"state":"unavailable","available":false,
            "reason":"broker_unavailable","stale_days":thresholds.stale_days,"targets":[]}),
    };
    Ok(axum::Json(view))
}

#[cfg(not(feature = "administrator"))]
async fn read() -> Api {
    Err(ApiError(StatusCode::NOT_FOUND, "Not found".into()))
}

/// Read the scan summary every `check_seconds` (one broker read; no per-account work) and
/// raise an administrator alert for new fixable high/critical findings, a failed or
/// unreadable scan, or a stale scan. What was alerted is kept in one memory row, so a restart
/// or the same finding in a later scan does not alert again.
// The administrator account it reads as comes from the Unix-only Host Admin adapter.
#[cfg(all(feature = "administrator", unix))]
pub(crate) async fn watch(app: App) {
    // Let start-up finish before the first broker read.
    let mut wait = std::time::Duration::from_secs(60);
    loop {
        tokio::select! {
            () = app.stop.cancelled() => break,
            () = tokio::time::sleep(wait) => {}
        }
        let (thresholds, every) = limits();
        wait = std::time::Duration::from_secs(every);
        if let Err(error) = check(&app, thresholds).await {
            tracing::debug!("Dependency scan check deferred: {error}");
        }
    }
}

#[cfg(not(all(feature = "administrator", unix)))]
pub(crate) async fn watch(_app: App) {}

#[cfg(all(feature = "administrator", unix))]
async fn check(
    app: &App,
    thresholds: webagent_administration::supply_chain::Thresholds,
) -> anyhow::Result<()> {
    use webagent_administration::supply_chain::{self, Memory, MEMORY_KEY};
    use webagent_notifications::alerts;
    if !webagent_administration::tools::installed() {
        return Ok(());
    }
    let mut caller = app.clone();
    caller.store = app
        .store
        .for_user(&super::host_admin::watch_administrator(app).await?)
        .await?;
    // A broker that cannot be reached decides nothing; a refused report counts as failed.
    let output = webagent_administration_host::dashboard::supply_chain(
        &crate::tools::administrator::Host(&caller),
    )
    .await
    .map_err(|error| anyhow::anyhow!(error.1))?;
    let report = supply_chain::parse(&output).unwrap_or(supply_chain::Report {
        available: false,
        reason: Some("malformed".into()),
        status: None,
        scanned_at: None,
        checked_at: None,
        failed_at: None,
        error: None,
        release: None,
        targets: std::collections::BTreeMap::new(),
        ui_libraries: None,
    });
    let pool = &app.store.app_pool;
    let memory = Memory::from_value(alerts::memory(pool, MEMORY_KEY).await?.as_ref());
    let decision = supply_chain::decide(&report, thresholds, now(), &memory);
    let config = alerts::Config::live();
    let stamp = i64::try_from(crate::now()).unwrap_or(i64::MAX);
    let mut raised = Vec::new();
    if decision.new_findings > 0 {
        if let Some(mut alert) = alerts::single(&config, "supply_chain_fixable") {
            alert.count = u32::try_from(decision.new_findings).unwrap_or(u32::MAX);
            alert.detail.clone_from(&decision.detail);
            raised.push(alert);
        }
    }
    if decision.failed {
        if let Some(mut alert) = alerts::single(&config, "supply_chain_failed") {
            alert.detail.clone_from(&decision.detail);
            raised.push(alert);
        }
    }
    if decision.stale {
        if let Some(mut alert) = alerts::single(&config, "supply_chain_stale") {
            alert.count = u32::try_from(thresholds.stale_days).unwrap_or(u32::MAX);
            raised.push(alert);
        }
    }
    for alert in &raised {
        alerts::deliver(pool, alert, stamp).await?;
    }
    // Remember only after the alerts are recorded: a failed write retries on the next pass.
    if decision.memory != memory {
        alerts::remember(pool, MEMORY_KEY, &decision.memory.to_value(), stamp).await?;
    }
    Ok(())
}

#[cfg(all(test, feature = "administrator"))]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{json, Value};
    use tower::ServiceExt;
    use webagent_accounts_host::AccountsStore;

    #[tokio::test]
    async fn route_is_hidden_from_everyone_but_administrators() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let request = |token: Option<&str>| {
            let mut builder = Request::builder().uri("/api/v3/admin/supply-chain");
            if let Some(token) = token {
                builder = builder.header("cookie", format!("v3_session={token}"));
            }
            builder.body(Body::empty()).unwrap()
        };
        let signed_out = crate::router(app.clone())
            .oneshot(request(None))
            .await
            .unwrap();
        assert_eq!(signed_out.status(), StatusCode::UNAUTHORIZED);

        let admin = crate::uuid();
        app.store
            .put(
                &format!("auth:{}", crate::digest(&json!(admin))),
                &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"}),
            )
            .await
            .unwrap();
        let allowed = crate::router(app.clone())
            .oneshot(request(Some(&admin)))
            .await
            .unwrap();
        assert_eq!(allowed.status(), StatusCode::OK);
        let body = axum::body::to_bytes(allowed.into_body(), 65536)
            .await
            .unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        // No broker in the fixture: the card shows the state, never fake zero counts.
        assert_eq!(value["schema_version"], 1);
        assert!(matches!(
            value["state"].as_str(),
            Some("unavailable" | "failed")
        ));

        let member = app
            .store
            .create_user_account("supply-chain-member", "fixture-only-password-hash")
            .await
            .unwrap();
        let user_id = member["user_id"].as_str().unwrap();
        app.store
            .update_account_access(user_id, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let account = app.store.account_by_id(user_id).await.unwrap().unwrap();
        let token = crate::uuid();
        app.store
            .put(
                &format!("auth:{}", crate::digest(&json!(token))),
                &json!({"csrf":"fixture","expires":crate::now()+3600,
                "generation":account["generation"],"user_id":user_id}),
            )
            .await
            .unwrap();
        let hidden = crate::router(app)
            .oneshot(request(Some(&token)))
            .await
            .unwrap();
        assert_eq!(hidden.status(), StatusCode::NOT_FOUND);
    }
}
