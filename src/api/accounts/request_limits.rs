//! Request limits from the access policy's `request_limits` section. Signed-in work is
//! limited per user and messaging calls per token, so it needs no client address; every
//! `/api/` request is also limited per address when a trusted front names the client.
use crate::*;
use axum::extract::ConnectInfo;
use std::sync::Mutex as SyncMutex;
use std::time::{Duration, Instant};

/// Policy and settings as read at most every few seconds: the per-address check runs on
/// every API request, and both files are live configuration.
struct Cached {
    at: Instant,
    limits: webagent_accounts::RequestLimits,
    settings: Value,
}
static CACHE: SyncMutex<Option<Cached>> = SyncMutex::new(None);
const FRESH: Duration = Duration::from_secs(5);

/// The current limits and proxy trust settings; an unreadable policy keeps the defaults rather
/// than turning limits off.
fn current(app: &App) -> (webagent_accounts::RequestLimits, Value) {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache.as_ref().filter(|c| c.at.elapsed() < FRESH) {
        return (cached.limits, cached.settings.clone());
    }
    let limits = app
        .store
        .read_access_policy()
        .ok()
        .and_then(|policy| webagent_accounts::RequestLimits::from_policy(&policy).ok())
        .unwrap_or_default();
    let settings = webagent_accounts::trust_settings(&app.store.root).unwrap_or_else(|_| json!({}));
    *cache = Some(Cached {
        at: Instant::now(),
        limits,
        settings: settings.clone(),
    });
    (limits, settings)
}

/// A 429 that says when to retry.
fn too_many(retry_after: u64, what: &str) -> Response {
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": format!("Too many {what}. Try again in {retry_after} seconds."),
            "retry_after": retry_after,
        })),
    )
        .into_response();
    response
        .headers_mut()
        .insert("retry-after", retry_after.into());
    response
}

/// Count one event; the 429 to return when its per-minute cap is already used.
async fn count(app: &App, key: String, cap: u32, what: &str) -> Option<Response> {
    let at = now();
    if app.login_attempts.hit(key.clone(), cap, 60, at).await {
        return None;
    }
    let wait = app.login_attempts.retry_after(&key, cap, at).await.max(1);
    Some(too_many(wait, what))
}

/// The per-user cap a signed-in request falls under: chat sends and uploads.
fn user_lane(method: &axum::http::Method, path: &str) -> Option<&'static str> {
    if method != axum::http::Method::POST {
        return None;
    }
    let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
    match parts.as_slice() {
        ["messages"] | ["sessions", _, "messages"] => Some("chat_send"),
        ["sessions", _, "attachments"] | ["sessions", _, "attachments", _, "describe"] => {
            Some("upload")
        }
        _ => None,
    }
}

/// Called by `auth` once the user is known; `path` is relative to `/api/v3`.
pub(crate) async fn signed_in(
    app: &App,
    user_id: &str,
    method: &axum::http::Method,
    path: &str,
) -> Option<Response> {
    let lane = user_lane(method, path.strip_prefix("/api/v3").unwrap_or(path))?;
    let (limits, _) = current(app);
    let user = limits.per_user_per_minute;
    let (cap, what) = match lane {
        "chat_send" => (user.chat_send, "messages"),
        _ => (user.upload, "uploads"),
    };
    count(app, format!("user:{lane}:{user_id}"), cap, what).await
}

/// Messaging API calls per bearer token; runs after the token is verified.
pub(crate) async fn messaging(State(app): State<App>, request: Request, next: Next) -> Response {
    let token = request
        .headers()
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    let key = format!("messaging:{:x}", sha2::Sha256::digest(token.as_bytes()));
    let (limits, _) = current(&app);
    if let Some(refused) = count(
        &app,
        key,
        limits.per_user_per_minute.messaging_api,
        "messaging API calls",
    )
    .await
    {
        return refused;
    }
    next.run(request).await
}

/// The client address a trusted front proves, if any; `None` for unknown clients.
pub(crate) fn proven_client(app: &App, request: &Request) -> Option<std::net::IpAddr> {
    // Without the edge's proof header the address can never be proven: skip the file reads.
    request.headers().get("x-webagent-client-proof")?;
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0);
    let (_, settings) = current(app);
    webagent_accounts::client_ip(peer, request.headers(), &settings)
        .ok()
        .filter(|ip| !ip.is_unspecified())
}

/// Every `/api/` request from an address a trusted front names, and its pairing attempts.
pub(crate) async fn per_address(State(app): State<App>, request: Request, next: Next) -> Response {
    if request.uri().path().starts_with("/api/") {
        if let Some(client) = proven_client(&app, &request) {
            let (limits, _) = current(&app);
            if request.uri().path() == "/api/v3/harness-bridge/pair" {
                if let Some(refused) = count(
                    &app,
                    format!("address:pairing:{client}"),
                    limits.per_address_per_minute.pairing,
                    "pairing attempts",
                )
                .await
                {
                    return refused;
                }
            }
            if let Some(refused) = count(
                &app,
                format!("address:api:{client}"),
                limits.per_address_per_minute.api,
                "requests",
            )
            .await
            {
                return refused;
            }
        }
    }
    next.run(request).await
}

/// Seconds to wait while wrong pairing codes from everyone have used this minute's cap.
pub(crate) async fn pairing_wait(app: &App) -> u64 {
    let (limits, _) = current(app);
    app.login_attempts
        .retry_after(
            "pairing:failures",
            limits.pairing_failures_per_minute,
            now(),
        )
        .await
}

/// Record one wrong pairing code against the shared cap.
pub(crate) async fn pairing_failed(app: &App) {
    let (limits, _) = current(app);
    app.login_attempts
        .hit(
            "pairing:failures".into(),
            limits.pairing_failures_per_minute,
            60,
            now(),
        )
        .await;
}

#[cfg(test)]
mod tests {
    use super::user_lane;
    use axum::http::Method;
    #[test]
    fn sends_and_uploads_are_the_limited_lanes() {
        assert_eq!(
            user_lane(&Method::POST, "/sessions/s1/messages"),
            Some("chat_send")
        );
        assert_eq!(user_lane(&Method::POST, "/messages"), Some("chat_send"));
        assert_eq!(
            user_lane(&Method::POST, "/sessions/s1/attachments"),
            Some("upload")
        );
        assert_eq!(
            user_lane(&Method::POST, "/sessions/s1/attachments/a1/describe"),
            Some("upload")
        );
        assert_eq!(user_lane(&Method::GET, "/sessions/s1/messages"), None);
        assert_eq!(
            user_lane(&Method::POST, "/sessions/s1/messages/m1/delivery"),
            None
        );
        assert_eq!(user_lane(&Method::POST, "/sessions"), None);
    }
}
