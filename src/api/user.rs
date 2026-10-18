//! Route each person's chat request to that person's storage. This is the
//! boundary that keeps one user's conversations out of another user's view.
use crate::State;
use crate::{App, InspectArgs, SessionAbilityBody, SessionModeBody};
use axum::{extract::{Path, Query}, http::HeaderMap, response::{IntoResponse, Response},
    routing::{get, post}, Extension, Json, Router};
use serde_json::Value;
use std::collections::HashMap;

async fn session_app(app: App, session_id: &str) -> Result<App, crate::ApiError> {
    session_app_lifecycle(app,session_id,false).await
}
async fn session_app_lifecycle(app:App,session_id:&str,lifecycle:bool)->Result<App,crate::ApiError>{
    if lifecycle {crate::shared_chat::scope_lifecycle(app,session_id).await}
    else {crate::shared_chat::scope(app,session_id).await}
      .map_err(session_access_error)
}
fn session_access_error(error:anyhow::Error)->crate::ApiError {
    if error.downcast_ref::<crate::shared_chat::SessionAuthorityDenied>().is_some() {
        crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Conversation authority unavailable".into())
    } else {crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Session not found".into())}
}
fn guard_access(app:&App,write:bool,owner_only:bool)->Result<(),crate::ApiError>{
    let Some(access)=app.chat_access.as_ref() else {return Ok(())};
    if (write&&!access.can_write()) || (owner_only&&!access.is_owner()){
        return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Conversation access does not permit this action".into()));
    }
    Ok(())
}
async fn run_app(app: App, run_id: &str) -> Result<App, crate::ApiError> {
    let actor=crate::shared_chat::actor(&app).to_owned();
    let mut candidates=vec![app.clone()];
    for entry in app.store.shared_sessions_for(&actor).await? {
        if let Some(owner)=entry["owner_id"].as_str() {
            let mut scoped=app.clone();scoped.store=app.store.for_user(owner).await?;
            scoped.chat_actor=Some(actor.clone());candidates.push(scoped);
        }
    }
    for candidate in candidates {
        if let Ok(run)=candidate.run(run_id).await {
            if run["history_removed"]==true { return Err(crate::conflict("This run was removed by Undo and cannot be controlled")); }
            if let Some(sid)=run["session_id"].as_str(){return session_app(candidate,sid).await;}
        }
    }
    Err(crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Run not found".into()))
}
trait SessionPath { fn session_id(&self) -> &str; }
impl SessionPath for String { fn session_id(&self) -> &str { self } }
impl SessionPath for (String, String) { fn session_id(&self) -> &str { &self.0 } }
macro_rules! json_arg {
    ($name:ident => $inner:ident) => {
        async fn $name(Extension(app): Extension<App>, Json(body): Json<Value>) -> Response {
            crate::$inner(State(app), Json(body)).await.into_response()
        }
    };
}
macro_rules! path_arg {
    ($name:ident => $inner:ident, $path:ty) => {
        async fn $name(Extension(app): Extension<App>, Path(path): Path<$path>) -> Response {
            let app = match session_app(app, path.session_id()).await {
                Ok(app) => app, Err(error) => return error.into_response(),
            };
            let private=matches!(stringify!($inner),"background_task_metadata_api"|"state_inspect_api"|"source_review"|"promotions"|"promotion_review"|"promotion_receipt"|"memory_list"|"memory_delete"|"upload_delete"|"upload_describe"|"promotion_reject");
            if let Err(error)=guard_access(&app,matches!(stringify!($inner),"memory_delete"|"upload_delete"|"upload_describe"|"promotion_reject"),private){return error.into_response();}
            crate::$inner(State(app), Path(path)).await.into_response()
        }
    };
}
macro_rules! path_json {
    ($name:ident => $inner:ident, $path:ty, $body:ty) => {
        async fn $name(Extension(app): Extension<App>, Path(path): Path<$path>,
            Json(body): Json<$body>) -> Response {
            let app = match session_app(app, path.session_id()).await {
                Ok(app) => app, Err(error) => return error.into_response(),
            };
            let owner_only=matches!(stringify!($inner),"recycle_session_api"|"restore_session_api"|"patch_session_api"|"memory_save"|"promotion_reject"|"voice_route"|"voice_control"|"voice_audio"|"voice_speech"|"undo_session_api"|"fork_session_api");
            if let Err(error)=guard_access(&app,true,owner_only){return error.into_response();}
            crate::$inner(State(app), Path(path), Json(body)).await.into_response()
        }
    };
}
async fn sessions(Extension(app): Extension<App>, identity:Option<Extension<Value>>, Query(args): Query<crate::SessionListArgs>) -> Response {
    let result=crate::sessions(State(app),Query(args)).await;
    match result {Ok(mut value)=>{if let Some(agent)=identity.as_ref().and_then(|v|v.0["_embed_agent"].as_str()) {if let Some(items)=value.0["sessions"].as_array_mut(){items.retain(|s|s["agent_id"].as_str().unwrap_or("shared_default")==agent);}}value.into_response()},Err(e)=>e.into_response()}
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionStatisticsArgs { status: Option<String>, offset: Option<usize>, limit: Option<usize> }
async fn session_statistics(Extension(app): Extension<App>, Query(args): Query<SessionStatisticsArgs>) -> Response {
    async fn read(app: App, args: SessionStatisticsArgs) -> crate::Api {
        let recycled = match args.status.as_deref().unwrap_or("active") {
            "active" => false, "recycled" => true, _ => return Err(crate::bad("Invalid session status")),
        };
        let limit = args.limit.unwrap_or(100);
        if !(1..=100).contains(&limit) { return Err(crate::bad("Invalid statistics limit")); }
        let (page, next) = app.store.session_statistics_page(recycled,args.offset.unwrap_or(0),limit).await?;
        let mut admitted = HashMap::new();
        let mut sessions = Vec::new();
        for item in page {
            let agent = item["agent_id"].as_str().unwrap_or("shared_default");
            let allowed = if let Some(allowed) = admitted.get(agent) { *allowed } else {
                let allowed = app.store.live_access(app.store.current_user_id(),agent).await
                    .map(|access| access.admitted()).unwrap_or(false);
                admitted.insert(agent.to_owned(),allowed); allowed
            };
            if allowed { sessions.push(item); }
        }
        Ok(Json(serde_json::json!({"sessions":sessions,"next_offset":next,"server_time":crate::now()})))
    }
    read(app,args).await.into_response()
}

async fn session_search(Extension(app): Extension<App>, identity:Option<Extension<Value>>, Query(args): Query<crate::SessionSearchArgs>) -> Response {
    let result=crate::api::sessions::session_search_scoped(app,args,identity.as_ref().and_then(|v|v.0["_embed_agent"].as_str())).await;
    match result {Ok(mut value)=>{if let Some(agent)=identity.as_ref().and_then(|v|v.0["_embed_agent"].as_str()) {if let Some(items)=value.0["results"].as_array_mut(){items.retain(|s|s["agent_id"].as_str().unwrap_or("shared_default")==agent);}}value.into_response()},Err(e)=>e.into_response()}
}
json_arg!(create_session => create_session);
async fn direct_message(Extension(app):Extension<App>,Json(body):Json<Value>)->Response {
    crate::api::direct_messages::send(State(app),Json(body)).await.into_response()
}
path_arg!(snapshot => snapshot, String);
path_arg!(background_task_metadata => background_task_metadata_api, (String,String));
async fn session_read(Extension(app):Extension<App>,Path(id):Path<String>,Json(body):Json<crate::SessionReadBody>)->Response {
    let app=match session_app(app,&id).await{Ok(app)=>app,Err(error)=>return error.into_response()};
    if let Err(error)=guard_access(&app,true,false){return error.into_response();}
    let outcome=app.store.shared_ack_read(&id,crate::shared_chat::actor(&app),body.through_seq).await;
    match outcome {
        Ok(crate::store::session::SessionPatchOutcome::Saved(value))=>Json(value).into_response(),
        Ok(crate::store::session::SessionPatchOutcome::NotFound)=>crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Session not found".into()).into_response(),
        Ok(crate::store::session::SessionPatchOutcome::Conflict(message))=>crate::conflict(message).into_response(),
        Ok(crate::store::session::SessionPatchOutcome::Invalid(message))=>crate::bad(message).into_response(),
        Err(error)=>crate::ApiError::from(error).into_response()
    }
}
path_json!(session_undo => undo_session_api, String, crate::store::conversation_history::HistoryRequest);
path_json!(session_fork => fork_session_api, String, crate::store::conversation_history::HistoryRequest);
async fn session_lifecycle(app:App,id:String,body:crate::store::session::SessionLifecycle,restore:bool)->Response {
    let app=match session_app_lifecycle(app,&id,true).await {Ok(app)=>app,Err(error)=>return error.into_response()};
    if let Err(error)=guard_access(&app,true,true){return error.into_response();}
    if restore {crate::restore_session_api(State(app),Path(id),Json(body)).await.into_response()}
    else {crate::recycle_session_api(State(app),Path(id),Json(body)).await.into_response()}
}
async fn session_recycle(Extension(app):Extension<App>,Path(id):Path<String>,Json(body):Json<crate::store::session::SessionLifecycle>)->Response {
    session_lifecycle(app,id,body,false).await
}
async fn session_restore(Extension(app):Extension<App>,Path(id):Path<String>,Json(body):Json<crate::store::session::SessionLifecycle>)->Response {
    session_lifecycle(app,id,body,true).await
}
async fn session_purge(Extension(app):Extension<App>,Path(id):Path<String>,Json(body):Json<crate::store::session::SessionLifecycle>)->Response {
    let app=match crate::shared_chat::scope_purge(app,&id).await.map_err(session_access_error) {Ok(app)=>app,Err(error)=>return error.into_response()};
    if let Err(error)=guard_access(&app,true,true){return error.into_response();}
    crate::purge_session_api(State(app),Path(id),Json(body)).await.into_response()
}
path_json!(session_patch => patch_session_api, String, crate::store::session::SessionPatch);
path_arg!(diagnostics => run_diagnostics_api, (String, String));
path_arg!(progress => run_progress_api, (String, String));
path_arg!(chat_controls => chat_controls_get, String);
path_arg!(chat_usage => chat_usage_get, String);
path_json!(chat_model => chat_model_set, String, crate::api::ChatModelBody);
path_arg!(mode_get => session_mode_get, String);
path_json!(mode_set => session_mode_set, String, SessionModeBody);
path_arg!(abilities => session_abilities, String);
path_json!(ability_set => session_ability_set, (String, String), SessionAbilityBody);
path_json!(send => send, String, Value);
path_json!(voice_route => voice_route, String, Value);
path_json!(voice_control => voice_control, String, Value);
path_json!(voice_audio => voice_audio, String, Value);
path_arg!(voice_capabilities => voice_capabilities,String);
path_json!(voice_speech => voice_speech,String,Value);
path_arg!(attachment => attachment_get, (String, String));
async fn upload(Extension(app): Extension<App>, Path(sid): Path<String>,
    request: axum::extract::Request) -> Response {
    let app = match session_app(app, &sid).await { Ok(app) => app, Err(error) => return error.into_response() };
    let body = match crate::api::uploads::request_body(&app,request).await {
        Ok(body) => body, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,true,false){return error.into_response();}
    crate::upload(State(app),Path(sid),Json(body)).await.into_response()
}
path_arg!(upload_delete => upload_delete, (String,String));
path_arg!(upload_content => upload_content, (String,String));
path_arg!(upload_describe => upload_describe, (String,String));
path_arg!(memory_list => memory_list, String);
path_json!(memory_save => memory_save, String, Value);
path_arg!(memory_delete => memory_delete, (String, String));
path_arg!(source_review => source_review, String);
path_arg!(promotions => promotions, String);
path_arg!(promotion_review => promotion_review, (String, String));
path_arg!(promotion_receipt => promotion_receipt, (String, String));
path_arg!(promotion_reject => promotion_reject, (String, String));

async fn state_inspect(Extension(app): Extension<App>, Path(id): Path<String>,
    Query(args): Query<InspectArgs>) -> Response {
    let app = match session_app(app, &id).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,false,true){return error.into_response();}
    crate::state_inspect_api(State(app), Path(id), Query(args)).await.into_response()
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivityArgs { after: Option<i64> }
async fn activity(Extension(app): Extension<App>, Extension(identity): Extension<Value>, Path(id): Path<String>, Query(args): Query<ActivityArgs>) -> Response {
    let result: Result<_, crate::ApiError> = async {
        crate::phase3_api::administrator(&app, &identity).await?;
        let app=session_app(app,&id).await?;
        guard_access(&app,false,true)?;
        let after=args.after.unwrap_or(0);
        if after<0 {return Err(crate::bad("Invalid activity cursor"));}
        let data=app.store.private_activity(&id,after).await?;
        crate::phase3_api::administrator(&app,&identity).await?;
        session_app(app,&id).await?;
        Ok(([("cache-control","no-store")],Json(data)))
    }.await;
    result.into_response()
}
// Private interactions are available only to an administrator who still owns
// the selected session and can access its agent. A run ID alone grants nothing.
async fn context_snapshot(Extension(app):Extension<App>,Extension(identity):Extension<Value>,
    Path((sid,rid,attempt)):Path<(String,String,String)>) -> Response {
    let result:Result<_,crate::ApiError>=async {
        crate::phase3_api::administrator(&app,&identity).await?;
        let scoped=session_app(app.clone(),&sid).await?;
        guard_access(&scoped,false,true)?;
        if scoped.store.current_user_id()!="admin" {return Err(crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Fixed admin account required".into()));}
        let run=scoped.run(&rid).await.map_err(|_|crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Run not found".into()))?;
        if run["session_id"]!=sid {return Err(crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Run not found".into()));}
        let data=crate::diagnostics::context(&scoped.store,&sid,&rid,&attempt).await?;
        crate::phase3_api::administrator(&app,&identity).await?;
        session_app(app,&sid).await?;
        Ok(([ ("cache-control","no-store") ],Json(data)))
    }.await;
    result.into_response()
}
async fn run_logs(Extension(app): Extension<App>, Extension(identity): Extension<Value>,
    Path((sid,rid)): Path<(String,String)>, Query(args): Query<ActivityArgs>) -> Response {
    let result: Result<_, crate::ApiError> = async {
        crate::phase3_api::administrator(&app,&identity).await?;
        let scoped=session_app(app.clone(),&sid).await?;
        guard_access(&scoped,false,true)?;
        if args.after.unwrap_or(0)<0 {return Err(crate::bad("Invalid activity cursor"));}
        let run=scoped.run(&rid).await
            .map_err(|_|crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Run not found".into()))?;
        if run["session_id"]!=sid {
            return Err(crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Run not found".into()));
        }
        let data=scoped.store.private_run_activity(&sid,&rid,args.after.unwrap_or(0)).await?;
        crate::phase3_api::administrator(&app,&identity).await?;
        session_app(app,&sid).await?;
        Ok(([("cache-control","no-store")],Json(data)))
    }.await;
    result.into_response()
}
async fn events(Extension(app): Extension<App>, Path(id): Path<String>,
    headers: HeaderMap, Extension(auth_doc): Extension<Value>,
    Query(params): Query<HashMap<String, String>>) -> Response {
    let app = match session_app(app, &id).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    crate::events(State(app), Path(id), headers, Extension(auth_doc), Query(params))
        .await.into_response()
}
async fn promote(Extension(app): Extension<App>, Extension(identity): Extension<Value>,
    Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    if let Err(error) = crate::phase3_api::administrator(&app, &identity).await {
        return error.into_response();
    }
    let app = match session_app(app, &id).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,true,true){return error.into_response();}
    crate::promote(State(app), Path(id), Json(body)).await.into_response()
}
// Resolve caller-owned session and live agent access before queue mutation.
async fn followup_delivery(Extension(app): Extension<App>, Path((sid, mid)): Path<(String, String)>,
    Json(body): Json<Value>) -> Response {
    let app = match session_app(app, &sid).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,true,true){return error.into_response();}
    crate::api::followup_queue::delivery(State(app), Path((sid, mid)), Json(body)).await.into_response()
}
async fn followup_resume(Extension(app): Extension<App>, Path(sid): Path<String>,
    Json(body): Json<Value>) -> Response {
    let app = match session_app(app, &sid).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,true,true){return error.into_response();}
    crate::api::followup_queue::resume(State(app), Path(sid), Json(body)).await.into_response()
}
async fn command(Extension(app): Extension<App>, Path((id, action)): Path<(String, String)>,
    Json(body): Json<Value>) -> Response {
    let app = match run_app(app, &id).await {
        Ok(app) => app, Err(error) => return error.into_response(),
    };
    if let Err(error)=guard_access(&app,true,false){return error.into_response();}
    crate::command(State(app), Path((id, action)), Json(body)).await.into_response()
}
async fn approval(Extension(app): Extension<App>, Path(id): Path<String>, Json(body): Json<Value>) -> Response {
    let actor=crate::shared_chat::actor(&app).to_owned();
    let mut candidates=vec![app.clone()];
    match app.store.shared_sessions_for(&actor).await {
        Ok(entries)=>for entry in entries {if let Some(owner)=entry["owner_id"].as_str(){
            if let Ok(store)=app.store.for_user(owner).await{let mut next=app.clone();next.store=store;next.chat_actor=Some(actor.clone());candidates.push(next);}
        }},
        Err(error)=>return crate::ApiError::from(error).into_response()
    }
    for candidate in candidates {
        let Ok(runs)=candidate.store.active_runs().await else{continue};
        for run in runs {
            if run["pending"]["id"]!=id {continue;}
            let owner=run.pointer("/pending/authorization/owner_user_id").and_then(Value::as_str)
                .or_else(||run["requester_user_id"].as_str()).unwrap_or(candidate.store.current_user_id());
            if owner!=actor{return crate::ApiError(axum::http::StatusCode::FORBIDDEN,"Only the requesting credential owner can authorize this action".into()).into_response();}
            let Some(rid)=run["id"].as_str() else{continue};
            let scoped=match run_app(candidate.clone(),rid).await{Ok(app)=>app,Err(error)=>return error.into_response()};
            if let Err(error)=guard_access(&scoped,true,false){return error.into_response();}
            return crate::approval(State(scoped),Path(id),Json(body)).await.into_response();
        }
    }
    crate::ApiError(axum::http::StatusCode::NOT_FOUND,"Approval not found".into()).into_response()
}

// Only caller-owned run summaries; never provider credentials or admin policy.
async fn user_status(Extension(app): Extension<App>,identity:Option<Extension<Value>>) -> crate::Api {
    let mut runs=app.store.active_runs().await?;
    runs.extend(app.store.recent_runs(None).await?);
    if let Some(agent)=identity.as_ref().and_then(|v|v.0["_embed_agent"].as_str()) {runs.retain(|r|r["agent_id"].as_str().unwrap_or("shared_default")==agent);}
    Ok(Json(serde_json::json!({"shutoff":app.shutoff().await?,
        "runs":crate::public_runs(&app,runs).await?})))
}

pub(crate) fn router() -> Router<App> {
    Router::new()
        .route("/main-chat", get(crate::main_chat::read_api).put(crate::main_chat::configure_api))
        .route("/main-notes", get(crate::main_chat::read_api).post(crate::main_chat::write_api))
        .route("/main-notes/proposals/{id}", post(crate::main_chat::decide_api))
        .route("/user/status", get(user_status))
        .route("/sessions", get(sessions).post(create_session))
        .route("/sessions/search", get(session_search))
        .route("/sessions/stats", get(session_statistics))
        .route("/sessions/{sid}/runs/{rid}/diagnostics", get(diagnostics))
        .route("/sessions/{sid}/runs/{rid}/diagnostics/context/{attempt_id}", get(context_snapshot))
        .route("/sessions/{sid}/runs/{rid}/progress", get(progress))
        .route("/sessions/{sid}/runs/{rid}/logs", get(run_logs))
        .route("/sessions/{sid}/background-tasks/{task_id}", get(background_task_metadata))
        .route("/sessions/{id}", get(snapshot).patch(session_patch).delete(session_recycle))
        .route("/sessions/{id}/purge", post(session_purge))
        .route("/sessions/{id}/restore", post(session_restore))
        .route("/sessions/{id}/undo", post(session_undo))
        .route("/sessions/{id}/fork", post(session_fork))
        .route("/sessions/{id}/read", axum::routing::put(session_read))
        .route("/sessions/{id}/controls", get(chat_controls))
        .route("/sessions/{id}/usage", get(chat_usage))
        .route("/sessions/{id}/model", axum::routing::put(chat_model))
        .route("/sessions/{id}/mode", get(mode_get).put(mode_set))
        .route("/sessions/{id}/abilities", get(abilities))
        .route("/sessions/{id}/abilities/{ability_id}", axum::routing::put(ability_set))
        .route("/sessions/{id}/state-inspect", get(state_inspect))
        .route("/sessions/{id}/source-review", get(source_review))
        .route("/sessions/{id}/promotions", get(promotions).post(promote))
        .route("/sessions/{id}/promotions/{request_id}/review", get(promotion_review))
        .route("/sessions/{id}/promotions/{request_id}", get(promotion_receipt))
        .route("/sessions/{id}/promotions/{request_id}/reject", post(promotion_reject))
        .route("/messages",post(direct_message).layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/sessions/{id}/messages", post(send).layer(axum::extract::DefaultBodyLimit::disable()))
        .route("/sessions/{id}/messages/{mid}/delivery", axum::routing::patch(followup_delivery))
        .route("/sessions/{id}/queue/resume", post(followup_resume))
        .route("/sessions/{id}/voice/route",post(voice_route).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/voice/capabilities",get(voice_capabilities))
        .route("/sessions/{id}/voice/speech",post(voice_speech).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/voice/control",post(voice_control))
        .route("/sessions/{id}/voice/audio",post(voice_audio).layer(axum::extract::DefaultBodyLimit::max(15*1024*1024)).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/attachments",post(upload).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/attachments/{aid}", get(attachment).delete(upload_delete).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/attachments/{aid}/content",get(upload_content).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/attachments/{aid}/describe",post(upload_describe).layer(axum::middleware::from_fn(crate::api::media_lane::limit)))
        .route("/sessions/{id}/memories", get(memory_list).post(memory_save))
        .route("/sessions/{id}/memories/{mid}", axum::routing::delete(memory_delete))
        .route("/sessions/{id}/events", get(events))
        .route("/sessions/{id}/activity", get(activity))
        .route("/runs/{id}/{action}", post(command))
        .route("/approvals/{id}/decision", post(approval))
}
pub(crate) fn messaging_router() -> Router<App> {
    Router::new()
        .route("/sessions", post(create_session))
        .route("/sessions/{id}", get(snapshot))
        .route("/sessions/{id}/messages", post(send).layer(axum::extract::DefaultBodyLimit::disable()))
}

#[cfg(test)]mod embed_identity_compatibility_tests {
    use super::*;
    use axum::{body::Body,extract::Request,http::StatusCode};
    use tower::ServiceExt;
    #[tokio::test]
    async fn machine_chat_handlers_allow_absent_browser_identity() {
        let (_root,mut app)=crate::runtime_tests::fixture().await;
        let token="fixture-machine-messaging-token-0123456789";
        app.message_api_token_hash=crate::api::message_api_token_hash(token).unwrap();
        // Exercise the shared handlers beneath the machine middleware, which
        // intentionally supplies caller App without a browser identity document.
        let route=Router::new().route("/sessions",get(sessions))
            .route("/sessions/search",get(session_search)).route("/user/status",get(user_status))
            .route_layer(axum::middleware::from_fn_with_state(app.clone(),crate::api::message_api_auth)).with_state(app);
        for uri in ["/sessions","/sessions/search?q=needle","/user/status"] {
            let request=Request::builder().uri(uri).header("authorization",format!("Bearer {token}")).body(Body::empty()).unwrap();
            assert_eq!(route.clone().oneshot(request).await.unwrap().status(),StatusCode::OK);
        }
    }
}

#[cfg(test)]
mod session_statistics_tests {
    use super::*;
    use axum::{body::Body, extract::Request, http::StatusCode};
    use tower::ServiceExt;
    #[tokio::test]
    async fn statistics_require_browser_auth_and_validate_bounds() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let router = crate::router(app.clone());
        let request = |uri: &str, cookie: Option<&str>| {
            let mut builder = Request::builder().uri(uri);
            if let Some(cookie) = cookie { builder = builder.header("cookie",cookie); }
            builder.body(Body::empty()).unwrap()
        };
        assert_eq!(router.clone().oneshot(request("/api/v3/sessions/stats",None)).await.unwrap().status(),StatusCode::UNAUTHORIZED);
        let token = crate::uuid();
        app.store.put(&format!("auth:{}",crate::digest(&serde_json::json!(token))),
            &serde_json::json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let cookie = format!("v3_session={token}");
        for uri in ["/api/v3/sessions/stats?status=bad","/api/v3/sessions/stats?limit=101","/api/v3/sessions/stats?limit=0","/api/v3/sessions/stats?user_id=other"] {
            assert_eq!(router.clone().oneshot(request(uri,Some(&cookie))).await.unwrap().status(),StatusCode::BAD_REQUEST,"{uri}");
        }
        let user = app.store.for_user("admin").await.unwrap();
        user.put("session:mine",&serde_json::json!({"id":"mine","title":"Mine","agent_id":"shared_default"})).await.unwrap();
        user.put("session:revoked",&serde_json::json!({"id":"revoked","title":"Hidden","agent_id":"unavailable-agent"})).await.unwrap();
        let response = router.oneshot(request("/api/v3/sessions/stats?limit=100",Some(&cookie))).await.unwrap();
        assert_eq!(response.status(),StatusCode::OK);
        let bytes = axum::body::to_bytes(response.into_body(),1024*1024).await.unwrap();
        let result: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(result["sessions"].as_array().unwrap().len(),1);
        assert_eq!(result["sessions"][0]["id"],"mine");
        assert_eq!(result["sessions"][0]["message_count"],0);
        assert!(result["sessions"][0]["usage"].is_null());
    }
}

#[cfg(test)]
mod shared_observer_mutation_tests {
 use serde_json::json;
 use sqlx::Row;
 use tower::ServiceExt;
 #[tokio::test]
 async fn silent_observer_cannot_mutate_owner_resources() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  let account=app.store.create_user_account("observer-owned","fixture-password-hash").await.unwrap();
  let owner=account["user_id"].as_str().unwrap();
  app.store.update_account_access(owner,1,crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
  let storage=app.store.for_user(owner).await.unwrap();
  storage.put("session:observer-guard",&json!({"id":"observer-guard","agent_id":"shared_default"})).await.unwrap();
  app.store.register_shared_session(owner,"observer-guard","shared_default").await.unwrap();
  let raw=sqlx::query("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&app.store.agent_pool).await.unwrap();
  let mut meta=app.store.unseal(&raw.get::<Vec<u8>,_>("metadata")).unwrap();
  meta["shared_chat"]=json!({"admin_view":true});
  sqlx::query("UPDATE agents SET metadata=?,revision=revision+1 WHERE id='shared_default'").bind(app.store.seal(&meta).unwrap()).execute(&app.store.agent_pool).await.unwrap();
  let token=crate::uuid();
  app.store.put(&format!("auth:{}",crate::digest(&json!(token))),&json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
  let router=crate::router(app);
  for (method,suffix) in [("DELETE","memories/missing"),("DELETE","attachments/missing"),("POST","attachments/missing/describe"),("POST","promotions/missing/reject"),("POST","promotions")] {
   let request=axum::http::Request::builder().method(method).uri(format!("/api/v3/sessions/observer-guard/{suffix}"))
    .header("cookie",format!("v3_session={token}")).header("x-csrf-token","fixture").header("content-type","application/json")
    .body(axum::body::Body::from("{}")).unwrap();
   assert_eq!(router.clone().oneshot(request).await.unwrap().status(),axum::http::StatusCode::FORBIDDEN,"{suffix}");
  }
 }
}

#[cfg(test)]
mod followup_route_tests {
    use super::*;
    use axum::{body::Body, extract::Request, http::StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    #[tokio::test]
    async fn active_followup_route_requires_auth_csrf_and_never_enters_context_before_delivery() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        crate::runtime_tests::queued(&app, &sid).await;
        let token = crate::uuid();
        app.store.put(&format!("auth:{}", crate::digest(&json!(token))),
            &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let uri = format!("/api/v3/sessions/{sid}/messages");
        let body = json!({"text":"pending followup must remain outside model history","client_message_id":crate::uuid()});
        let req = |cookie: bool, csrf: bool| {
            let mut request = Request::builder().method("POST").uri(&uri).header("content-type","application/json");
            if cookie { request = request.header("cookie", format!("v3_session={token}")); }
            if csrf { request = request.header("x-csrf-token","fixture"); }
            request.body(Body::from(body.to_string())).unwrap()
        };
        assert_eq!(crate::router(app.clone()).oneshot(req(false,false)).await.unwrap().status(),StatusCode::UNAUTHORIZED);
        assert_eq!(crate::router(app.clone()).oneshot(req(true,false)).await.unwrap().status(),StatusCode::FORBIDDEN);
        assert_eq!(crate::router(app.clone()).oneshot(req(true,true)).await.unwrap().status(),StatusCode::ACCEPTED);
        assert_eq!(crate::router(app.clone()).oneshot(req(true,true)).await.unwrap().status(),StatusCode::ACCEPTED);
        let queue = app.store.followup_queue(&sid).await.unwrap();
        assert_eq!(queue["entries"].as_array().unwrap().len(),1);
        let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
        assert_eq!(chat.len(),2);
        assert!(!app.store.context(&sid).await.unwrap().iter().any(|item|item["content"]==body["text"]));
    }

    #[tokio::test]
    async fn delivery_control_cannot_cross_sessions_or_user_planes() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        crate::runtime_tests::queued(&app, &sid).await;
        let (_, Json(sent)) = crate::send(State(app.clone()),Path(sid.clone()),
            Json(json!({"text":"private queued message","client_message_id":crate::uuid()}))).await.unwrap();
        let mid = sent["submission_id"].as_str().unwrap().to_owned();
        let other = crate::runtime_tests::conversation(&app).await;
        let body = json!({"expected_revision":0,"client_request_id":crate::uuid(),"choice":"cancel"});
        assert_eq!(followup_delivery(Extension(app.clone()),Path((other,mid.clone())),Json(body.clone())).await.status(),StatusCode::NOT_FOUND);
        let mut foreign = app.clone();
        let mut account = app.store.account_by_id("admin").await.unwrap().unwrap();
        account["user_id"] = json!("foreign_fixture");
        account["username"] = json!("foreign_fixture");
        sqlx::query("INSERT INTO user_accounts(user_id,username,value) VALUES('foreign_fixture','foreign_fixture',?)")
            .bind(app.store.seal(&account).unwrap()).execute(&app.store.app_pool).await.unwrap();
        foreign.store = app.store.for_user("foreign_fixture").await.unwrap();
        assert_eq!(followup_delivery(Extension(foreign),Path((sid.clone(),mid)),Json(body)).await.status(),StatusCode::NOT_FOUND);
        assert_eq!(app.store.followup_queue(&sid).await.unwrap()["entries"][0]["status"],"pending");
    }
}
