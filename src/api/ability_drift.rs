//! Administrator popup for ability changes in the live checkout that need a rebuild.
//! Each administrator dismisses a change set either until their next login or for good;
//! a different change set shows again.
use crate::App;
use axum::{extract::State, http::StatusCode, Extension, Json};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webagent_host_core::{Api, ApiError};

const STATE: &str = "ability_drift_notice";
const SILENCED: usize = 32;

/// This login's identity for "remind me at next login"; the session key itself is never stored.
fn login(identity: &Value) -> String {
    hex::encode(Sha256::digest(
        identity["_auth_key"].as_str().unwrap_or_default(),
    ))
}

/// GET: the current change set, and whether this login should see it.
pub(crate) async fn read(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    let drift = crate::ability_drift::current();
    let digest = drift.digest();
    let state = app.store.get(STATE).await?.unwrap_or_default();
    let silenced = state["silenced"]
        .as_array()
        .is_some_and(|items| items.iter().any(|item| item == digest.as_str()));
    let snoozed = state["snoozed"]["digest"] == digest.as_str()
        && state["snoozed"]["login"] == login(&identity).as_str();
    Ok(Json(json!({"digest": digest, "notices": drift.notices,
        "show": !digest.is_empty() && !silenced && !snoozed})))
}

/// POST `{digest, remind_next_login}`: hide this change set until the next login, or
/// for good when `remind_next_login` is false.
pub(crate) async fn dismiss(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let (Some(digest), Some(remind)) =
        (body["digest"].as_str(), body["remind_next_login"].as_bool())
    else {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "digest and remind_next_login are required".into(),
        ));
    };
    if digest.is_empty() || digest != crate::ability_drift::current().digest() {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "The changes are different now; reload them".into(),
        ));
    }
    let mut state = app.store.get(STATE).await?.unwrap_or_else(|| json!({}));
    if remind {
        state["snoozed"] = json!({"digest": digest, "login": login(&identity)});
    } else {
        let mut silenced: Vec<Value> = state["silenced"].as_array().cloned().unwrap_or_default();
        silenced.retain(|item| item != digest);
        silenced.push(json!(digest));
        let keep = silenced.len().saturating_sub(SILENCED);
        state["silenced"] = json!(silenced.split_off(keep));
    }
    app.store.put(STATE, &state).await?;
    Ok(Json(json!({"show": false})))
}

/// The broker's administrator workspace for checkout rebuilds.
const ADMIN_WORKSPACE: &str = "00000000-0000-0000-0000-000000000000";

/// POST `{mode, request_id}`: release the live checkout as it is through the protected
/// broker. `dev` publishes ability packages when only abilities changed (no restart);
/// `stable` is a full build that links ability plugins and briefly restarts the app. The
/// broker accepts only the installation administrator and returns the same job when the
/// same `request_id` is retried.
pub(crate) async fn rebuild(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let mode = match body["mode"].as_str() {
        Some(mode @ ("dev" | "stable")) => mode,
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "mode must be dev or stable".into())),
    };
    let request_id = body["request_id"]
        .as_str()
        .filter(|id| {
            (8..=64).contains(&id.len())
                && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
        .ok_or_else(|| ApiError(StatusCode::BAD_REQUEST, "request_id required".into()))?;
    let user = identity["_user_id"].as_str().unwrap_or_else(|| app.store.current_user_id());
    let fingerprint = hex::encode(Sha256::digest(format!("ability-rebuild\0{user}\0{request_id}")));
    let trusted = json!({"user_id": user, "session_id": ADMIN_WORKSPACE,
        "lease_until": webagent_host_core::now() + 30, "access_fingerprint": fingerprint});
    let reply = crate::tools::execute_host_trusted(
        "v3_build_rebuild_checkout",
        &json!({"mode": mode, "request_id": request_id}),
        ADMIN_WORKSPACE,
        &app.build_socket,
        Some(trusted),
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|error| ApiError(StatusCode::BAD_GATEWAY, format!("Build service unavailable: {error:#}")))?;
    if reply["status"] != "success" {
        let reason = reply["output"].as_str().unwrap_or("Rebuild refused").to_owned();
        let status = if reason.contains("Administrator rebuild required") { StatusCode::FORBIDDEN } else { StatusCode::CONFLICT };
        return Err(ApiError(status, reason));
    }
    Ok(Json(reply["output"].clone()))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test]
    async fn rebuild_requires_a_mode_and_request_id_and_reports_an_absent_broker() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id": "admin", "_auth_key": "login"});
        let call = |body: Value| rebuild(State(app.clone()), Extension(identity.clone()), Json(body));
        for body in [json!({"mode": "release", "request_id": "press-0001"}), json!({"mode": "dev"}),
            json!({"mode": "dev", "request_id": "x"}), json!({"mode": "dev", "request_id": "a/b/c/d/e"})] {
            assert_eq!(call(body).await.unwrap_err().0, StatusCode::BAD_REQUEST);
        }
        let unavailable = call(json!({"mode": "dev", "request_id": "press-0001"})).await.unwrap_err();
        assert_eq!(unavailable.0, StatusCode::BAD_GATEWAY);
    }
}
