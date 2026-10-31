//! Agent Operator roster endpoints; app role never grants tenant authority.
use super::*;
use axum::Extension;
use serde_json::Map;
use sqlx::Row;
use std::collections::{BTreeMap, HashSet};
async fn operator(app: &App, identity: &Value, agent: &str) -> Result<Store, ApiError> {
    let actor = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Sign in required"))?;
    if !app
        .store
        .agent_authority(actor, agent)
        .await
        .is_ok_and(|r| r["can_manage"] == true)
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent not found".into()));
    }
    Ok(app.store.for_agent(agent).await?)
}
async fn read(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Api {
    let store = operator(&app, &identity, &id).await?;
    if let Some(username) = query.get("username") {
        if username.is_empty() || username.len() > 128 {
            return Err(bad("Exact username required"));
        }
        let account = store
            .account_by_username(username)
            .await?
            .filter(|a| a["role"] != "guest")
            .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Account not found".into()))?;
        return Ok(Json(
            json!({"user":{"user_id":account["user_id"],"username":account["username"]}}),
        ));
    }
    let offset = query
        .get("offset")
        .map(|v| v.parse::<usize>())
        .transpose()
        .map_err(|_| bad("Invalid offset"))?
        .unwrap_or(0);
    let members = store.agent_memberships(&id).await?;
    let total = members.len();
    let mut page = Vec::new();
    for mut member in members.into_iter().skip(offset).take(100) {
        let user = member["user_id"].as_str().unwrap_or("");
        let effective = store
            .access_repository()
            .resolved_membership(user, &id)
            .await?;
        member["effective_level_id"] = effective
            .as_ref()
            .map(|v| v["capability_level_id"].clone())
            .unwrap_or(Value::Null);
        member["access_origin"] = effective
            .as_ref()
            .map(|v| v["origin"].clone())
            .unwrap_or_else(|| json!("none"));
        if let Some(user) = member["user_id"].as_str().map(str::to_owned) {
            if let Some(account) = store.account_by_id(&user).await? {
                member["username"] = account["username"].clone();
                let (sessions, messages, profile) = activity(&app, &user, &id).await?;
                member["session_count"] = json!(sessions);
                member["message_count"] = json!(messages);
                for key in ["last_login_at", "display_name", "source_channel"] {
                    member[key] = profile[key].clone();
                }
            }
        }
        page.push(member);
    }
    let mut pending = Vec::new();
    for (user, requested_at) in access_requests(&store, &id).await? {
        let Some(account) = store.account_by_id(&user).await? else {
            continue;
        };
        let profile = app.store.user_profile(&user).await?.unwrap_or(Value::Null);
        let revision = store
            .agent_membership(&user, &id)
            .await?
            .and_then(|membership| membership["revision"].as_u64())
            .unwrap_or(0);
        pending.push(json!({"user_id":user,"username":account["username"],
            "display_name":profile["display_name"],"requested_at":requested_at,"revision":revision}));
    }
    Ok(Json(
        json!({"members":page,"access_requests":pending,"next_offset":if offset+100<total {Some(offset+100)}else{None},
        "capability_levels":store.capability_levels(&id).await?,"owner_id":store.agent_authority(identity["_user_id"].as_str().unwrap(),&id).await?["owner_id"]}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberUpdate {
    user_id: String,
    level_id: Option<String>,
    #[serde(default)]
    blocked: bool,
    expected_revision: u64,
}
async fn update(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<MemberUpdate>,
) -> Api {
    let _guard = app.gate.lock().await;
    let store = operator(&app, &identity, &id).await?;
    let member = store
        .access_repository()
        .manage_agent_member(
            identity["_user_id"].as_str().unwrap(),
            &body.user_id,
            &id,
            body.level_id.as_deref(),
            body.blocked,
            body.expected_revision,
        )
        .await
        .map_err(|_| {
            conflict("Membership changed or appointment rejected; reload before retrying")
        })?;
    drop_access_request(&store, &id, &body.user_id).await?;
    crate::phase3_api::quiesce_user(&app, &body.user_id, Some(&id)).await?;
    Ok(Json(json!({"member":member})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LadderUpdate {
    expected_revision: u64,
    ladder: crate::capabilities::CapabilityLevels,
    #[serde(default)]
    confirm_expansion: bool,
    #[serde(default)]
    reassignments: BTreeMap<String, String>,
}
async fn ladder(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(mut body): Json<LadderUpdate>,
) -> Api {
    let _guard = app.gate.lock().await;
    let store = operator(&app, &identity, &id).await?;
    body.ladder
        .validate()
        .map_err(|_| bad("Invalid capability ladder"))?;
    let previous = store.capability_levels(&id).await?;
    for (level_id, next) in &body.ladder.levels {
        let old = previous
            .levels
            .get(level_id)
            .map(|v| serde_json::to_value(v))
            .transpose()
            .map_err(anyhow::Error::from)?
            .unwrap_or(json!({}));
        super::ability_admin::grant_ceiling(
            &app,
            &identity,
            &serde_json::to_value(&next.ability_permissions).map_err(anyhow::Error::from)?,
            &serde_json::to_value(&next.tool_permissions).map_err(anyhow::Error::from)?,
            &old["ability_permissions"],
            &old["tool_permissions"],
            false,
        )
        .await?;
    }
    let mut tx = store
        .agent_pool
        .begin()
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("UPDATE agents SET revision=revision WHERE id=?")
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(&id)
        .fetch_one(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    let mut metadata = store.unseal_agent(&raw)?;
    let before = crate::capabilities::CapabilityLevels::from_metadata(&metadata)?;
    if before.revision != body.expected_revision || body.ladder.revision != body.expected_revision {
        return Err(conflict("Capability ladder changed"));
    }
    let actor = identity["_user_id"].as_str().unwrap();
    let owner = metadata["owner_id"]
        .as_str()
        .unwrap_or(if id == "shared_default" { "admin" } else { "" });
    let actor_blob: Option<Vec<u8>> =
        sqlx::query_scalar("SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
            .bind(actor)
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    if actor != owner
        && !actor_blob
            .as_ref()
            .map(|b| store.unseal_agent(b))
            .transpose()?
            .is_some_and(|v| v["relationship"] == "member" && v["agent_administrator"] == true)
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Agent Operator required".into(),
        ));
    }
    let expansion = before.order != body.ladder.order
        && before
            .order
            .iter()
            .filter(|level| body.ladder.levels.contains_key(*level))
            .any(|level| {
                let old = before.effective(level).ok();
                let next = body.ladder.effective(level).ok();
                old.zip(next).is_some_and(|(a, b)| a != b)
            });
    if expansion && !body.confirm_expansion {
        return Err(conflict(
            "Reordering changes effective access; preview and confirm",
        ));
    }
    let rows = sqlx::query("SELECT user_id,value FROM agent_memberships WHERE agent_id=?")
        .bind(&id)
        .fetch_all(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    for row in rows {
        let mut membership = store.unseal_agent(&row.get::<Vec<u8>, _>("value"))?;
        let level = crate::capabilities::membership_level(
            Some(&membership),
            serde_json::from_value(membership["relationship"].clone())
                .map_err(anyhow::Error::from)?,
        )?;
        if body.ladder.levels.contains_key(&level) {
            continue;
        }
        let replacement = body
            .reassignments
            .get(&level)
            .filter(|r| r.as_str() != "operator" && body.ladder.levels.contains_key(*r))
            .ok_or_else(|| bad("Choose a replacement for every removed assigned level"))?;
        membership["capability_level_id"] = json!(replacement);
        if membership["relationship"] != "revoked" && membership["origin"] != "removed" {
            membership["relationship"] = json!(if replacement == "visitor" {
                "caller"
            } else {
                "member"
            });
        }
        membership["agent_administrator"] = json!(false);
        membership["revision"] = json!(membership["revision"]
            .as_u64()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| bad("Membership revision overflow"))?);
        sqlx::query("UPDATE agent_memberships SET value=? WHERE user_id=? AND agent_id=?")
            .bind(store.seal_agent(&membership)?)
            .bind(row.get::<String, _>("user_id"))
            .bind(&id)
            .execute(&mut *tx)
            .await
            .map_err(anyhow::Error::from)?;
    }
    body.ladder.revision = body
        .expected_revision
        .checked_add(1)
        .ok_or_else(|| bad("Ladder revision overflow"))?;
    metadata
        .as_object_mut()
        .ok_or_else(|| anyhow!("Invalid agent metadata"))?
        .remove("ability_permissions");
    metadata.as_object_mut().unwrap().remove("tool_permissions");
    metadata["capability_levels"] =
        serde_json::to_value(&body.ladder).map_err(anyhow::Error::from)?;
    sqlx::query("UPDATE agents SET metadata=?,revision=revision+1 WHERE id=?")
        .bind(store.seal_agent(&metadata)?)
        .bind(&id)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
        .bind(format!("agent:{id}:ladder_audit:{}", body.ladder.revision))
        .bind(store.seal_agent(
            &json!({"actor_id":actor,"revision":body.ladder.revision,"order":body.ladder.order}),
        )?)
        .execute(&mut *tx)
        .await
        .map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    crate::phase3_api::quiesce_agent(&app, &id).await?;
    Ok(Json(json!({"capability_levels":body.ladder})))
}
async fn summary(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    let user = identity["_user_id"].as_str().unwrap_or("");
    if !app
        .store
        .live_access(user, &id)
        .await
        .is_ok_and(|a| a.admitted())
    {
        return Err(ApiError(StatusCode::NOT_FOUND, "Agent unavailable".into()));
    }
    let store = app.store.for_agent(&id).await?;
    Ok(Json(
        json!({"conversation_privacy":store.access_repository().member_access(&id).await?.conversation_privacy,
        "tools_notice":app.store.capability_levels(&id).await?.tools_notice()}),
    ))
}
/// This agent's public sessions (active and archived) for the user `view` belongs to.
async fn agent_sessions(view: &Store, agent: &str) -> anyhow::Result<HashSet<String>> {
    let mut sessions = HashSet::new();
    for archived in [false, true] {
        for summary in view.ordered_sessions_status(archived).await? {
            if summary["agent_id"] == agent {
                if let Some(id) = summary["id"].as_str() {
                    sessions.insert(id.to_owned());
                }
            }
        }
    }
    Ok(sessions)
}

/// Messages in `sessions`; with `user_turns_since`, only user turns from that time.
async fn session_messages(
    view: &Store,
    sessions: &HashSet<String>,
    user_turns_since: Option<i64>,
) -> anyhow::Result<i64> {
    let counts: Vec<(String, i64)> = sqlx::query_as(
        "SELECT session_id,COUNT(*) FROM chat WHERE (?1 IS NULL OR (role='user' AND created_at>=?1)) GROUP BY session_id",
    )
    .bind(user_turns_since)
    .fetch_all(&view.pool)
    .await?;
    Ok(counts
        .into_iter()
        .filter(|(session, _)| sessions.contains(session))
        .map(|(_, count)| count)
        .sum())
}

/// This agent's public sessions and messages for one user, plus the account
/// profile (last login, display name, sign-up channel); other agents' activity
/// is excluded.
async fn activity(app: &App, user: &str, agent: &str) -> anyhow::Result<(usize, i64, Value)> {
    let view = app.store.for_user(user).await?;
    let sessions = agent_sessions(&view, agent).await?;
    let messages = session_messages(&view, &sessions, None).await?;
    let profile = app.store.user_profile(user).await?.unwrap_or(Value::Null);
    Ok((sessions.len(), messages, profile))
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// Refuses a new user message once the user's level cap for this agent is
/// reached over the last 24 hours or 30 days. Operators are never limited.
/// shortcut: counts turns in the user's own sessions only, not turns they post
/// in other users' shared sessions; add an author index if that matters.
pub(crate) async fn enforce_turn_limits(
    app: &App,
    user: &str,
    agent: &str,
    level: &crate::capabilities::CapabilityLevel,
) -> Result<(), ApiError> {
    let windows = [
        (level.daily_turns, 86_400, "24 hours"),
        (level.monthly_turns, 30 * 86_400, "30 days"),
    ];
    if level.id == "operator" || windows.iter().all(|(limit, _, _)| limit.is_none()) {
        return Ok(());
    }
    let view = app.store.for_user(user).await?;
    let sessions = agent_sessions(&view, agent).await?;
    for (limit, span, label) in windows {
        let Some(limit) = limit else { continue };
        let used = session_messages(&view, &sessions, Some(unix_now() - span)).await?;
        if used >= i64::from(limit) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                format!(
                    "Message limit reached: {} allows {limit} messages per {label} with this agent. Try again later.",
                    level.name
                ),
            ));
        }
    }
    Ok(())
}

/// Pending access requests (user id -> requested_at) live in the agent store,
/// so recording one never bumps the agent revision.
const ACCESS_REQUESTS_MAX: usize = 200;
fn requests_key(agent: &str) -> String {
    format!("agent:{agent}:access_requests")
}
async fn access_requests(store: &Store, agent: &str) -> anyhow::Result<Map<String, Value>> {
    Ok(store
        .get(&requests_key(agent))
        .await?
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default())
}
async fn drop_access_request(store: &Store, agent: &str, user: &str) -> anyhow::Result<()> {
    let mut requests = access_requests(store, agent).await?;
    if requests.remove(user).is_some() {
        store
            .put(&requests_key(agent), &Value::Object(requests))
            .await?;
    }
    Ok(())
}

/// The caller's own request state for an agent they cannot use yet.
async fn access_request_status(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    let user = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Sign in required"))?;
    let store = app
        .store
        .for_agent(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Agent unavailable".into()))?;
    let requested = access_requests(&store, &id).await?.contains_key(user);
    Ok(Json(json!({"requested":requested})))
}

/// A signed-in app account without any membership asks the agent's Operators
/// for access; guests, members and blocked users cannot request.
async fn request_access(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
) -> Api {
    let user = identity["_user_id"]
        .as_str()
        .ok_or_else(|| bad("Sign in required"))?;
    if !app
        .store
        .account_by_id(user)
        .await?
        .is_some_and(|account| account["role"] != "guest")
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Sign in with an app account to request access".into(),
        ));
    }
    let _guard = app.gate.lock().await;
    let store = app
        .store
        .for_agent(&id)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Agent unavailable".into()))?;
    if store
        .access_repository()
        .resolved_membership(user, &id)
        .await?
        .is_some()
    {
        return Err(conflict(
            "Your access to this agent is already set by its Operators",
        ));
    }
    let mut requests = access_requests(&store, &id).await?;
    if !requests.contains_key(user) {
        if requests.len() >= ACCESS_REQUESTS_MAX {
            return Err(conflict(
                "Too many pending access requests; try again later",
            ));
        }
        requests.insert(user.to_owned(), json!(unix_now()));
        store
            .put(&requests_key(&id), &Value::Object(requests))
            .await?;
    }
    Ok(Json(json!({"requested":true})))
}

/// Operator declines a pending request without changing any membership.
async fn dismiss_access_request(
    State(app): State<App>,
    Extension(identity): Extension<Value>,
    Path((id, user)): Path<(String, String)>,
) -> Api {
    let _guard = app.gate.lock().await;
    let store = operator(&app, &identity, &id).await?;
    drop_access_request(&store, &id, &user).await?;
    Ok(Json(json!({"requested":false})))
}

pub fn router() -> Router<App> {
    Router::new()
        .route("/agents/{id}/members", get(read).put(update))
        .route("/agents/{id}/capability-ladder", put(ladder))
        .route("/agents/{id}/access-summary", get(summary))
        .route(
            "/agents/{id}/access-request",
            get(access_request_status).post(request_access),
        )
        .route(
            "/agents/{id}/access-requests/{user}",
            axum::routing::delete(dismiss_access_request),
        )
}

#[cfg(test)]
mod reassignment_tests {
    use super::*;
    #[tokio::test]
    async fn turn_limits_count_recent_user_turns_and_spare_operators() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let agent = "shared_default";
        let account = app
            .store
            .create_user_account("limited", "fixture-password-hash")
            .await
            .unwrap();
        let user = account["user_id"].as_str().unwrap().to_owned();
        let view = app.store.for_user(&user).await.unwrap();
        view.put(
            "session:limited",
            &json!({"id":"limited","title":"t","agent_id":agent}),
        )
        .await
        .unwrap();
        // Two recent user turns, one assistant reply and one turn outside both windows.
        for (index, (role, at)) in [
            ("user", unix_now()),
            ("user", unix_now()),
            ("assistant", unix_now()),
            ("user", 1),
        ]
        .into_iter()
        .enumerate()
        {
            sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)")
                .bind(format!("m{index}")).bind("limited").bind("run-test").bind(role)
                .bind(view.seal(&json!("hi")).unwrap()).bind(at).execute(&view.pool).await.unwrap();
        }
        let mut level = app.store.capability_levels(agent).await.unwrap().levels["member"].clone();
        level.monthly_turns = Some(3);
        assert!(enforce_turn_limits(&app, &user, agent, &level)
            .await
            .is_ok());
        level.daily_turns = Some(2);
        let error = enforce_turn_limits(&app, &user, agent, &level)
            .await
            .unwrap_err();
        assert_eq!(error.0, StatusCode::FORBIDDEN);
        assert!(error.1.contains("2 messages per 24 hours"));
        level.id = "operator".into();
        assert!(enforce_turn_limits(&app, &user, agent, &level)
            .await
            .is_ok());
    }
    #[tokio::test]
    async fn access_requests_are_recorded_listed_and_resolved_by_an_operator_decision() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let agent = "shared_default";
        let store = app.store.for_agent(agent).await.unwrap();
        let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(&store.agent_pool)
            .await
            .unwrap();
        let mut metadata = store.unseal_agent(&raw).unwrap();
        metadata["member_access"] = json!({"visitor_access":false,"app_login_members":false,"conversation_privacy":"private"});
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
            .bind(store.seal_agent(&metadata).unwrap())
            .bind(agent)
            .execute(&store.agent_pool)
            .await
            .unwrap();
        let account = app
            .store
            .create_user_account("asker", "fixture-password-hash")
            .await
            .unwrap();
        let user = account["user_id"].as_str().unwrap().to_owned();
        let asker = json!({"_user_id":user});
        let _ = request_access(
            State(app.clone()),
            Extension(asker.clone()),
            Path(agent.into()),
        )
        .await
        .unwrap();
        let Json(status) = access_request_status(
            State(app.clone()),
            Extension(asker.clone()),
            Path(agent.into()),
        )
        .await
        .unwrap();
        assert_eq!(status["requested"], true);
        let admin = json!({"_user_id":"admin"});
        let Json(listing) = read(
            State(app.clone()),
            Extension(admin.clone()),
            Path(agent.into()),
            Query(HashMap::new()),
        )
        .await
        .unwrap();
        assert_eq!(listing["access_requests"][0]["user_id"], user.as_str());
        let _ = update(
            State(app.clone()),
            Extension(admin),
            Path(agent.into()),
            Json(MemberUpdate {
                user_id: user.clone(),
                level_id: Some("member".into()),
                blocked: false,
                expected_revision: listing["access_requests"][0]["revision"].as_u64().unwrap(),
            }),
        )
        .await
        .unwrap();
        let Json(status) = access_request_status(
            State(app.clone()),
            Extension(asker.clone()),
            Path(agent.into()),
        )
        .await
        .unwrap();
        assert_eq!(status["requested"], false);
        // Now a member: asking again is refused rather than re-queued.
        assert!(
            request_access(State(app), Extension(asker), Path(agent.into()))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn activity_counts_only_this_agents_sessions_and_messages() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let user = app.store.for_user("admin").await.unwrap();
        for (sid, agent, messages) in [
            ("mine", "shared_default", 2),
            ("elsewhere", "other-agent", 1),
        ] {
            user.put(
                &format!("session:{sid}"),
                &json!({"id":sid,"title":sid,"agent_id":agent}),
            )
            .await
            .unwrap();
            for index in 0..messages {
                sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?)")
                    .bind(format!("{sid}-{index}")).bind(sid).bind("run-test").bind("user")
                    .bind(user.seal(&json!("hi")).unwrap()).bind(1_i64).execute(&user.pool).await.unwrap();
            }
        }
        let (sessions, messages, _) = activity(&app, "admin", "shared_default").await.unwrap();
        assert_eq!((sessions, messages), (1, 2));
    }
    #[tokio::test]
    async fn deleting_a_level_reassigns_visitor_relationship_without_unblocking_or_restoring_removed_users(
    ) {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let identity = json!({"_user_id":"admin"});
        let agent = "shared_default";
        let mut levels = app.store.capability_levels(agent).await.unwrap();
        let mut custom = levels.levels["member"].clone();
        custom.id = "custom".into();
        custom.name = "Custom".into();
        levels.levels.insert("custom".into(), custom);
        levels.order.insert(1, "custom".into());
        let Json(saved) = ladder(
            State(app.clone()),
            Extension(identity.clone()),
            Path(agent.into()),
            Json(LadderUpdate {
                expected_revision: levels.revision,
                ladder: levels,
                confirm_expansion: true,
                reassignments: BTreeMap::new(),
            }),
        )
        .await
        .unwrap();
        let mut users = Vec::new();
        for name in ["explicit", "blocked", "removed"] {
            let account = app
                .store
                .create_user_account(name, "fixture-password-hash")
                .await
                .unwrap();
            let user = account["user_id"].as_str().unwrap().to_owned();
            app.store
                .access_repository()
                .manage_agent_member("admin", &user, agent, Some("custom"), false, 0)
                .await
                .unwrap();
            if name != "explicit" {
                app.store
                    .access_repository()
                    .manage_agent_member("admin", &user, agent, None, name == "blocked", 1)
                    .await
                    .unwrap();
            }
            users.push(user);
        }
        let mut levels: crate::capabilities::CapabilityLevels =
            serde_json::from_value(saved["capability_levels"].clone()).unwrap();
        levels.levels.remove("custom");
        levels.order.retain(|id| id != "custom");
        ladder(
            State(app.clone()),
            Extension(identity),
            Path(agent.into()),
            Json(LadderUpdate {
                expected_revision: levels.revision,
                ladder: levels,
                confirm_expansion: true,
                reassignments: BTreeMap::from([("custom".into(), "visitor".into())]),
            }),
        )
        .await
        .unwrap();
        for (index, user) in users.iter().enumerate() {
            let member = app
                .store
                .agent_membership(user, agent)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(member["capability_level_id"], "visitor");
            assert_eq!(
                member["relationship"],
                if index == 1 { "revoked" } else { "caller" }
            );
            assert_eq!(member["agent_administrator"], false);
            assert_eq!(member["origin"], ["explicit", "blocked", "removed"][index]);
        }
    }
}
