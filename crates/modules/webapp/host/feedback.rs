//! Send feedback: forwards a signed-in caller's report to the configured feedback relay.
//! Submission rules, settings and the attempt limiter live in `webagent_webapp::feedback`;
//! this root-bound adapter supplies the HTTP handler, the caller check, the installation's
//! relay identity and the outbound call.
use axum::http::StatusCode;
use axum::Json;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;
use webagent_access::identity::Identity;
use webagent_access::Role;
use webagent_webapp::configuration::{app_settings, LIVE_SETTINGS};
use webagent_webapp::feedback::{self, Limiter, Settings, Submission};

use crate::{bad, uuid, Api, ApiError, App};

/// Outbound attempts counted for this API process; they start again at each restart.
static LIMITER: LazyLock<Mutex<Limiter>> = LazyLock::new(Default::default);
/// One client for every relay call, so repeat calls reuse its connection set-up.
static CLIENT: LazyLock<Option<reqwest::Client>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .ok()
});

/// Forward one report to the relay; the reply carries no issue link (the repository is private).
/// The caller's handle comes from the request: the sign-in middleware stores it there with the
/// caller's identity, which the router's shared `State` never carries.
pub(crate) async fn submit(
    axum::Extension(app): axum::Extension<App>,
    Json(body): Json<Value>,
) -> Api {
    let settings = feedback::settings(&app_settings(Path::new(LIVE_SETTINGS))?)?;
    let installation = installation_id(&app.store.root).await?;
    let (user_id, kind, payload) = {
        let mut limiter = LIMITER
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prepare(
            app.identity.as_deref(),
            &settings,
            body,
            &installation,
            &mut limiter,
            crate::now(),
        )?
    };
    let outcome = send(&settings.relay_url, &payload).await;
    tracing::info!(target: "feedback", user_id, kind, outcome = outcome.as_ref().map_or_else(|e| e.1.as_str(), |_| "sent"), "feedback attempt");
    outcome.map(|()| Json(json!({"ok": true})))
}

/// Only people signed in to the app with a registered account may send: the app's guest
/// (anonymous) role is refused. A person's guest or member standing in an agent does not count.
fn may_send(identity: Option<&Identity>) -> Option<&Identity> {
    identity.filter(|identity| identity.principal.role != Role::Guest)
}

/// Everything before the outbound call: switch, caller, report rules, then the limits. The
/// attempt is charged here, so it counts whatever the relay later answers.
fn prepare(
    identity: Option<&Identity>,
    settings: &Settings,
    body: Value,
    installation: &str,
    limiter: &mut Limiter,
    now: u64,
) -> Result<(String, String, Value), ApiError> {
    if !settings.enabled {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Feedback is turned off here".into(),
        ));
    }
    let caller = may_send(identity).ok_or_else(|| {
        ApiError(
            StatusCode::FORBIDDEN,
            "Sign in with an account to send feedback".into(),
        )
    })?;
    let submission: Submission =
        serde_json::from_value(body).map_err(|_| bad("Invalid feedback"))?;
    let payload = feedback::relay_payload(&submission, installation, env!("CARGO_PKG_VERSION"))
        .map_err(bad)?;
    if !limiter.try_charge(caller.user_id(), now, settings) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many submissions. Please try again later.".into(),
        ));
    }
    Ok((caller.user_id().to_owned(), submission.kind, payload))
}

/// Post one report; only the relay's status is used, its body is never read.
async fn send(relay_url: &str, payload: &Value) -> Result<(), ApiError> {
    let unreachable = || {
        ApiError(
            StatusCode::BAD_GATEWAY,
            "Feedback service unreachable".into(),
        )
    };
    let client = CLIENT.as_ref().ok_or_else(unreachable)?;
    let status = client
        .post(relay_url)
        .json(payload)
        .send()
        .await
        .map_err(|error| {
            tracing::warn!(%error, "feedback relay call failed");
            unreachable()
        })?
        .status();
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
    Ok(())
}

