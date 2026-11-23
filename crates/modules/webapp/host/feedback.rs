//! Send feedback: forwards a signed-in caller's report to the configured feedback relay.
//! Submission rules and settings live in `webagent_webapp::feedback`; this root-bound adapter
//! supplies the HTTP handler, the installation's relay identity and the outbound call.
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;
use webagent_webapp::configuration::{app_settings, LIVE_SETTINGS};
use webagent_webapp::feedback::{self, Submission};

use crate::{bad, uuid, Api, ApiError, App, State};

/// Forward one report to the relay and answer with the issue link it returns, when any.
pub(crate) async fn submit(State(app): State<App>, Json(body): Json<Value>) -> Api {
    let submission: Submission =
        serde_json::from_value(body).map_err(|_| bad("Invalid feedback"))?;
    let settings = feedback::settings(&app_settings(Path::new(LIVE_SETTINGS))?)?;
    if !settings.enabled {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Feedback is turned off here".into(),
        ));
    }
    let installation = installation_id(&app.store.root).await?;
    let payload = feedback::relay_payload(&submission, &installation, env!("CARGO_PKG_VERSION"))
        .map_err(bad)?;
    let unreachable = || {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "Feedback service unreachable".into(),
        )
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| unreachable())?;
    let response = client
        .post(&settings.relay_url)
        .json(&payload)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "feedback relay call failed");
            unreachable()
        })?;
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many submissions. Please try again later.".into(),
        ));
    }
    if !status.is_success() {
        tracing::warn!(
            status = status.as_u16(),
            "feedback relay rejected a submission"
        );
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "Feedback service rejected the submission".into(),
        ));
    }
    let reply: Value = response.json().await.unwrap_or(Value::Null);
    let issue_url = reply["issue_url"]
        .as_str()
        .filter(|url| url.starts_with("https://"));
    Ok(Json(json!({"ok": true, "issue_url": issue_url})))
}

/// The installation's stable relay identity, created on first use so the relay can rate-limit
/// one installation without affecting others. It lives in state, never in shared config.
async fn installation_id(root: &Path) -> anyhow::Result<String> {
    let path = root.join("webapp/feedback-installation-id");
    if let Ok(existing) = tokio::fs::read_to_string(&path).await {
        let existing = existing.trim();
        if existing.len() >= 16 {
            return Ok(existing.to_owned());
        }
    }
    tokio::fs::create_dir_all(root.join("webapp")).await?;
    let fresh = uuid();
    tokio::fs::write(&path, &fresh).await?;
    Ok(fresh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn installation_id_is_created_once_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let first = installation_id(root.path()).await.unwrap();
        assert!(first.len() >= 16);
        assert_eq!(installation_id(root.path()).await.unwrap(), first);
    }
}
