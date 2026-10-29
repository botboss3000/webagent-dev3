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
