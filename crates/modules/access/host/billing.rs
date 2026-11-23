//! The outgoing connection to the private billing service (plus tier spec 3.9 and 10.4; wire
//! protocol `work/plus-tier/billing-protocol.md` v1). Unix server builds only.
//!
//! The service owns the socket file; the API process connects out to it when
//! `config/access/billing.json` names one, checks the peer's system account, and keeps one
//! connection, reconnecting with a back-off when it drops. Grants, the catalog switch, plan
//! summaries and notices arrive on it and go through the account writer and the owner's
//! own database; usage rows and link requests leave on it. With no path, or the service down,
//! nothing changes: accounts keep their tiers, grant ends still come from `account_due` rows.
use crate::access::billing::{self as rules, MAX_LINE, PROTOCOL};
use crate::App;
use anyhow::{anyhow, bail, Result};
use axum::{extract::State, routing::get, Extension, Json, Router};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};
use webagent_accounts_host::AccountsStore;

use crate::phase3_api::PAID_END_DUE;
/// Longest back-off between connection attempts.
const MAX_BACKOFF: Duration = Duration::from_secs(60);
/// How often the connection looks for new usage rows to hint (one indexed read).
const USAGE_LOOK: Duration = Duration::from_secs(60);
/// Applied references are kept this long (seconds).
const REFERENCES_KEPT: i64 = 400 * 86_400;
/// Owner tiers sent per message after a handshake.
const TIER_BATCH: usize = 500;

/// The open connection, when there is one.
struct Link {
    lines: mpsc::Sender<String>,
    pending: Mutex<HashMap<String, oneshot::Sender<Value>>>,
    offers: Value,
    next: std::sync::atomic::AtomicU64,
}

fn link() -> &'static Mutex<Option<std::sync::Arc<Link>>> {
    static LINK: OnceLock<Mutex<Option<std::sync::Arc<Link>>>> = OnceLock::new();
    LINK.get_or_init(Default::default)
}

