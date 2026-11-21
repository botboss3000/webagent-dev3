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
    crate::api::notifications::alert(app, "rate_limited");
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

/// The client's address: a direct (untrusted) peer, or one a trusted front proves.
/// `None` for unknown clients, such as a trusted proxy's request without the edge's proof.
pub(crate) fn proven_client(app: &App, request: &Request) -> Option<std::net::IpAddr> {
    let peer = request
        .extensions()
        .get::<ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0);
    client_from(app, peer, request.headers())
}

/// `proven_client` for handlers that extract the peer and headers themselves.
pub(crate) fn client_from(
    app: &App,
    peer: Option<std::net::SocketAddr>,
    headers: &axum::http::HeaderMap,
) -> Option<std::net::IpAddr> {
    let (_, settings) = current(app);
    webagent_accounts::client_ip(peer, headers, &settings)
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

/// Wrong pairing codes are capped per client address, and all clients together get ten
/// times that, so one sender cannot lock pairing for everyone. Unknown clients share one key.
const PAIRING_GLOBAL_FACTOR: u32 = 10;
fn pairing_keys(client: Option<std::net::IpAddr>) -> [String; 2] {
    let client = client.map_or_else(|| "unknown".to_owned(), |ip| ip.to_string());
    [
        format!("pairing:failures:{client}"),
        "pairing:failures".to_owned(),
    ]
}

/// Seconds to wait while this client's, or everyone's, wrong pairing codes used their cap.
pub(crate) async fn pairing_wait(app: &App, client: Option<std::net::IpAddr>) -> u64 {
    let (limits, _) = current(app);
    let cap = limits.pairing_failures_per_minute;
    let [own, all] = pairing_keys(client);
    let now = now();
    let own = app.login_attempts.retry_after(&own, cap, now).await;
    let all = app
        .login_attempts
        .retry_after(&all, cap.saturating_mul(PAIRING_GLOBAL_FACTOR), now)
        .await;
    own.max(all)
}

/// Count one new account against the shared daily cap; false once today's cap is used.
pub(crate) async fn new_account_admitted(app: &App) -> bool {
    let (limits, _) = current(app);
    app.login_attempts
        .hit(
            "new_accounts".into(),
            limits.new_accounts_per_day,
            86_400,
            now(),
        )
        .await
}

/// Record one wrong pairing code against the client's cap and the shared ceiling.
pub(crate) async fn pairing_failed(app: &App, client: Option<std::net::IpAddr>) {
    let (limits, _) = current(app);
    let cap = limits.pairing_failures_per_minute;
    let [own, all] = pairing_keys(client);
    let now = now();
    app.login_attempts.hit(own, cap, 60, now).await;
    app.login_attempts
        .hit(all, cap.saturating_mul(PAIRING_GLOBAL_FACTOR), 60, now)
        .await;
}

/// Open live-update streams one user may hold at once: each polls the database several times
/// a second, so the count is what bounds a user's background load.
const STREAMS_PER_USER: usize = 32;
static STREAMS: SyncMutex<Option<std::collections::HashMap<String, usize>>> = SyncMutex::new(None);

/// One open stream; dropping it (the stream ends or the client goes away) frees the slot.
pub(crate) struct StreamSlot(String);
impl Drop for StreamSlot {
    fn drop(&mut self) {
        let mut streams = STREAMS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(map) = streams.as_mut() {
            if let Some(count) = map.get_mut(&self.0) {
                *count -= 1;
                if *count == 0 {
                    map.remove(&self.0);
                }
            }
        }
    }
}

/// A slot for one more open stream, or `None` once the user holds `STREAMS_PER_USER`.
pub(crate) fn stream_slot(user: &str) -> Option<StreamSlot> {
    let mut streams = STREAMS.lock().unwrap_or_else(|p| p.into_inner());
    let count = streams
        .get_or_insert_with(Default::default)
        .entry(user.to_owned())
        .or_insert(0);
    if *count >= STREAMS_PER_USER {
        return None;
    }
    *count += 1;
    Some(StreamSlot(user.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::{stream_slot, user_lane, STREAMS_PER_USER};
    #[test]
    fn open_streams_are_capped_per_user_and_freed_on_drop() {
        let held: Vec<_> = (0..STREAMS_PER_USER)
            .map(|_| stream_slot("stream-cap-user").unwrap())
            .collect();
        assert!(stream_slot("stream-cap-user").is_none());
        assert!(stream_slot("stream-cap-other").is_some());
        drop(held);
        assert!(stream_slot("stream-cap-user").is_some());
    }
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
