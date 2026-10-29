//! Source promotion routes. The Administration module owns the proposal, review,
//! rejection and broker dispatch; this adds HTTP framing and the root's facades.
use crate::tools::administrator::Host;
use crate::{Api, App, State};
use anyhow::Result;
use axum::extract::Path;
use axum::Json;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use webagent_administration_host::promotion;

/// Record the session's reviewed patch as a proposal from run `run_id`.
pub(crate) async fn propose_promotion(
    app: &App,
    sid: &str,
    request_id: &str,
    run_id: &str,
    args: &Value,
    policy_version: u64,
    cancel: CancellationToken,
) -> Result<Value> {
    let proposal = promotion::Proposal {
        sid,
        request_id,
        run_id,
        policy_version,
    };
    promotion::propose(&Host(app), proposal, args, cancel).await
}

pub(crate) async fn promotions(State(app): State<App>, Path(id): Path<String>) -> Api {
    Ok(Json(promotion::list(&Host(&app), &id).await?))
}

pub(crate) async fn promotion_review(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Api {
    Ok(Json(
        promotion::review(&Host(&app), &id, &request_id).await?,
    ))
}

pub(crate) async fn promotion_receipt(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Api {
    Ok(Json(
        promotion::receipt(&Host(&app), &id, &request_id).await?,
    ))
}

pub(crate) async fn promotion_reject(
    State(app): State<App>,
    Path((id, request_id)): Path<(String, String)>,
) -> Api {
    Ok(Json(
        promotion::reject(&Host(&app), &id, &request_id).await?,
    ))
}

pub(crate) async fn promote(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    Ok(Json(promotion::promote(&Host(&app), &id, &body).await?))
}

/// Mark promotions left dispatching by a restart as uncertain.
pub(crate) async fn recover_promotions(app: &App) -> Result<()> {
    promotion::recover(&Host(app)).await
}