fn current() -> Option<std::sync::Arc<Link>> {
    link()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// The live settings; a missing or invalid file turns the connection off (and says why).
fn config() -> rules::Config {
    match std::fs::read(rules::LIVE) {
        Ok(bytes) => rules::Config::parse(&bytes).unwrap_or_else(|error| {
            tracing::warn!(%error, "Billing settings invalid; billing connection off");
            rules::Config::default()
        }),
        Err(_) => rules::Config::default(),
    }
}

/// Keep the billing connection while the API runs. Returns at once when no socket is set.
pub(crate) async fn service(app: App) {
    let settings = config();
    let (Some(path), Some(uid)) = (settings.socket_path.clone(), settings.service_uid) else {
        return;
    };
    let timeout = Duration::from_secs(settings.request_timeout_seconds);
    let mut backoff = Duration::from_secs(1);
    loop {
        let started = std::time::Instant::now();
        if let Err(error) = connect_once(&app, &path, uid, timeout).await {
            tracing::warn!(%error, "Billing connection ended");
        }
        if started.elapsed() >= MAX_BACKOFF {
            backoff = Duration::from_secs(1);
        }
        tokio::select! {
            () = app.stop.cancelled() => return,
            () = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

async fn connect_once(app: &App, path: &str, uid: u32, timeout: Duration) -> Result<()> {
    let stream = UnixStream::connect(path).await?;
    let peer = stream.peer_cred()?.uid();
    if peer != uid {
        bail!("billing peer refused (uid {peer})");
    }
    run_session(app, stream, timeout).await
}

/// Read one line of at most [`MAX_LINE`] bytes; `None` when the peer closed.
async fn read_line<R: AsyncBufReadExt + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    let mut buffer = Vec::new();
    let read = reader
        .take(u64::try_from(MAX_LINE).unwrap_or(u64::MAX))
        .read_until(b'\n', &mut buffer)
        .await?;
    if read == 0 {
        return Ok(None);
    }
    if buffer.last() != Some(&b'\n') {
        bail!("billing message too long or cut off");
    }
    let message: Value =
        serde_json::from_slice(&buffer).map_err(|_| anyhow!("billing message is not JSON"))?;
    if !message.is_object() || message["v"] != 1 {
        bail!("billing message without version 1");
    }
    Ok(Some(message))
}

/// One connection, from the handshake until either side closes it.
pub(crate) async fn run_session(app: &App, stream: UnixStream, timeout: Duration) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    write
        .write_all(
            b"{\"v\":1,\"id\":\"h1\",\"type\":\"hello\",\"app\":\"webagent\",\"protocol\":1}\n",
        )
        .await?;
    let hello = tokio::time::timeout(timeout, read_line(&mut reader))
        .await
        .map_err(|_| anyhow!("billing handshake timed out"))??
        .ok_or_else(|| anyhow!("billing service closed during the handshake"))?;
    if hello["re"] != "h1" || hello["ok"] != true || hello["protocol"] != PROTOCOL {
        bail!("billing handshake refused");
    }
    let offers: Vec<Value> = hello["offers"]
        .as_array()
        .into_iter()
        .flatten()
        .take(8)
        .filter(|offer| {
            offer["what"].as_str().is_some_and(|what| {
                (1..=32).contains(&what.len())
                    && what
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
            }) && offer["label"]
                .as_str()
                .is_some_and(|label| !label.is_empty() && label.chars().count() <= 40)
        })
        .map(|offer| json!({"what":offer["what"],"label":offer["label"]}))
        .collect();
    let (lines, mut outgoing) = mpsc::channel::<String>(256);
    let session = std::sync::Arc::new(Link {
        lines,
        pending: Mutex::new(HashMap::new()),
        offers: json!(offers),
        next: std::sync::atomic::AtomicU64::new(1),
    });
    *link().lock().unwrap_or_else(|error| error.into_inner()) = Some(session.clone());
    let writer = tokio::spawn(async move {
        while let Some(line) = outgoing.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
    });
    let usage = tokio::spawn(usage_hints(app.clone(), session.clone()));
    let result = async {
        send_owner_tiers(app, &session).await?;
        while let Some(message) = read_line(&mut reader).await? {
            if let Some(id) = message["re"].as_str() {
                let waiting = session
                    .pending
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .remove(id);
                if let Some(waiting) = waiting {
                    let _ = waiting.send(message);
                }
                continue;
            }
            let id = message["id"]
                .as_str()
                .filter(|id| rules::valid_token(id, 64));
            let Some(id) = id else {
                bail!("billing request without a valid id");
            };
            let mut reply = handle(app, &session, &message).await;
            reply["v"] = json!(1);
            reply["re"] = json!(id);
            send(&session, &reply).await?;
        }
        Ok(())
    }
    .await;
    *link().lock().unwrap_or_else(|error| error.into_inner()) = None;
    usage.abort();
    writer.abort();
    result
}

async fn send(session: &Link, message: &Value) -> Result<()> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');
    session
        .lines
        .send(line)
        .await
        .map_err(|_| anyhow!("billing connection closed"))
}

/// Send a request on the open connection and wait for its reply.
async fn request(mut message: Value) -> Result<Value> {
    let session = current().ok_or_else(|| anyhow!("Billing unavailable"))?;
    let id = format!(
        "a{}",
        session
            .next
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    let (done, answer) = oneshot::channel();
    session
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .insert(id.clone(), done);
    message["v"] = json!(1);
    message["id"] = json!(id);
    send(&session, &message).await?;
    let timeout = Duration::from_secs(config().request_timeout_seconds);
    let reply = tokio::time::timeout(timeout, answer).await;
    session
        .pending
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .remove(&id);
    match reply {
        Ok(Ok(reply)) => Ok(reply),
        _ => bail!("Billing unavailable"),
    }
}

/// Tell the service usage rows are waiting: once a minute at most, only when the feed grew.
async fn usage_hints(app: App, session: std::sync::Arc<Link>) {
    let mut last: Option<Option<i64>> = None;
    loop {
        let newest: Result<Option<i64>, sqlx::Error> =
            sqlx::query_scalar("SELECT MAX(seq) FROM usage_turns")
                .fetch_one(&app.store.app_pool)
                .await;
        if let Ok(newest) = newest {
            if last.is_some_and(|previous| newest > previous) {
                let message =
                    json!({"v":1,"id":format!("u{}", newest.unwrap_or(0)),"type":"usage_ready"});
                if send(&session, &message).await.is_err() {
                    return;
                }
            }
            last = Some(newest);
        }
        tokio::time::sleep(USAGE_LOOK).await;
    }
}

/// The owner-tier message for one account, from its stored tier sources.
async fn owner_tier(app: &App, user: &str) -> Result<Option<Value>> {
    let Some(account) = app.store.account_by_id(user).await? else {
        return Ok(None);
    };
    let sources = crate::access::repository::TierSources::of(&account)?;
    Ok(Some(json!({"user":user,"tier":account["tier_id"],
        "grant_until":sources.admin_until,"paid_until":sources.paid.as_ref().map(|paid| paid.until),
        "source":sources.source(crate::now())})))
}

/// After a handshake: every account above Free, from the plain `account_tiers` index.
async fn send_owner_tiers(app: &App, session: &Link) -> Result<()> {
    let now = crate::now();
    let accounts: Vec<Value> = app
        .store
        .accounts_above_free()
        .await?
        .into_iter()
        .map(|row| {
            let source = crate::access::repository::source_of(
                &row.admin_tier,
                row.paid_until.is_some_and(|until| until > now),
            );
            json!({"user":row.user_id,"tier":row.tier_id,"grant_until":row.grant_until,
                "paid_until":row.paid_until,"source":source})
        })
        .collect();
    let batches: Vec<&[Value]> = if accounts.is_empty() {
        vec![&[]]
    } else {
        accounts.chunks(TIER_BATCH).collect()
    };
    let count = batches.len();
    for (index, batch) in batches.into_iter().enumerate() {
        let message = json!({"v":1,"id":format!("t{index}"),"type":"owner_tiers",
            "accounts":batch,"last":index + 1 == count});
        send(session, &message).await?;
    }
    Ok(())
}

/// Tell the service an account's tier or grants changed (no-op without a connection).
pub(crate) fn tier_changed(app: &App, user: &str) {
    let Some(session) = current() else {
        return;
    };
    let app = app.clone();
    let user = user.to_owned();
    tokio::spawn(async move {
        if let Ok(Some(mut message)) = owner_tier(&app, &user).await {
            message["v"] = json!(1);
            message["id"] = json!(format!(
                "o{}",
                session
                    .next
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            message["type"] = json!("owner_tier");
            let _ = send(&session, &message).await;
        }
    });
}

fn refused(code: &str) -> Value {
    json!({"ok":false,"error":code})
}

/// Answer one request from the service.
async fn handle(app: &App, session: &Link, message: &Value) -> Value {
    let kind = message["type"].as_str().unwrap_or("");
    let result = match kind {
        "read_usage" => read_usage(app, message).await,
        "grant_tier" | "end_grant" | "set_catalog_access" | "add_notice" => {
            once_per_reference(app, session, kind, message).await
        }
        "set_plan_summary" => plan_summary(app, message).await,
        _ => return refused("unknown_type"),
    };
    result.unwrap_or_else(|error| {
        let code = error.to_string();
        let known = [
            "bad_request",
            "unknown_user",
            "tier_not_grantable",
            "bad_until",
            "unknown_kind",
            "reference_conflict",
        ];
        if known.contains(&code.as_str()) {
            refused(&code)
        } else {
            tracing::warn!(kind, error = %code, "Billing request failed");
            refused("internal")
        }
    })
}

fn bad() -> anyhow::Error {
    anyhow!("bad_request")
}

/// The account a request names; it must exist (never created here).
async fn named_user(app: &App, message: &Value) -> Result<String> {
    let user = message["user"]
        .as_str()
        .filter(|user| crate::access::valid_id(user))
        .ok_or_else(bad)?;
    if app.store.account_by_id(user).await?.is_none() {
        bail!("unknown_user");
    }
    Ok(user.to_owned())
}

async fn references_schema(app: &App) -> Result<()> {
    sqlx::query("CREATE TABLE IF NOT EXISTS billing_applied(reference TEXT PRIMARY KEY,kind TEXT NOT NULL,user_id TEXT NOT NULL,digest TEXT NOT NULL,applied_at INTEGER NOT NULL) WITHOUT ROWID")
        .execute(&app.store.app_pool)
        .await?;
    Ok(())
}

/// Grants, the catalog switch and notices apply once per reference: a repeat with the same
/// fields changes nothing; the same reference with other fields is refused.
async fn once_per_reference(
    app: &App,
    session: &Link,
    kind: &str,
    message: &Value,
) -> Result<Value> {
    let reference = message["reference"]
        .as_str()
        .filter(|reference| rules::valid_token(reference, 128))
        .ok_or_else(bad)?;
    let mut fields = message.clone();
    if let Some(object) = fields.as_object_mut() {
        object.remove("id");
        object.remove("v");
    }
    let digest = webagent_configuration::digest(&fields);
    references_schema(app).await?;
    let seen: Option<String> =
        sqlx::query_scalar("SELECT digest FROM billing_applied WHERE reference=?")
            .bind(reference)
            .fetch_optional(&app.store.app_pool)
            .await?;
    match seen {
        Some(seen) if seen == digest => return Ok(json!({"ok":true,"duplicate":true})),
        Some(_) => bail!("reference_conflict"),
        None => {}
    }
    let user = named_user(app, message).await?;
    let reply = match kind {
        "grant_tier" => grant(app, &user, message, reference).await?,
        "end_grant" => end(app, &user, message).await?,
        "set_catalog_access" => {
            let allowed = message["allowed"].as_bool().ok_or_else(bad)?;
            app.store
                .set_catalog_models(&user, Some(allowed))
                .await
                .map_err(|_| anyhow!("bad_request"))?;
            json!({"ok":true,"allowed":allowed})
        }
        _ => {
            let notice = message["kind"].as_str().and_then(rules::notice_text);
            let Some(text) = notice else {
                bail!("unknown_kind");
            };
            let severity = message["severity"]
                .as_str()
                .filter(|severity| matches!(*severity, "info" | "warning" | "critical"))
                .ok_or_else(bad)?;
            let kind = format!("billing_{}", message["kind"].as_str().unwrap_or(""));
            app.store
                .for_user(&user)
                .await?
                .add_notice(&kind, severity, text)
                .await?;
            json!({"ok":true})
        }
    };
    let now = i64::try_from(crate::now()).unwrap_or(i64::MAX);
    sqlx::query("INSERT INTO billing_applied(reference,kind,user_id,digest,applied_at) VALUES(?,?,?,?,?) ON CONFLICT(reference) DO NOTHING")
        .bind(reference)
        .bind(kind)
        .bind(&user)
        .bind(&digest)
        .bind(now)
        .execute(&app.store.app_pool)
        .await?;
    sqlx::query("DELETE FROM billing_applied WHERE applied_at<?")
        .bind(now - REFERENCES_KEPT)
        .execute(&app.store.app_pool)
        .await?;
    if kind != "add_notice" {
        if let Ok(Some(mut update)) = owner_tier(app, &user).await {
            update["v"] = json!(1);
            update["id"] = json!(format!(
                "o{}",
                session
                    .next
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            update["type"] = json!("owner_tier");
            let _ = send(session, &update).await;
        }
    }
    Ok(reply)
}

async fn grant(app: &App, user: &str, message: &Value, reference: &str) -> Result<Value> {
    let tier = message["tier"].as_str().ok_or_else(bad)?;
    let until = message["until"].as_u64().ok_or_else(bad)?;
    let policy = app.store.read_access_policy()?;
    if crate::access::account_ladder::grantable(&policy, tier).is_err() {
        bail!("tier_not_grantable");
    }
    if until <= crate::now() || until > crate::access::account_ladder::LATEST_TIER_END {
        bail!("bad_until");
    }
    // Due row first (only ever brought earlier), then the account, then the exact time.
    let pool = &app.store.app_pool;
    webagent_engine::due::earlier(pool, user, PAID_END_DUE, until).await?;
    let (account, _) = {
        let _guard = app.gate.lock().await;
        app.store
            .apply_paid_grant(user, tier, until, reference)
            .await?
    };
    webagent_engine::due::set(pool, user, PAID_END_DUE, Some(until)).await?;
    Ok(json!({"ok":true,"tier":account["tier_id"],"until":until}))
}

async fn end(app: &App, user: &str, message: &Value) -> Result<Value> {
    let grant_reference = message["grant_reference"]
        .as_str()
        .filter(|reference| rules::valid_token(reference, 128))
        .ok_or_else(bad)?;
    let ended = {
        let _guard = app.gate.lock().await;
        app.store.end_paid_grant(user, grant_reference).await?
    };
    match ended {
        Some((account, _)) => {
            webagent_engine::due::set(&app.store.app_pool, user, PAID_END_DUE, None).await?;
            Ok(json!({"ok":true,"tier":account["tier_id"]}))
        }
        None => Ok(json!({"ok":true,"noop":true})),
    }
}

/// Key of the plan summary in the account's own database.
const SUMMARY_KEY: &str = "billing_plan_summary";

async fn plan_summary(app: &App, message: &Value) -> Result<Value> {
    let user = named_user(app, message).await?;
    let as_of = message["as_of"].as_u64().ok_or_else(bad)?;
    let summary = rules::summary_fields(&message["summary"]).ok_or_else(bad)?;
    let store = app.store.for_user(&user).await?;
    let stored = store.get(SUMMARY_KEY).await?;
    if stored
        .as_ref()
        .and_then(|stored| stored["as_of"].as_u64())
        .is_some_and(|stored| stored > as_of)
    {
        return Ok(json!({"ok":true,"stale":true}));
    }
    store
        .put(SUMMARY_KEY, &json!({"as_of":as_of,"summary":summary}))
        .await?;
    Ok(json!({"ok":true}))
}

/// Usage rows after `after` (at most `limit`); `after` also acknowledges every row up to it.
async fn read_usage(app: &App, message: &Value) -> Result<Value> {
    let after = message["after"].as_u64().ok_or_else(bad)?;
    let limit = message["limit"].as_u64().unwrap_or(500).clamp(1, 500);
    let after = i64::try_from(after).map_err(|_| bad())?;
    let pool = &app.store.app_pool;
    sqlx::query("INSERT INTO billing_state(key,value) VALUES('usage_cursor',?) ON CONFLICT(key) DO UPDATE SET value=MAX(value,excluded.value)")
        .bind(after)
        .execute(pool)
        .await?;
    let oldest: Option<i64> = sqlx::query_scalar("SELECT MIN(seq) FROM usage_turns")
        .fetch_one(pool)
        .await?;
    let rows: Vec<(i64, String, String, String, String, bool, i64, i64, i64, i64, Option<i64>, i64)> = sqlx::query_as(
        "SELECT seq,owner_id,agent_id,requester_id,model,catalog,input_tokens,cached_input_tokens,output_tokens,compute_ms,cost_micros,finished_at FROM usage_turns WHERE seq>? ORDER BY seq LIMIT ?",
    )
    .bind(after)
    .bind(i64::try_from(limit).unwrap_or(500))
    .fetch_all(pool)
    .await?;
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({"seq":row.0,"owner":row.1,"agent":row.2,"requester":row.3,"model":row.4,
                "catalog":row.5,"input_tokens":row.6,"cached_input_tokens":row.7,
                "output_tokens":row.8,"compute_seconds":row.9 as f64 / 1000.0,
                "cost_micros":row.10,"finished_at":row.11})
        })
        .collect();
    Ok(json!({"ok":true,"rows":rows,"gap":oldest.is_some_and(|oldest| oldest > after + 1)}))
}

/// What the Plan panel may show: whether billing is set up and reachable, the purchase offers
/// and the account's last plan summary. People only: no guests, widgets or agent browsers.
async fn summary(State(app): State<App>, Extension(identity): Extension<Value>) -> crate::Api {
    let user = person(&identity)?;
    let configured = config().socket_path.is_some();
    let session = current();
    // The authentication middleware scoped the store to the caller's own database.
    debug_assert_eq!(app.store.current_user_id(), user);
    let stored = app.store.get(SUMMARY_KEY).await?;
    Ok(Json(
        json!({"configured":configured,"connected":session.is_some(),
        "offers":session.map_or(json!([]), |session| session.offers.clone()),
        "summary":if configured { stored.unwrap_or(Value::Null) } else { Value::Null }}),
    ))
}

/// Ask the service for a one-time link to its provider's hosted page, for the caller only.
async fn link_for(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> crate::Api {
    let user = person(&identity)?;
    let what = body["what"].as_str().unwrap_or("");
    let message = if what == "billing_details" {
        json!({"type":"billing_details_link","user":user})
    } else {
        let offered = current().is_some_and(|session| {
            session
                .offers
                .as_array()
                .into_iter()
                .flatten()
                .any(|offer| offer["what"] == what)
        });
        if !offered {
            return Err(crate::ApiError(
                axum::http::StatusCode::BAD_REQUEST,
                "Unknown purchase".into(),
            ));
        }
        json!({"type":"payment_link","user":user,"what":what})
    };
    let unavailable = || {
        crate::ApiError(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Billing unavailable".into(),
        )
    };
    let reply = request(message).await.map_err(|_| unavailable())?;
    if reply["ok"] != true {
        if reply["error"] == "no_billing_account" {
            return Err(crate::ApiError(
                axum::http::StatusCode::NOT_FOUND,
                "No billing details yet".into(),
            ));
        }
        return Err(unavailable());
    }
    let url = reply["url"]
        .as_str()
        .filter(|url| rules::valid_link(url))
        .ok_or_else(unavailable)?;
    Ok(Json(json!({"url":url})))
}

/// The caller as a person with their own account: no guest, widget or agent browser.
fn person(identity: &Value) -> Result<String, crate::ApiError> {
    let forbidden = || {
        crate::ApiError(
            axum::http::StatusCode::FORBIDDEN,
            "Billing is for signed-in people".into(),
        )
    };
    if crate::access::identity::CredentialKind::restricted_document(identity)
        || identity["_role"] == "guest"
        || identity["_credential"] == crate::access::identity::TEST_PERSONA
    {
        return Err(forbidden());
    }
    identity["_user_id"]
        .as_str()
        .filter(|user| !user.is_empty())
        .map(str::to_owned)
        .ok_or_else(forbidden)
}

pub(crate) fn router() -> Router<App> {
    Router::new()
        .route("/billing", get(summary))
        .route("/billing/link", axum::routing::post(link_for))
}

#[cfg(test)]
#[path = "billing_tests.rs"]
mod billing_tests;
