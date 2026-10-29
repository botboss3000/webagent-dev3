//! Private development-browser artifacts (PNG screenshots), read through the caller's own
//! session and browser authority only.
use crate::AdministrationHost;
use axum::http::StatusCode;
use base64::Engine;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_accounts_host::AccountsStore;
use webagent_host_core::{bad, digest, now, ApiError};

/// Largest artifact served, in bytes.
const MAX_ARTIFACT: usize = 1024 * 1024;

fn denied() -> ApiError {
    ApiError(
        StatusCode::FORBIDDEN,
        "Artifact access unavailable under current policy".into(),
    )
}

fn invalid() -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Invalid artifact".into())
}

/// The fingerprint of the caller's authority over session `sid`'s artifacts, and the
/// browser authority the worker checks.
async fn authority(host: &dyn AdministrationHost, sid: &str) -> Result<(String, Value), ApiError> {
    let not_found = || ApiError(StatusCode::NOT_FOUND, "Session not found".into());
    let access = host
        .access(sid, &[("dev_browser_action", "diagnostics")])
        .await?
        .ok_or_else(not_found)?;
    if !access.active {
        return Err(not_found());
    }
    if !access.admitted
        || access
            .permissions
            .first()
            .is_none_or(|p| *p == Permission::Deny)
    {
        return Err(denied());
    }
    let store = host.store();
    let user = store.current_user_id();
    let browser = store
        .get(&format!("administrator_browser:{sid}"))
        .await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Artifact not found".into()))?;
    let account = store.account_by_id(user).await?.ok_or_else(denied)?;
    if browser["session_id"] != sid
        || browser["user_id"] != user
        || browser["agent_id"] != access.agent.as_str()
        || browser["generation"] != account["generation"]
        || browser["expires_at"].as_u64().unwrap_or(0) <= now()
    {
        return Err(denied());
    }
    let fingerprint = digest(&json!([
        access.fingerprint,
        browser,
        host.abilities_version()
    ]));
    Ok((fingerprint, browser))
}

/// The PNG bytes of artifact `id` in session `sid`. Worker I/O is not an authorization
/// lease: a restriction or credential change during the read withdraws the bytes.
pub async fn read(host: &dyn AdministrationHost, sid: &str, id: &str) -> Result<Vec<u8>, ApiError> {
    if uuid::Uuid::parse_str(id).is_err() {
        return Err(bad("Invalid artifact identifier"));
    }
    let (before, browser) = authority(host, sid).await?;
    let output = host
        .worker(
            "administrator_artifact_get",
            &json!({"artifact_id":id}),
            sid,
            Some(browser),
            CancellationToken::new(),
        )
        .await?;
    let (after, _) = authority(host, sid).await?;
    if before != after {
        return Err(denied());
    }
    let encoded = output["png_base64"]
        .as_str()
        .filter(|value| value.len() <= 1_400_000)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Artifact unavailable".into()))?;
    let data = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|_| invalid())?;
    if data.len() > MAX_ARTIFACT || !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(invalid());
    }
    Ok(data)
}
