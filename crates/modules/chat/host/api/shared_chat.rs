//! Browser-authenticated shared-session actions; each operation retains its actor.
use super::*;
use crate::shared_chat;
use axum::Extension;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Invite {
    client_request_id: String,
    expires_in: Option<u64>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Redeem {
    token: String,
    client_request_id: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Help {
    client_request_id: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Participation {
    client_request_id: String,
    expected_revision: u64,
    active: bool,
}
fn no_access() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Session not found".into())
}
async fn access(app: &App, sid: &str) -> Result<crate::shared_chat::SessionAccess, ApiError> {
    shared_chat::resolve(app, sid, shared_chat::actor(app))
        .await
        .map_err(|_| no_access())
}
async fn announce(app: &App, sid: &str, user: &str) -> Result<(), ApiError> {
    let profile = app.store.user_profile(user).await?.unwrap_or(json!({}));
    let name = profile["display_name"].as_str().unwrap_or(user);
    let directory = app
        .store
        .shared_directory(sid)
        .await?
        .ok_or_else(no_access)?;
    if directory["owner_id"] == user {
        return Ok(());
    }
    let store = app
        .store
        .for_user(directory["owner_id"].as_str().unwrap())
        .await?;
    store.publish_shared_join(sid, user, name).await?;
    // Help resolution is a durable canonical session update, not agent deactivation.
    if shared_chat::staff(&store, user, directory["agent_id"].as_str().unwrap()).await? {
        store.shared_help_finish(sid, "resolved", user).await?;
    }
    Ok(())
}
async fn sharing(Extension(app): Extension<App>, Path(sid): Path<String>) -> Api {
    let rights = access(&app, &sid).await?;
    let store = app.store.for_user(&rights.owner_id).await?;
    let policy = shared_chat::policy(&store, &rights.agent_id).await?;
    let mut participants = store.shared_participants(&sid).await?;
    for participant in &mut participants {
        let user = participant["user_id"].as_str().unwrap();
        let profile = store.user_profile(user).await?.unwrap_or(json!({}));
        participant["display_name"] = profile.get("display_name").cloned().unwrap_or(json!(user));
    }
    let write = rights.can_write();
    Ok(Json(
        json!({"policy":policy,"participants":participants,"participation":store.participation_state(&sid).await?,
 "help":store.get(&format!("shared_help:{sid}")).await?.unwrap_or(json!({"status":"none"})),
 "invitations":if write{store.shared_invites(&sid).await?}else{vec![]},
 "access":{"can_send":write,"can_control":write,"can_invite":write&&policy.shareable,"observer":!write}}),
    ))
}
async fn issue(
    Extension(app): Extension<App>,
    Path(sid): Path<String>,
    Json(body): Json<Invite>,
) -> Api {
    let _guard = app.gate.lock().await;
    let rights = access(&app, &sid).await?;
    if !rights.can_write()
        || !shared_chat::policy(&app.store, &rights.agent_id)
            .await?
            .shareable
    {
        return Err(no_access());
    }
    let ttl = body.expires_in.unwrap_or(86400);
    if body.client_request_id.is_empty()
        || body.client_request_id.len() > 128
        || ttl == 0
        || ttl > 30 * 86400
    {
        return Err(bad("Invalid invitation expiration or request ID"));
    }
    Ok(Json(
        app.store
            .shared_invite_issue(&sid, &rights.actor_id, &body.client_request_id, ttl)
            .await?,
    ))
}
async fn revoke(Extension(app): Extension<App>, Path((sid, id)): Path<(String, String)>) -> Api {
    let _guard = app.gate.lock().await;
    let rights = access(&app, &sid).await?;
    if !rights.can_write() {
        return Err(no_access());
    }
    app.store.shared_invite_revoke(&sid, &id).await?;
    Ok(Json(json!({"revoked":true})))
}
async fn require_active_directory(app: &App, directory: &Value) -> Result<(), ApiError> {
    let owner = directory["owner_id"].as_str().ok_or_else(no_access)?;
    let sid = directory["session_id"].as_str().ok_or_else(no_access)?;
    let storage = app.store.for_user(owner).await?;
    let session = storage.get(&format!("session:{sid}")).await?.ok_or_else(no_access)?;
    if session["status"] != "active" { return Err(no_access()); }
    Ok(())
}
async fn redeem(Extension(app): Extension<App>, Json(body): Json<Redeem>) -> Api {
    if body.client_request_id.is_empty() || body.client_request_id.len() > 128 {
        return Err(bad("Request ID required"));
    }
    let _guard = app.gate.lock().await;
    let user = shared_chat::actor(&app);
    let account = app.store.account_by_id(user).await?.ok_or_else(no_access)?;
    if !crate::access::Principal::from_account(&account)?.active() {
        return Err(no_access());
    }
    let sid = app
        .store
        .shared_invite_session_for(&body.token, user)
        .await?
        .ok_or_else(|| bad("Invitation expired or used"))?;
    let dir = app
        .store
        .shared_directory(&sid)
        .await?
        .ok_or_else(no_access)?;
    require_active_directory(&app, &dir).await?;
    let agent = dir["agent_id"].as_str().unwrap();
    let live = app.store.live_access(user, agent).await?;
    if !live.agent_enabled
        || !live.principal.active()
        || app
            .store
            .agent_membership(user, agent)
            .await?
            .is_some_and(|v| v["relationship"] == "revoked")
    {
        return Err(no_access());
    }
    if !shared_chat::policy(&app.store, agent).await?.shareable {
        return Err(no_access());
    }
    let (sid, added) = app
        .store
        .shared_invite_redeem(&body.token, user)
        .await
        .map_err(|_| bad("Invitation expired or used"))?;
    let _ = added;
    announce(&app, &sid, user).await?;
    Ok(Json(json!({"session_id":sid})))
}
async fn join(Extension(app):Extension<App>,Path(sid):Path<String>)->Api {
    let _guard=app.gate.lock().await;let user=shared_chat::actor(&app);
    let rights=shared_chat::resolve(&app,&sid,user).await.map_err(|_|no_access())?;
    if !shared_chat::staff(&app.store,user,&rights.agent_id).await? {return Err(no_access());}
    let store=app.store.for_user(&rights.owner_id).await?;
    store.operator_join(&sid,user).await?;
    let profile=app.store.user_profile(user).await?.unwrap_or(json!({}));
    store.publish_shared_join(&sid,user,profile["display_name"].as_str().unwrap_or("Agent Operator")).await?;
    Ok(Json(json!({"session_id":sid,"joined":true})))
}
async fn operator_reply(Extension(app):Extension<App>,Path(sid):Path<String>,Json(body):Json<Value>)->Api {
    let _guard=app.gate.lock().await;let actor=shared_chat::actor(&app);
    let rights=shared_chat::resolve(&app,&sid,actor).await.map_err(|_|no_access())?;
    if !shared_chat::staff(&app.store,actor,&rights.agent_id).await? {return Err(no_access());}
    if body.as_object().is_none_or(|o|o.keys().any(|k|!matches!(k.as_str(),"text"|"client_message_id"|"attachment_ids")))
        || body["attachment_ids"].as_array().is_some_and(|a|!a.is_empty()) {return Err(bad("Operator replies support text only"));}
    let text=body["text"].as_str().filter(|v|!v.trim().is_empty()&&v.len()<=65536).ok_or_else(||bad("Message text required"))?;
    let request=super::required(&body,"client_message_id",128)?;
    let store=app.store.for_user(&rights.owner_id).await?;
    let session=store.get(&format!("session:{sid}")).await?.ok_or_else(no_access)?;
    if session["conversation_privacy"]!="operator_reviewable" || session["operator_joined"][actor]!=true {return Err(no_access());}
    let rid=format!("operator:{}",crate::digest(&json!([sid,actor,request])));
    let receipt=format!("receipt:message:{sid}:{actor}:{request}");
    let result=json!({"run_id":null,"operator_reply":true});
    let docs=vec![(receipt,json!({"digest":crate::digest(&body),"result":result}))];
    let value=store.admit_shared_message(docs,&sid,"human_message",&json!({"role":"user","content":text,"run_id":rid,"author_id":actor,"message_kind":"operator"})).await?;
    Ok(Json(value))
}
async fn discover(Extension(app): Extension<App>, Path(agent): Path<String>) -> Api {
    let user = shared_chat::actor(&app);
    let policy = shared_chat::policy(&app.store, &agent).await?;
    if !app.store.live_access(user, &agent).await?.agent_enabled
        || !shared_chat::staff(&app.store, user, &agent).await?
    {
        return Err(no_access());
    }
    let mut sessions = vec![];
    for directory in app.store.shared_all_sessions().await? {
        if directory["agent_id"] != agent {
            continue;
        }
        let owner = directory["owner_id"].as_str().unwrap();
        let sid = directory["session_id"].as_str().unwrap();
        let store = app.store.for_user(owner).await?;
        if let Some(session) = store.get(&format!("session:{sid}")).await? {
            if session["status"] != "active" || session["conversation_privacy"]!="operator_reviewable" {continue;}
            // Pending help requests surface first so staff can use this list as a handoff inbox.
            let help=store.get(&format!("shared_help:{sid}")).await?.filter(|h|h["status"]=="pending");
            sessions.push(json!({"id":sid,"title":session["title"],"agent_id":agent,"can_observe":true,"can_join":true,
                "help_pending":help.is_some(),"help_requested_at":help.map(|h|h["requested_at"].clone())}));
        }
    }
    sessions.sort_by_key(|s|(s["help_pending"]!=true,std::cmp::Reverse(s["help_requested_at"].as_u64().unwrap_or(0))));
    Ok(Json(json!({"sessions":sessions})))
}
async fn help(
    Extension(app): Extension<App>,
    Path(sid): Path<String>,
    Json(body): Json<Help>,
) -> Api {
    let _guard = app.gate.lock().await;
    let rights = access(&app, &sid).await?;
    if !rights.can_write()
        || !shared_chat::policy(&app.store, &rights.agent_id)
            .await?
            .help
    {
        return Err(no_access());
    }
    if body.client_request_id.is_empty() || body.client_request_id.len() > 128 {
        return Err(bad("Request ID required"));
    }
    let store = app.store.for_user(&rights.owner_id).await?;
    Ok(Json(
        store
            .shared_help_request(&sid, &rights.actor_id, &body.client_request_id)
            .await?,
    ))
}
async fn cancel_help(Extension(app): Extension<App>, Path(sid): Path<String>) -> Api {
    let _guard = app.gate.lock().await;
    let rights = access(&app, &sid).await?;
    if !rights.can_write() {
        return Err(no_access());
    }
    let store = app.store.for_user(&rights.owner_id).await?;
    Ok(Json(
        store
            .shared_help_finish(&sid, "cancelled", &rights.actor_id)
            .await?,
    ))
}
async fn participation(
    Extension(app): Extension<App>,
    Path(sid): Path<String>,
    Json(body): Json<Participation>,
) -> Api {
    if body.client_request_id.is_empty()
        || body.client_request_id.len() > 128
        || body.expected_revision >= i64::MAX as u64
    {
        return Err(bad("Invalid participation request ID or revision"));
    }
    let _guard = app.gate.lock().await;
    let rights = access(&app, &sid).await?;
    if !rights.can_write() {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Observation is read only".into(),
        ));
    }
    let store = app.store.for_user(&rights.owner_id).await?;
    // Capture running handles before the durable cancellation changes their status.
    let running = if !body.active {
        store
            .active_runs()
            .await?
            .into_iter()
            .filter(|run| run["session_id"] == sid)
            .filter_map(|run| run["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let result = store
        .set_participation(
            &sid,
            &rights.actor_id,
            body.active,
            body.expected_revision,
            &body.client_request_id,
        )
        .await
        .map_err(|error| match error.to_string().as_str() {
            "Participation revision changed" | "Identifier reused" => {
                conflict("Participation changed or request ID reused; reload before retrying")
            }
            _ => ApiError::from(error),
        })?;
    let current = store.participation_state(&sid).await?;
    if !body.active && current["active"] == false && current["generation"] == result["generation"] {
        // The loop process also observes durable generation fences; these handles stop
        // any work local to this API process without waiting for its next boundary.
        let active = app.active.lock().await;
        for id in running {
            if let Some(cancel) = active.get(&id) {
                cancel.cancel();
            }
        }
    }
    Ok(Json(result))
}
pub fn router() -> Router<App> {
    Router::new()
        .route("/sessions/{sid}/sharing", get(sharing))
        .route(
            "/sessions/{sid}/participation",
            axum::routing::put(participation),
        )
        .route("/sessions/{sid}/invitations", post(issue))
        .route(
            "/sessions/{sid}/invitations/{id}",
            axum::routing::delete(revoke),
        )
        .route("/chat-invitations/redeem", post(redeem))
        .route("/sessions/{sid}/join", post(join))
        .route("/sessions/{sid}/operator-replies",post(operator_reply))
        .route("/agents/{id}/shared-sessions", get(discover))
        .route("/sessions/{sid}/help", post(help).delete(cancel_help))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::Row;
    use tower::ServiceExt;
    async fn set_policy(app: &App, policy: shared_chat::SharedChatPolicy) {
        let row = sqlx::query("SELECT metadata FROM agents WHERE id='shared_default'")
            .fetch_one(&app.store.agent_pool)
            .await
            .unwrap();
        let mut metadata = app
            .store
            .unseal(&row.get::<Vec<u8>, _>("metadata"))
            .unwrap();
        metadata["shared_chat"] = serde_json::to_value(policy).unwrap();
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1 WHERE id='shared_default'")
            .bind(app.store.seal_agent(&metadata).unwrap())
            .execute(&app.store.agent_pool)
            .await
            .unwrap();
    }
    async fn active_user(app: &App, name: &str) -> String {
        let account = app
            .store
            .create_user_account(name, "fixture-long-password-hash")
            .await
            .unwrap();
        let id = account["user_id"].as_str().unwrap();
        app.store
            .update_account_access(
                id,
                1,
                crate::access::AccountStatus::Active,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        id.into()
    }
    async fn cookie(app: &App, user: &str) -> String {
        let account = app.store.account_by_id(user).await.unwrap().unwrap();
        let token = crate::uuid();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),&json!({"user_id":user,"csrf":"fixture","expires":crate::now()+3600,"generation":account["generation"]})).await.unwrap();
        format!("v3_session={token}")
    }
    fn request(
        method: &str,
        url: &str,
        cookie: &str,
        body: Value,
    ) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(method)
            .uri(url)
            .header("cookie", cookie)
            .header("content-type", "application/json")
            .header("x-csrf-token", "fixture")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    }
    async fn body(response: axum::response::Response) -> Value {
        serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn archived_sessions_deny_new_admissions_without_consuming_invites() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let owner = active_user(&app,"archived-owner").await;
        let guest = active_user(&app,"archived-guest").await;
        let owner_store = app.store.for_user(&owner).await.unwrap();
        set_policy(&app,shared_chat::SharedChatPolicy {shareable:true,admin_join:true,admin_view:true,..Default::default()}).await;
        let guest_cookie = cookie(&app,&guest).await;
        let admin_cookie = cookie(&app,"admin").await;
        let router = crate::router(app.clone());
        for status in ["recycled","deleted"] {
            let sid = format!("archive-{status}");
            owner_store.put(&format!("session:{sid}"),&json!({"id":sid,"agent_id":"shared_default","title":"Archived","status":status})).await.unwrap();
            app.store.register_shared_session(&owner,&sid,"shared_default").await.unwrap();
            let invitation = app.store.shared_invite_issue(&sid,&owner,"archived-invite",3600).await.unwrap();
            let response = router.clone().oneshot(request("POST","/api/v3/chat-invitations/redeem",&guest_cookie,json!({"token":invitation["token"],"client_request_id":"redeem-archived"}))).await.unwrap();
            assert_eq!(response.status(),StatusCode::NOT_FOUND);
            assert!(!app.store.shared_is_participant(&sid,&guest).await.unwrap());
            assert_eq!(app.store.shared_invite_session_for(invitation["token"].as_str().unwrap(),&guest).await.unwrap(),Some(sid.clone()));
            let response = router.clone().oneshot(request("POST",&format!("/api/v3/sessions/{sid}/join"),&admin_cookie,json!({}))).await.unwrap();
            assert_eq!(response.status(),StatusCode::NOT_FOUND);
            assert!(!app.store.shared_is_participant(&sid,"admin").await.unwrap());
        }
        let response = router.oneshot(request("GET","/api/v3/agents/shared_default/shared-sessions",&admin_cookie,json!({}))).await.unwrap();
        assert_eq!(response.status(),StatusCode::OK);
        assert!(body(response).await["sessions"].as_array().unwrap().is_empty());
    }
    #[tokio::test]
    async fn authenticated_invitee_retains_actor_and_one_canonical_history() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let alice = active_user(&app, "shared-alice").await;
        app.store
            .put(
                "session:shared-http",
                &json!({"id":"shared-http","agent_id":"shared_default","title":"Canonical"}),
            )
            .await
            .unwrap();
        app.store
            .register_shared_session("admin", "shared-http", "shared_default")
            .await
            .unwrap();
        set_policy(
            &app,
            shared_chat::SharedChatPolicy {
                shareable: true,
                ..Default::default()
            },
        )
        .await;
        let admin_cookie = cookie(&app, "admin").await;
        let alice_cookie = cookie(&app, &alice).await;
        let router = crate::router(app.clone());
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/v3/sessions/shared-http/invitations",
                &admin_cookie,
                json!({"client_request_id":"issue","expires_in":86400}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let invitation = body(response).await;
        for _ in 0..2 {
            let response = router
                .clone()
                .oneshot(request(
                    "POST",
                    "/api/v3/chat-invitations/redeem",
                    &alice_cookie,
                    json!({"client_request_id":"redeem","token":invitation["token"]}),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert!(app
            .store
            .for_user(&alice)
            .await
            .unwrap()
            .get("session:shared-http")
            .await
            .unwrap()
            .is_none());
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/v3/sessions/shared-http/sharing",
                &alice_cookie,
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let sharing = body(response).await;
        assert_eq!(sharing["access"]["can_send"], true);
        let response = router
            .oneshot(request(
                "POST",
                "/api/v3/sessions/shared-http/invitations",
                &alice_cookie,
                json!({"client_request_id":"alice-issue"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let invitations = app.store.shared_invites("shared-http").await.unwrap();
        assert!(invitations.iter().any(|v| v["issuer_id"] == alice));
        let (messages, _) = app.store.chat_snapshot("shared-http").await.unwrap();
        assert_eq!(messages.len(), 1);
        let scoped = shared_chat::scope(
            {
                let mut scoped = app.clone();
                scoped.store = app.store.for_user(&alice).await.unwrap();
                scoped
            },
            "shared-http",
        )
        .await
        .unwrap();
        assert_eq!(scoped.store.current_user_id(), "admin");
        assert_eq!(shared_chat::actor(&scoped), alice);
    }
    #[tokio::test]
    async fn staff_view_and_join_are_independent_and_revoked_membership_denies() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let alice = active_user(&app, "shared-owner").await;
        let owner = app.store.for_user(&alice).await.unwrap();
        owner
            .put(
                "session:staff-chat",
                &json!({"id":"staff-chat","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        app.store
            .register_shared_session(&alice, "staff-chat", "shared_default")
            .await
            .unwrap();
        assert!(shared_chat::resolve(&app, "staff-chat", "admin")
            .await
            .is_err());
        set_policy(&app,shared_chat::SharedChatPolicy{admin_view:true,admin_join:true,..Default::default()}).await;
        // Old global switches cannot change the pinned privacy of an existing conversation.
        assert!(shared_chat::resolve(&app,"staff-chat","admin").await.is_err());
        owner.put("session:staff-chat",&json!({"id":"staff-chat","agent_id":"shared_default","status":"active","conversation_privacy":"operator_reviewable"})).await.unwrap();
        assert_eq!(shared_chat::resolve(&app,"staff-chat","admin").await.unwrap().kind,shared_chat::AccessKind::Observer);
        let scoped=app.clone();
        assert!(join(Extension(scoped.clone()),Path("staff-chat".into())).await.is_ok());
        // Joining permits only the dedicated human-reply route, never participant controls.
        assert_eq!(shared_chat::resolve(&app,"staff-chat","admin").await.unwrap().kind,shared_chat::AccessKind::Observer);
        let reply=json!({"text":"Operator response","client_message_id":"operator-once"});
        operator_reply(Extension(scoped.clone()),Path("staff-chat".into()),Json(reply.clone())).await.unwrap();
        operator_reply(Extension(scoped),Path("staff-chat".into()),Json(reply)).await.unwrap();
        let (messages,_)=owner.chat_snapshot("staff-chat").await.unwrap();
        assert_eq!(messages.iter().filter(|m|m["content"]=="Operator response").count(),1);
        let bob = active_user(&app, "shared-revoked").await;
        app.store.shared_join("staff-chat", &bob).await.unwrap();
        assert!(shared_chat::resolve(&app, "staff-chat", &bob).await.is_ok());
        app.store
            .set_agent_membership(
                &bob,
                "shared_default",
                crate::access::Relationship::Revoked,
                0,
            )
            .await
            .unwrap();
        assert!(shared_chat::resolve(&app, "staff-chat", &bob)
            .await
            .is_err());
        let revision = app.store.account_by_id(&alice).await.unwrap().unwrap()["access_revision"]
            .as_u64()
            .unwrap();
        app.store
            .update_account_access(
                &alice,
                revision,
                crate::access::AccountStatus::Suspended,
                crate::access::Role::User,
                "basic",
            )
            .await
            .unwrap();
        assert!(shared_chat::resolve(&app, "staff-chat", &alice)
            .await
            .is_err());
    }
    #[tokio::test]
    async fn help_lifecycle_is_idempotent_and_metadata_events_are_staged() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        app.store
            .put(
                "session:help-chat",
                &json!({"id":"help-chat","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        let first = app
            .store
            .shared_help_request("help-chat", "requester", "first")
            .await
            .unwrap();
        let duplicate = app
            .store
            .shared_help_request("help-chat", "requester", "second")
            .await
            .unwrap();
        assert_eq!(first, duplicate);
        let cancelled = app
            .store
            .shared_help_finish("help-chat", "cancelled", "admin")
            .await
            .unwrap();
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(
            app.store
                .shared_help_finish("help-chat", "cancelled", "admin")
                .await
                .unwrap(),
            cancelled
        );
        let next = app
            .store
            .shared_help_request("help-chat", "admin", "third")
            .await
            .unwrap();
        assert_ne!(next["id"], first["id"]);
        app.store
            .shared_help_finish("help-chat", "resolved", "admin")
            .await
            .unwrap();
        let types = sqlx::query_scalar::<_, String>(
            "SELECT type FROM notification_stage WHERE session_id='help-chat' ORDER BY source_seq",
        )
        .fetch_all(&app.store.pool)
        .await
        .unwrap();
        assert_eq!(
            types,
            vec![
                "session.help_requested",
                "session.help_cancelled",
                "session.help_requested",
                "session.help_resolved"
            ]
        );
        let actors = sqlx::query_scalar::<_, String>(
            "SELECT actor FROM notification_stage WHERE session_id='help-chat' ORDER BY source_seq",
        )
        .fetch_all(&app.store.pool)
        .await
        .unwrap();
        assert_eq!(actors, vec!["requester", "admin", "admin", "admin"]);
    }
    #[tokio::test]
    async fn participants_toggle_persist_retry_and_human_chat_while_observers_cannot() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let alice = active_user(&app, "participation-alice").await;
        app.store
            .put(
                "session:participation-http",
                &json!({"id":"participation-http","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        app.store
            .register_shared_session("admin", "participation-http", "shared_default")
            .await
            .unwrap();
        app.store
            .shared_join("participation-http", &alice)
            .await
            .unwrap();
        let alice_cookie = cookie(&app, &alice).await;
        let admin_cookie = cookie(&app, "admin").await;
        let router = crate::router(app.clone());
        let toggle = json!({"client_request_id":"off","expected_revision":0,"active":false});
        let response = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/participation-http/participation",
                &alice_cookie,
                toggle.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let first = body(response).await;
        assert_eq!(first["active"], false);
        assert_eq!(first["revision"], 1);
        assert_eq!(first["actor_id"], alice);
        let response = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/participation-http/participation",
                &alice_cookie,
                toggle,
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, first);
        let response = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/participation-http/participation",
                &alice_cookie,
                json!({"client_request_id":"stale","expected_revision":0,"active":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/participation-http/participation",
                &alice_cookie,
                json!({"client_request_id":"off","expected_revision":1,"active":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let response=router.clone().oneshot(request("PUT","/api/v3/sessions/participation-http/participation",&alice_cookie,json!({"client_request_id":"bad","expected_revision":1,"active":true,"unexpected":true}))).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let response = router
            .clone()
            .oneshot(request(
                "POST",
                "/api/v3/sessions/participation-http/messages",
                &alice_cookie,
                json!({"client_message_id":"human-one","text":"INACTIVE_HTTP_SENTINEL"}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let sent = body(response).await;
        assert!(sent["run_id"].is_null());
        assert!(app
            .store
            .recent_runs(Some("participation-http"))
            .await
            .unwrap()
            .is_empty());
        let response = router
            .clone()
            .oneshot(request(
                "GET",
                "/api/v3/sessions/participation-http",
                &alice_cookie,
                json!({}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let snapshot = body(response).await;
        assert!(snapshot["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["content"] == "INACTIVE_HTTP_SENTINEL" && row["author_id"] == alice));
        let response = router
            .clone()
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/participation-http/participation",
                &admin_cookie,
                json!({"client_request_id":"on","expected_revision":1,"active":true}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await["revision"], 2);
        assert!(app
            .store
            .recent_runs(Some("participation-http"))
            .await
            .unwrap()
            .is_empty());
        // A staff observer can view a different owner's chat but cannot toggle it.
        app.store
            .set_agent_membership(
                &alice,
                "shared_default",
                crate::access::Relationship::Caller,
                0,
            )
            .await
            .unwrap();
        let owner = app.store.for_user(&alice).await.unwrap();
        owner
            .put(
                "session:observed-participation",
                &json!({"id":"observed-participation","agent_id":"shared_default","status":"active","conversation_privacy":"operator_reviewable"}),
            )
            .await
            .unwrap();
        app.store
            .register_shared_session(&alice, "observed-participation", "shared_default")
            .await
            .unwrap();
        set_policy(
            &app,
            shared_chat::SharedChatPolicy {
                admin_view: true,
                ..Default::default()
            },
        )
        .await;
        let response = router
            .oneshot(request(
                "PUT",
                "/api/v3/sessions/observed-participation/participation",
                &admin_cookie,
                json!({"client_request_id":"observer-off","expected_revision":0,"active":false}),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            owner
                .participation_state("observed-participation")
                .await
                .unwrap()["active"],
            true
        );
    }
}
