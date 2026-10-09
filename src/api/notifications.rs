//! Installation-admin event reads; the assistant uses this feed through outbound polling.
use super::*;
use sqlx::Row;
use std::collections::VecDeque;
use std::sync::{Mutex as StdMutex, OnceLock};

#[derive(Default)]
struct Counts {
    credentials: HashMap<String, usize>,
    users: HashMap<String, usize>,
    total: usize,
}
static COUNTS: OnceLock<StdMutex<Counts>> = OnceLock::new();
struct Connection {
    credential: String,
    user: String,
}
impl Connection {
    fn acquire(identity: &Value) -> std::result::Result<Self, ApiError> {
        let credential = identity["_auth_key"]
            .as_str()
            .filter(|v| !v.is_empty())
            .ok_or_else(|| bad("Missing event credential"))?
            .to_owned();
        let user = identity["_user_id"].as_str().unwrap_or("").to_owned();
        let mut counts = COUNTS.get_or_init(Default::default).lock().unwrap();
        if counts.credentials.get(&credential).copied().unwrap_or(0) >= 4
            || counts.users.get(&user).copied().unwrap_or(0) >= 8
            || counts.total >= 64
        {
            return Err(ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Event connection limit reached".into(),
            ));
        }
        *counts.credentials.entry(credential.clone()).or_default() += 1;
        *counts.users.entry(user.clone()).or_default() += 1;
        counts.total += 1;
        Ok(Self { credential, user })
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        let mut counts = COUNTS.get().unwrap().lock().unwrap();
        for (key, credential) in [(&self.credential, true), (&self.user, false)] {
            let map = if credential {
                &mut counts.credentials
            } else {
                &mut counts.users
            };
            if let Some(count) = map.get_mut(key) {
                *count -= 1;
                if *count == 0 {
                    map.remove(key);
                }
            }
        }
        counts.total -= 1;
    }
}