/// The installation's stable relay identity, created on first use so the relay can rate-limit
/// one installation without affecting others. It lives in state, never in shared config. A new
/// ID is written to a private temporary file and then linked into place, so a reader never sees
/// a partly written file and simultaneous first uses all return the one that was linked first.
async fn installation_id(root: &Path) -> anyhow::Result<String> {
    let path = root.join("webapp/feedback-installation-id");
    let read = || async {
        tokio::fs::read_to_string(&path)
            .await
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| id.len() >= 16)
    };
    if let Some(id) = read().await {
        return Ok(id);
    }
    tokio::fs::create_dir_all(root.join("webapp")).await?;
    let fresh = uuid();
    let temporary = root.join(format!("webapp/.feedback-installation-id.{fresh}"));
    tokio::fs::write(&temporary, &fresh).await?;
    tokio::fs::File::open(&temporary).await?.sync_all().await?;
    let published = if tokio::fs::try_exists(&path).await? {
        // A concurrent first use may have published a valid ID since the read above (the ID
        // appears complete: hard link or rename); keep it. Only an existing file that is not a
        // valid ID (for example emptied by a power loss) is replaced.
        if let Some(id) = read().await {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Ok(id);
        }
        tokio::fs::rename(&temporary, &path).await
    } else {
        match tokio::fs::hard_link(&temporary, &path).await {
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    };
    let _ = tokio::fs::remove_file(&temporary).await;
    published?;
    read()
        .await
        .ok_or_else(|| anyhow::anyhow!("Feedback installation ID unreadable"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use webagent_access::identity::CredentialKind;
    use webagent_access::{AccountStatus, Principal};

    fn identity(role: Role, user_id: &str) -> Identity {
        Identity {
            principal: Principal {
                user_id: user_id.into(),
                status: AccountStatus::Active,
                role,
                tier_id: "free".into(),
                access_revision: 1,
                test_account: false,
            },
            username: user_id.into(),
            generation: "g".into(),
            auth_epoch: Some(0),
            kind: CredentialKind::Person,
            credential_key: "auth:x".into(),
            credential_record: Vec::new(),
            agent_id: None,
        }
    }

    fn report() -> Value {
        json!({"type":"bug","body":"broken"})
    }

    #[test]
    fn only_registered_app_accounts_pass_and_refusals_charge_nothing() {
        let on = feedback::settings(&json!({})).unwrap();
        let off = feedback::settings(&json!({"feedback":{"enabled":false}})).unwrap();
        let user = identity(Role::User, "u1");
        let admin = identity(Role::Administrator, "a1");
        let guest = identity(Role::Guest, "g1");
        let mut limiter = Limiter::default();
        let status = |result: Result<(String, String, Value), ApiError>| {
            result.map(|_| StatusCode::OK).unwrap_or_else(|e| e.0)
        };
        assert_eq!(
            status(prepare(Some(&user), &off, report(), "i", &mut limiter, 0)),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(prepare(Some(&guest), &on, report(), "i", &mut limiter, 0)),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(prepare(None, &on, report(), "i", &mut limiter, 0)),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status(prepare(
                Some(&user),
                &on,
                json!({"type":"bug","body":" "}),
                "i",
                &mut limiter,
                0
            )),
            StatusCode::BAD_REQUEST
        );
        // None of the refusals above was charged: all five hourly attempts remain.
        for _ in 0..5 {
            assert_eq!(
                status(prepare(Some(&user), &on, report(), "i", &mut limiter, 0)),
                StatusCode::OK
            );
        }
        assert_eq!(
            status(prepare(Some(&user), &on, report(), "i", &mut limiter, 0)),
            StatusCode::TOO_MANY_REQUESTS
        );
        let (caller, kind, payload) =
            prepare(Some(&admin), &on, report(), "inst", &mut limiter, 0).unwrap();
        assert_eq!((caller.as_str(), kind.as_str()), ("a1", "bug"));
        assert!(
            !payload.to_string().contains("a1"),
            "the relay never sees the account"
        );
    }

    #[tokio::test]
    async fn installation_id_is_created_once_and_shared_by_concurrent_first_uses() {
        let root = tempfile::tempdir().unwrap();
        let (a, b) = tokio::join!(installation_id(root.path()), installation_id(root.path()));
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(a.len() >= 16);
        assert_eq!(a, b);
        assert_eq!(installation_id(root.path()).await.unwrap(), a);
    }

    /// Through the real router and sign-in middleware: a registered user's report reaches the
    /// report rules (400 for an empty one, so nothing is sent), proving the caller's identity
    /// arrives; without a sign-in the router refuses before the handler.
    #[tokio::test]
    async fn the_route_sees_the_signed_in_caller() {
        use tower::ServiceExt;
        use webagent_accounts_host::AccountsStore;
        let (_root, app) = crate::runtime_tests::fixture().await;
        let account = app
            .store
            .create_user_account("feedback-user", "fixture-long-password-hash")
            .await
            .unwrap();
        let user = account["user_id"].as_str().unwrap().to_owned();
        app.store
            .update_account_access(&user, 1, crate::access::AccountStatus::Active, "free")
            .await
            .unwrap();
        let account = app.store.account_by_id(&user).await.unwrap().unwrap();
        let token = uuid();
        app.store
            .put(
                &format!("auth:{}", crate::digest(&json!(token))),
                &json!({"user_id":user,"csrf":"fixture","expires":crate::now()+3600,"generation":account["generation"]}),
            )
            .await
            .unwrap();
        let post = |cookie: Option<String>| {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/v3/feedback")
                .header("content-type", "application/json")
                .header("x-csrf-token", "fixture");
            if let Some(cookie) = cookie {
                request = request.header("cookie", cookie);
            }
            request
                .body(axum::body::Body::from(
                    json!({"type":"bug","body":" "}).to_string(),
                ))
                .unwrap()
        };
        let router = crate::router(app.clone());
        let signed_in = router
            .clone()
            .oneshot(post(Some(format!("v3_session={token}"))))
            .await
            .unwrap();
        assert_eq!(signed_in.status(), StatusCode::BAD_REQUEST);
        let anonymous = router.oneshot(post(None)).await.unwrap();
        assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
    }
}