// Event-reader credentials are hashed, sealed, revocable, and limited to these two GET routes.
// This is deliberately separate from the message-submission bearer API.
pub(crate) async fn bearer_identity(
    app: &App,
    method: &axum::http::Method,
    path: &str,
    headers: &HeaderMap,
) -> Option<Value> {
    if *method != axum::http::Method::GET
        || !matches!(
            path,
            "/api/v3/admin/events"
                | "/api/v3/admin/events/stream"
                | "/admin/events"
                | "/admin/events/stream"
        )
    {
        return None;
    }
    let token = headers
        .get("authorization")?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")?;
    if token.len() != 64 || !token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let key = format!("notification_reader:{}", digest(&json!(token)));
    let mut identity = app.store.get(&key).await.ok()??;
    identity["_auth_key"] = json!(key);
    identity["_user_id"] = identity["user_id"].clone();
    if valid(app, &identity).await {
        Some(identity)
    } else {
        None
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReaderBody {
    expires_in: u64,
}
pub(super) async fn issue_reader(
    State(app): State<App>,
    axum::Extension(identity): axum::Extension<Value>,
    Json(body): Json<ReaderBody>,
) -> Api {
    if !(60..=90 * 24 * 60 * 60).contains(&body.expires_in) {
        return Err(bad("Reader expiration must be 60 seconds to 90 days"));
    }
    let _guard = app.gate.lock().await;
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM docs WHERE key LIKE 'notification_reader:%'")
            .fetch_one(&app.store.app_pool)
            .await
            .map_err(anyhow::Error::from)?;
    if count >= 64 {
        return Err(conflict(
            "Event reader credential limit reached; revoke unused readers",
        ));
    }
    let user = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Reader owner required"))?;
    let account = app
        .store
        .account_by_id(user)
        .await?
        .ok_or_else(|| bad("Reader owner unavailable"))?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let id = digest(&json!(token));
    let record = json!({"user_id":user,"generation":account["generation"],"created_at":now(),"expires":now()+body.expires_in});
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
        .bind(format!("notification_reader:{id}"))
        .bind(app.store.seal(&record)?)
        .execute(&app.store.app_pool)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(
        json!({"id":id,"token":token,"expires":record["expires"],"scope":"events:read","max_connections":4}),
    ))
}
pub(super) async fn readers(State(app): State<App>) -> Api {
    let rows = sqlx::query(
        "SELECT key,value FROM docs WHERE key LIKE 'notification_reader:%' ORDER BY key LIMIT 64",
    )
    .fetch_all(&app.store.app_pool)
    .await
    .map_err(anyhow::Error::from)?;
    let mut readers = Vec::new();
    for row in rows {
        let doc = app.store.unseal(&row.get::<Vec<u8>, _>("value"))?;
        readers.push(json!({"id":row.get::<String,_>("key").trim_start_matches("notification_reader:"),
            "user_id":doc["user_id"],"created_at":doc["created_at"],"expires":doc["expires"],"scope":"events:read"}));
    }
    Ok(Json(json!({"readers":readers})))
}
pub(super) async fn revoke_reader(State(app): State<App>, Path(id): Path<String>) -> Api {
    if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(bad("Invalid event reader ID"));
    }
    sqlx::query("DELETE FROM docs WHERE key=?")
        .bind(format!("notification_reader:{id}"))
        .execute(&app.store.app_pool)
        .await
        .map_err(anyhow::Error::from)?;
    Ok(Json(json!({"revoked":true})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Args {
    since: Option<i64>,
    limit: Option<i64>,
    wait: Option<u64>,
}
fn bounds(args: &Args) -> std::result::Result<(i64, i64, u64), ApiError> {
    let (since, limit, wait) = (
        args.since.unwrap_or(0),
        args.limit.unwrap_or(200),
        args.wait.unwrap_or(30),
    );
    if since < 0 || !(1..=200).contains(&limit) || wait > 30 {
        return Err(bad("Invalid event cursor, limit or wait"));
    }
    Ok((since, limit, wait))
}
async fn valid(app: &App, identity: &Value) -> bool {
    let Some(key) = identity["_auth_key"].as_str() else {
        return false;
    };
    let Some(user) = identity["_user_id"].as_str() else {
        return false;
    };
    let auth = app.store.get(key).await.ok().flatten();
    let account = app.store.account_by_id(user).await.ok().flatten();
    auth.as_ref().is_some_and(|doc| {
        doc["expires"].as_u64().unwrap_or(0) > now()
            && doc["generation"] == identity["generation"]
            && doc["user_id"].as_str().unwrap_or("admin") == user
    }) && account.as_ref().is_some_and(|account| {
        account["generation"] == identity["generation"]
            && account["is_approved"] == true
            && access::Principal::from_account(account)
                .is_ok_and(|p| p.active() && p.administrator())
    })
}
pub(super) async fn read(
    State(app): State<App>,
    axum::Extension(identity): axum::Extension<Value>,
    Query(args): Query<Args>,
) -> std::result::Result<Response, ApiError> {
    let (since, limit, wait) = bounds(&args)?;
    let _connection = Connection::acquire(&identity)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(wait);
    loop {
        if !valid(&app, &identity).await {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Event credential revoked or expired".into(),
            ));
        }
        let page = app.store.notification_page(since, limit).await?;
        if page["error"] == "cursor_expired" {
            return Ok((StatusCode::GONE, Json(page)).into_response());
        }
        if !page["events"].as_array().unwrap().is_empty() || tokio::time::Instant::now() >= deadline
        {
            if !valid(&app, &identity).await {
                return Err(ApiError(
                    StatusCode::UNAUTHORIZED,
                    "Event credential revoked or expired".into(),
                ));
            }
            return Ok(Json(page).into_response());
        }
        tokio::select! {
            _=app.stop.cancelled()=>return Err(ApiError(StatusCode::SERVICE_UNAVAILABLE,"Event service stopping".into())),
            _=tokio::time::sleep_until(deadline.min(tokio::time::Instant::now()+Duration::from_millis(500)))=>{}
        }
    }
}
pub(super) async fn stream(
    State(app): State<App>,
    axum::Extension(identity): axum::Extension<Value>,
    headers: HeaderMap,
    Query(args): Query<Args>,
) -> std::result::Result<Response, ApiError> {
    let (mut since, limit, _) = bounds(&args)?;
    if let Some(header) = headers.get("last-event-id") {
        let value = header
            .to_str()
            .ok()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|v| *v >= 0)
            .ok_or_else(|| bad("Invalid Last-Event-ID"))?;
        since = since.max(value);
    }
    let connection = Connection::acquire(&identity)?;
    if !valid(&app, &identity).await {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "Event credential revoked or expired".into(),
        ));
    }
    let initial = app.store.notification_page(since, limit).await?;
    if initial["error"] == "cursor_expired" {
        return Ok((StatusCode::GONE, Json(initial)).into_response());
    }
    let queue: VecDeque<Value> = initial["events"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .collect();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3600);
    let stream = futures_util::stream::unfold(
        (app, identity, since, queue, connection, deadline, false),
        move |(app, identity, mut cursor, mut queue, connection, deadline, closed)| async move {
            if closed {
                return None;
            }
            loop {
                if app.stop.is_cancelled()
                    || tokio::time::Instant::now() >= deadline
                    || !valid(&app, &identity).await
                {
                    return None;
                }
                if let Some(row) = queue.pop_front() {
                    cursor = row["seq"].as_i64().unwrap();
                    let event = Event::default()
                        .id(cursor.to_string())
                        .event(row["type"].as_str().unwrap())
                        .data(row.to_string());
                    return Some((
                        Ok::<Event, Infallible>(event),
                        (app, identity, cursor, queue, connection, deadline, false),
                    ));
                }
                let page = app.store.notification_page(cursor, limit).await.ok()?;
                if page["error"] == "cursor_expired" {
                    let event = Event::default()
                        .event("cursor_expired")
                        .data(page.to_string());
                    return Some((
                        Ok(event),
                        (app, identity, cursor, queue, connection, deadline, true),
                    ));
                }
                queue = page["events"].as_array()?.iter().cloned().collect();
                if queue.is_empty() {
                    tokio::select! {_=app.stop.cancelled()=>return None,
                    _=tokio::time::sleep(Duration::from_millis(500))=>{}}
                }
            }
        },
    );
    Ok(Sse::new(stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response())
}

// One API-owned relay; run emission still belongs to the independent loop's transactions.
// Store methods are the only permitted route to user state.
pub(crate) async fn service(app: App) {
    let mut last_pruned = 0;
    loop {
        if app.stop.is_cancelled() {
            break;
        }
        match app.store.all_user_ids().await {
            Ok(ids) => {
                for id in ids {
                    if app.stop.is_cancelled() {
                        break;
                    }
                    let result = async {
                        let store = app.store.for_user(&id).await?;
                        store.relay_notifications().await?;
                        Ok::<(), anyhow::Error>(())
                    }
                    .await;
                    if result.is_err() {
                        tracing::error!("Notification relay deferred");
                    }
                }
            }
            Err(_) => tracing::error!("Notification owner scan deferred"),
        }
        if now().saturating_sub(last_pruned) >= 60 {
            if app.store.prune_notifications(now() as i64).await.is_err() {
                tracing::error!("Notification retention deferred");
            }
            last_pruned = now();
        }
        tokio::select! {_=app.stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_secs(1))=>{}}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    async fn credential(app: &App) -> (String, Value) {
        let token = uuid();
        let key = format!("auth:{}", digest(&json!(token)));
        let account = app.store.account_by_id("admin").await.unwrap().unwrap();
        let doc = json!({"user_id":"admin","generation":account["generation"],"expires":now()+3600,"csrf":"fixture"});
        app.store.put(&key, &doc).await.unwrap();
        let mut identity = doc;
        identity["_auth_key"] = json!(key);
        identity["_user_id"] = json!("admin");
        (format!("v3_session={token}"), identity)
    }
    #[tokio::test]
    async fn outbox_admin_auth_bounds_backfill_and_stream() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (cookie, identity) = credential(&app).await;
        let route = crate::api::router(app.clone());
        let response = route
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/events?wait=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        for uri in [
            "/api/v3/admin/events?since=-1",
            "/api/v3/admin/events?wait=31",
            "/api/v3/admin/events?limit=201",
        ] {
            let response = route
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("cookie", &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
        app.store
            .put("session:s", &json!({"id":"s","agent_id":"shared_default"}))
            .await
            .unwrap();
        let mut tx = app.store.pool.begin().await.unwrap();
        app.store
            .stage_notification(&mut tx, "created:s", "session.created", "s", None, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        app.store.relay_notifications().await.unwrap();
        let response = route
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/events?since=0&wait=0")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let page: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(page["events"][0]["type"], "session.created");
        let response = route
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/events/stream?since=0")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let frame = response
            .into_body()
            .frame()
            .await
            .unwrap()
            .unwrap()
            .into_data()
            .unwrap();
        assert!(String::from_utf8(frame.to_vec())
            .unwrap()
            .contains("event: session.created"));
        app.store
            .prune_notifications(now() as i64 + crate::store::notifications::RETENTION_SECONDS + 1)
            .await
            .unwrap();
        let response = route
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/events?since=0&wait=0")
                    .header("cookie", &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::GONE);
        app.store
            .delete(identity["_auth_key"].as_str().unwrap())
            .await
            .unwrap();
        assert!(!valid(&app, &identity).await);
    }
    #[tokio::test]
    async fn connection_caps_release_and_nonadmin_rejection() {
        let identity = json!({"_auth_key":uuid(),"_user_id":uuid()});
        let mut connections = Vec::new();
        for _ in 0..4 {
            connections.push(Connection::acquire(&identity).unwrap());
        }
        assert!(matches!(
            Connection::acquire(&identity),
            Err(ApiError(StatusCode::TOO_MANY_REQUESTS, _))
        ));
        connections.pop();
        assert!(Connection::acquire(&identity).is_ok());
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (cookie, identity) = credential(&app).await;
        let mut account = app.store.account_by_id("admin").await.unwrap().unwrap();
        account["role"] = json!("user");
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id='admin'")
            .bind(app.store.seal(&account).unwrap())
            .execute(&app.store.app_pool)
            .await
            .unwrap();
        assert!(!valid(&app, &identity).await);
        let response = crate::api::router(app)
            .oneshot(
                Request::builder()
                    .uri("/api/v3/admin/events?wait=0")
                    .header("cookie", cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn event_reader_scope_and_live_revocation() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (_cookie, identity) = credential(&app).await;
        let reader = issue_reader(
            State(app.clone()),
            axum::Extension(identity),
            Json(ReaderBody { expires_in: 3600 }),
        )
        .await
        .unwrap()
        .0;
        let bearer = format!("Bearer {}", reader["token"].as_str().unwrap());
        let route = crate::api::router(app.clone());
        let request = |uri: &str| {
            Request::builder()
                .uri(uri)
                .header("authorization", &bearer)
                .body(Body::empty())
                .unwrap()
        };
        let response = route
            .clone()
            .oneshot(request("/api/v3/admin/events?wait=0"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = route
            .clone()
            .oneshot(request("/api/v3/sessions"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let _ = revoke_reader(
            State(app.clone()),
            Path(reader["id"].as_str().unwrap().to_owned()),
        )
        .await
        .unwrap();
        let response = route
            .oneshot(request("/api/v3/admin/events?wait=0"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    #[tokio::test]
    async fn long_poll_wakes_and_disconnect_releases_capacity() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let (_cookie, identity) = credential(&app).await;
        let polling = tokio::spawn(read(
            State(app.clone()),
            axum::Extension(identity.clone()),
            Query(Args {
                since: Some(0),
                limit: None,
                wait: Some(3),
            }),
        ));
        tokio::time::sleep(Duration::from_millis(100)).await;
        app.store
            .put("session:s", &json!({"id":"s","agent_id":"shared_default"}))
            .await
            .unwrap();
        let mut tx = app.store.pool.begin().await.unwrap();
        app.store
            .stage_notification(&mut tx, "created:s", "session.created", "s", None, None)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        app.store.relay_notifications().await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), polling)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response = stream(
            State(app.clone()),
            axum::Extension(identity.clone()),
            HeaderMap::new(),
            Query(Args {
                since: Some(1),
                limit: None,
                wait: None,
            }),
        )
        .await
        .unwrap();
        let mut connections = Vec::new();
        for _ in 0..3 {
            connections.push(Connection::acquire(&identity).unwrap());
        }
        assert!(Connection::acquire(&identity).is_err());
        drop(response);
        assert!(Connection::acquire(&identity).is_ok());
    }
    #[tokio::test]
    async fn emission_follows_committed_lifecycle_and_terminal_publication() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::create_session(
            State(app.clone()),
            Json(json!({"client_request_id":"notification-lifecycle"})),
        )
        .await
        .unwrap()
        .0["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let body =
            json!({"client_message_id":"notification-message","text":"private fixture text"});
        let result = crate::send(State(app.clone()), Path(sid.clone()), Json(body.clone()))
            .await
            .unwrap();
        let _ = crate::send(State(app.clone()), Path(sid.clone()), Json(body))
            .await
            .unwrap();
        let rid = result.1 .0["run_id"].as_str().unwrap().to_owned();
        let mut run = app.run(&rid).await.unwrap();
        assert!(app
            .store
            .claim_run(&mut run, "fixture", "generation-notification")
            .await
            .unwrap());
        run["status"] = json!("waiting_approval");
        run["pending"] = json!({"id":"approval-1","state":"waiting"});
        app.store
            .save_run(&mut run, "approval_required", &json!({"secret":"excluded"}))
            .await
            .unwrap();
        app.store
            .save_run(&mut run, "approval_required", &json!({"secret":"excluded"}))
            .await
            .unwrap();
        run["status"] = json!("queued");
        app.store
            .save_run_override(&mut run, "run_status", &json!({}))
            .await
            .unwrap();
        assert!(app
            .store
            .claim_run(&mut run, "fixture", "generation-resume")
            .await
            .unwrap());
        let policy = crate::workflow::Policy::resolve(None).unwrap();
        let output = vec![
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"final fixture reply"}]}),
        ];
        crate::publication::terminal(
            &app.store,
            &mut run,
            &output,
            "final fixture reply",
            &policy,
        )
        .await
        .unwrap();
        crate::publication::terminal(
            &app.store,
            &mut run,
            &output,
            "final fixture reply",
            &policy,
        )
        .await
        .unwrap();
        app.store.relay_notifications().await.unwrap();
        let page = app.store.notification_page(0, 200).await.unwrap();
        let kinds: Vec<&str> = page["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "session.created",
                "message.received",
                "run.started",
                "run.waiting_approval",
                "run.completed"
            ]
        );
        assert!(!page.to_string().contains("private fixture text"));
        assert!(!page.to_string().contains("secret"));
        let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(
            chat.iter().filter(|row| row["role"] == "assistant").count(),
            1
        );

        let result = crate::send(
            State(app.clone()),
            Path(sid.clone()),
            Json(
                json!({"client_message_id":"notification-failure","text":"private failure input"}),
            ),
        )
        .await
        .unwrap();
        let failed_id = result.1 .0["run_id"].as_str().unwrap().to_owned();
        let mut failed = app.run(&failed_id).await.unwrap();
        assert!(app
            .store
            .claim_run(&mut failed, "fixture", "generation-failure")
            .await
            .unwrap());
        failed["status"] = json!("failed");
        failed["error"] = json!("private provider error");
        app.store
            .save_run(&mut failed, "run_status", &json!({}))
            .await
            .unwrap();
        app.store
            .save_run_override(&mut failed, "run_status", &json!({}))
            .await
            .unwrap();
        app.store.relay_notifications().await.unwrap();
        let page = app.store.notification_page(0, 200).await.unwrap();
        assert_eq!(
            page["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["type"] == "run.failed" && row["run_id"] == failed_id)
                .count(),
            1
        );
        assert!(!page.to_string().contains("private provider error"));
        let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(
            chat.iter()
                .filter(|row| row["role"] == "assistant" && row["run_id"] == failed_id)
                .count(),
            0
        );
    }
}
