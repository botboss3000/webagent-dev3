//! Permanent coordination identity and cross-session notes. Note content is
//! untrusted evidence; neither approving a topic nor reviewing it grants effects.
use crate::*;
use axum::Extension;

pub(crate) use webagent_chat::main_chat::{action_intent, empty, review_id, scheduled_entry_current, Contribution, Settings};
use webagent_chat::main_chat::{public, request_fields, target};
// The Main Notes state machine is Chat-owned; the host supplies the clock.
fn apply(state:&mut Value,c:&Contribution,origin:&str,approved:bool)->Result<Value> {webagent_chat::main_chat::apply(state,c,origin,approved,now())}
fn contribute(state:&mut Value,c:&Contribution,origin:&str,proposal_id:&str,human:bool)->Result<Value> {
    webagent_chat::main_chat::contribute(state,c,origin,proposal_id,human,now())
}
fn error(error:anyhow::Error)->ApiError {ApiError(StatusCode::CONFLICT,error.to_string())}

fn fallback(app:&App,name:&str)->Option<String> {fallback_store(&app.store,name)}
fn fallback_store(store:&Store,name:&str)->Option<String> {
    // Only JSON is authoritative. Missing/malformed guidance never gates runtime tools.
    crate::live_assets::main_chat_prompts(&store.root).ok()
        .and_then(|v|v[name].as_str().filter(|s|!s.trim().is_empty()).map(str::to_owned))
}
fn skill_view(app:&App,state:&Value)->Value {
    let mut skills=json!({});
    for name in ["contribute","review"]{skills[name]=json!({"override":state["skill_overrides"][name],"fallback":fallback(app,name)});}
    skills
}
pub(crate) async fn guidance(app:&App,agent:&str,sid:&str)->Result<Option<String>> {guidance_store(&app.store,agent,sid).await}
pub(crate) async fn guidance_store(store:&Store,agent:&str,sid:&str)->Result<Option<String>> {
    let store=store.for_agent(agent).await?;
    let state=Box::pin(store.main_chat_state()).await?;
    if state["settings"]["enabled"]!=true{return Ok(None);}
    let name=if state["session_id"]==sid{"review"}else{"contribute"};
    Ok(state["skill_overrides"][name].as_str().filter(|s|!s.trim().is_empty()).map(str::to_owned).or_else(||fallback_store(&store,name)))
}
async fn agent_scope(app:&App,identity:&Value,id:&str,manage:bool)->std::result::Result<(App,Value),ApiError> {
    let actor=identity["_user_id"].as_str().ok_or_else(||ApiError(StatusCode::FORBIDDEN,"Agent access unavailable".into()))?;
    let rights=app.store.agent_authority(actor,id).await.map_err(|_|ApiError(StatusCode::NOT_FOUND,"Agent not found".into()))?;
    if manage&&rights["can_manage"]!=true || !manage&&rights["can_manage"]!=true&&!app.store.live_access(actor,id).await.is_ok_and(|a|a.admitted()){return Err(ApiError(StatusCode::FORBIDDEN,"Agent access unavailable".into()));}
    let mut scoped=app.clone();
    scoped.store=app.store.for_agent(id).await?.main_chat_owner().await?;
    Ok((scoped,rights))
}
pub(crate) async fn agent_read(Extension(app):Extension<App>,Extension(identity):Extension<Value>,Path(id):Path<String>)->Api {
    let (app,rights)=agent_scope(&app,&identity,&id,false).await?;
    let mut state=read_api(Extension(app)).await?.0;
    state["can_manage"]=rights["can_manage"].clone();
    Ok(Json(state))
}
pub(crate) async fn agent_configure(Extension(app):Extension<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<Value>)->Api {
    let (app,_)=agent_scope(&app,&identity,&id,true).await?;
    configure_api(Extension(app),Json(body)).await
}
pub(crate) async fn agent_write(Extension(app):Extension<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<Value>)->Api {
    let (app,_)=agent_scope(&app,&identity,&id,true).await?;
    write_api(Extension(app),Json(body)).await
}
pub(crate) async fn agent_decide(Extension(app):Extension<App>,Extension(identity):Extension<Value>,Path((agent,id)):Path<(String,String)>,Json(body):Json<Value>)->Api {
    let (app,_)=agent_scope(&app,&identity,&agent,true).await?;
    decide_api(Extension(app),Path(id),Json(body)).await
}
pub(crate) async fn read_api(Extension(app):Extension<App>)->Api {
    let mut state=app.store.main_chat_state().await?;
    if let Some(sid)=state["session_id"].as_str() {
        if let Some(session)=app.store.get(&key("session",sid)).await? {state["settings"]["chat_name"]=session["title"].clone();state["session_metadata_revision"]=session["metadata_revision"].clone();}
    }
    let automation=app.store.module_state("automation").await?;
    state["review_status"]=json!(if state["settings"]["enabled"]!=true {"disabled"}
        else if !crate::modules::available("automation") {"automation_unavailable"}
        else {automation["state"]["definitions"][review_id(app.store.current_agent_id())]["status"].as_str().unwrap_or("awaiting_subscription")});
    state["agent_id"]=json!(app.store.current_agent_id());
    state["skills"]=skill_view(&app,&state);
    Ok(Json(public(&state)))
}
pub(crate) async fn configure_api(Extension(app):Extension<App>,Json(body):Json<Value>)->Api {
    let (expected,request)=request_fields(&body).map_err(error)?;
    let settings:Settings=serde_json::from_value(body["settings"].clone()).map_err(|_|bad("Invalid Main Chat settings"))?;
    settings.validate().map_err(error)?;
    let overrides=body["skill_overrides"].as_object().ok_or_else(||bad("Skill overrides required"))?;
    if overrides.keys().any(|k|!["contribute","review"].contains(&k.as_str())){return Err(bad("Unknown Main Chat skill"));}
    let mut skills=json!({});
    for name in ["contribute","review"] {
        skills[name]=match overrides.get(name) {
            Some(Value::Null)=>Value::Null,
            Some(Value::String(text)) if text.len()<=65536=>if text.trim().is_empty(){Value::Null}else{json!(text)},
            _=>return Err(bad("Invalid Main Chat skill override")),
        };
    }
    let _guard=app.gate.lock().await;
    let previous=app.store.main_chat_state().await?;
    if previous["settings"]["enabled"]==true&&!settings.enabled&&body["confirm_deactivation"]!=true{return Err(bad("Confirm deactivation: this session becomes a normal chat; enabling again creates a new Main Chat"));}
    let mut extra=vec![];
    let mut sid=previous["session_id"].as_str().map(str::to_owned);
    if settings.enabled&&sid.is_none() {
        let mut prepared=crate::api::chat_sessions::prepare_session_id(&app,&json!({"title":settings.chat_name,"agent_id":app.store.current_agent_id()}),None).await?;
        sid=prepared.result["id"].as_str().map(str::to_owned);
        prepared.docs[0].1["main_chat"]=json!(true);
        prepared.docs[0].1["main_chat_agent_id"]=json!(app.store.current_agent_id());
        extra=prepared.docs;
        extra.push((key("session_ability_state",sid.as_deref().unwrap_or("")),json!({"revision":0,"active":["automation"],"suppressed":[]})));
    }
    let result=app.store.main_chat_change(expected,&request,&digest(&body),extra,|state| {
        state["settings"]=serde_json::to_value(&settings)?;
        state["skill_overrides"]=skills.clone();
        state["_rename_expected_revision"]=body["expected_session_revision"].clone();
        state["session_id"]=if settings.enabled {json!(sid)}else{Value::Null};
        Ok(json!({"configured":true,"agent_id":app.store.current_agent_id(),"session_id":state["session_id"],"revision":expected+1}))
    }).await.map_err(error)?;
    if let Some(sid)=result["session_id"].as_str() {app.store.register_shared_session(app.store.current_user_id(),sid,app.store.current_agent_id()).await?;}
    wake::notify(&app).await;
    Ok(Json(result))
}
pub(crate) async fn write_api(Extension(app):Extension<App>,Json(body):Json<Value>)->Api {
    let (expected,request)=request_fields(&body).map_err(error)?;
    let c:Contribution=serde_json::from_value(body["contribution"].clone()).map_err(|_|bad("Invalid contribution"))?;
    let sid="human".to_owned();
    let result=app.store.main_chat_change(expected,&request,&digest(&body),vec![],|state|contribute(state,&c,&sid,&request,true)).await.map_err(error)?;
    wake::notify(&app).await;Ok(Json(result))
}
pub(crate) async fn decide_api(Extension(app):Extension<App>,Path(id):Path<String>,Json(body):Json<Value>)->Api {
    let (expected,request)=request_fields(&body).map_err(error)?;
    let accept=body["approve"].as_bool().ok_or_else(||bad("Approval decision required"))?;
    let result=app.store.main_chat_change(expected,&request,&digest(&json!([id,body])),vec![],|state| {
        let proposal=state["proposals"][&id].clone();
        if proposal["status"]!="pending" {return Err(anyhow!("Proposal is not pending"));}
        let c:Contribution=serde_json::from_value(proposal["contribution"].clone())?;
        if accept {
                    if state["topics"][target(state,&c)]["revision"].as_u64().unwrap_or(0)!=proposal["topic_revision"].as_u64().unwrap_or(0) {
                return Err(anyhow!("Topic changed; review a fresh proposal"));
            }
            let result=apply(state,&c,proposal["origin_session_id"].as_str().unwrap_or(""),true)?;
            state["proposals"][&id]["status"]=json!("approved");Ok(result)
        } else {state["proposals"][&id]["status"]=json!("rejected");Ok(json!({"status":"rejected"}))}
    }).await.map_err(error)?;
    wake::notify(&app).await;Ok(Json(result))
}
pub(crate) use webagent_chat::main_chat::{owns, schemas};
pub(crate) async fn tool(app:&App,sid:&str,_rid:&str,name:&str,args:&Value)->Result<Value> {
    // Reuse the private cross-session owner boundary; shared membership grants no notes authority.
    let actor=crate::shared_chat::actor(app);
    if actor!=app.store.current_user_id() {return Err(anyhow!("Shared chat membership does not authorize private Main Notes tools"));}
    let agent=session_agent_id(app,sid).await?;
    let store=app.store.for_user(actor).await?.for_agent(&agent).await?;
    let access=store.live_access(actor,&agent).await?;
    if !access.admitted() {return Err(anyhow!("Main Notes access revoked"));}
    if name=="main_notes_read" {
        let state=store.main_chat_state().await?;
        let offset=args["offset"].as_u64().unwrap_or(0) as usize;
        let limit=args["limit"].as_u64().unwrap_or(20) as usize;
        if !(1..=20).contains(&limit)||offset>65536 {return Err(anyhow!("Invalid Main Notes page"));}
        let (items,total)=if let Some(topic)=args["topic_id"].as_str() {
            let rows:Vec<Value>=state["topics"][topic]["entries"].as_object().into_iter().flatten().map(|(_,v)|v.clone()).collect();
            (rows.iter().skip(offset).take(limit).cloned().collect::<Vec<_>>(),rows.len())
        } else {
            let rows:Vec<Value>=state["topics"].as_object().into_iter().flatten().map(|(id,v)|json!({"id":id,"title":v["title"],"scope":v["scope"],"approved_scope":v["approved_scope"],"groups":v["groups"],"revision":v["revision"],"entry_count":v["entries"].as_object().map_or(0,|e|e.len())})).collect();
            (rows.iter().skip(offset).take(limit).cloned().collect::<Vec<_>>(),rows.len())
        };
        return Ok(json!({"status":"success","source_kind":"untrusted_main_notes","output":{"revision":state["revision"],"notes_version":state["notes_version"],"settings":state["settings"],"session_id":state["session_id"],"items":items,"total":total,"next_offset":offset+items.len(),"has_more":offset+items.len()<total}}));
    }
    let (expected,request)=request_fields(args)?;
    let c:Contribution=serde_json::from_value(args["contribution"].clone())?;
    let result=store.main_chat_change(expected,&request,&digest(&json!([sid,args])),vec![],|state|contribute(state,&c,sid,&request,false)).await?;
    wake::notify(app).await;Ok(json!({"status":"success","output":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn contribution()->Contribution {serde_json::from_value(json!({"topic_id":"goal","title":"Goal","scope":"Track deployment blockers","entry":{"id":"next","text":"Review the blocker","state":"open","sources":["session:source"]}})).unwrap()}
    #[tokio::test]
    async fn storage_receipts_isolation_events_and_restart() {
        let (root,app)=crate::runtime_tests::fixture().await;let store=app.store.clone();
        let docs=vec![("session:main".into(),json!({"id":"main","agent_id":"shared_default","main_chat":true,"main_chat_agent_id":"shared_default"}))];
        store.main_chat_change(0,"init","init",docs,|s|{s["settings"]["enabled"]=json!(true);s["session_id"]=json!("main");Ok(json!({"initialized":true}))}).await.unwrap();
        let c=contribution();
        let output=store.main_chat_change(1,"note","same",vec![],|s|contribute(s,&c,"human","p",true)).await.unwrap();
        let duplicate=store.main_chat_change(0,"note","same",vec![],|_|panic!("duplicate must not apply")).await.unwrap();
        assert_eq!(output,duplicate);
        assert!(store.main_chat_change(2,"note","changed",vec![],|_|Ok(Value::Null)).await.is_err());
        let count:i64=sqlx::query_scalar("SELECT count(*) FROM notification_stage WHERE type='main_notes.updated'").fetch_one(&store.pool).await.unwrap();assert_eq!(count,1);
        let mut progress=c.clone();progress.entry.state="blocked".into();progress.entry.progress=Some("Verified".into());
        let quiet=store.main_chat_change(2,"quiet","quiet",vec![],|s|contribute(s,&progress,"main","quiet",true)).await.unwrap();
        assert_eq!(quiet["change"]["review_required"],false);
        let notifications:i64=sqlx::query_scalar("SELECT count(*) FROM notification_stage WHERE type='main_notes.updated'").fetch_one(&store.pool).await.unwrap();assert_eq!(notifications,2);
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='main_chat'").fetch_one(&store.agent_pool).await.unwrap();assert!(serde_json::from_slice::<Value>(&raw).is_err());
        let other=store.create_guest_account().await.unwrap();let other=store.for_user(other["user_id"].as_str().unwrap()).await.unwrap();
        assert_eq!(other.main_chat_state().await.unwrap()["notes_version"],2);
        let restored=Store::open(root.path()).await.unwrap();assert_eq!(restored.main_chat_state().await.unwrap()["notes_version"],2);
        restored.put("session:main",&json!({"id":"main","checkpoint_input":[],"checkpoint_through":100})).await.unwrap();
        assert_eq!(restored.get("session:main").await.unwrap().unwrap()["main_chat"],true);
        assert_eq!(restored.main_chat_state().await.unwrap()["notes_version"],2);
        let req=crate::store::session::SessionLifecycle{client_request_id:"delete".into(),expected_revision:0};
        assert!(matches!(restored.lifecycle_session("main",&req,false).await.unwrap(),crate::store::session::SessionPatchOutcome::Conflict(_)));
        assert!(matches!(restored.purge_session("main",&req).await.unwrap(),crate::store::session::SessionPatchOutcome::Conflict(_)));
        let a=store.clone();let b=restored.clone();
        let (a,b)=tokio::join!(a.main_chat_change(3,"a","a",vec![],|s|{s["settings"]["notes_name"]=json!("A");Ok(Value::Null)}),b.main_chat_change(3,"b","b",vec![],|s|{s["settings"]["notes_name"]=json!("B");Ok(Value::Null)}));
        assert_ne!(a.is_ok(),b.is_ok());
    }
}

#[cfg(test)]
mod api_tests {
    use super::*;
    async fn second(app:&App)->App {
        let pool=crate::agent_storage::open_agent_pool(&app.store.root,"agent-two").await.unwrap();
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES('agent-two','Second','','openai_subscription','fixture','active',?,1,0,0)")
            .bind(app.store.seal(&json!({"owner_id":"admin","ability_revision":0})).unwrap()).execute(&pool).await.unwrap();
        pool.close().await;
        app.store.register_agent("agent-two","admin").await.unwrap();
        app.store.set_agent_membership("admin","agent-two",crate::access::Relationship::Owner,0).await.unwrap();
        let mut scoped=app.clone();scoped.store=app.store.for_agent("agent-two").await.unwrap();scoped
    }
    fn config(revision:u64,session_revision:Value,request:&str,enabled:bool)->Value {
        json!({"expected_revision":revision,"expected_session_revision":session_revision,"request_id":request,
            "settings":{"enabled":enabled,"chat_name":"Coordinator","notes_name":"Agent Notes","write_mode":"new_topics"},
            "skill_overrides":{"contribute":"Contribute sentinel","review":"Review sentinel"}})
    }
    #[tokio::test]
    async fn agent_isolation_disable_fresh_identity_and_receipts() {
        let (_root,app)=crate::runtime_tests::fixture().await;let other=second(&app).await;
        assert_eq!(read_api(Extension(other.clone())).await.unwrap().0["settings"]["enabled"],false);
        let body=config(0,Value::Null,"enable",true);
        let first=configure_api(Extension(app.clone()),Json(body.clone())).await.unwrap().0;
        let sid=first["session_id"].as_str().unwrap();
        assert_eq!(configure_api(Extension(app.clone()),Json(body)).await.unwrap().0,first);
        let second=configure_api(Extension(other.clone()),Json(config(0,Value::Null,"enable",true))).await.unwrap().0;
        assert_ne!(first["session_id"],second["session_id"]);
        let c=json!({"topic_id":"topic","title":"Topic","scope":"Track a goal","entry":{"id":"action","text":"Ask about the goal","state":"open"}});
        write_api(Extension(app.clone()),Json(json!({"expected_revision":1,"request_id":"human-note","contribution":c}))).await.unwrap();
        assert!(other.store.main_chat_state().await.unwrap()["topics"].as_object().unwrap().is_empty());
        let patch=crate::store::session::SessionPatch{client_request_id:"rename".into(),expected_revision:0,title:Some("Renamed".into()),pinned:Some(true)};
        assert!(matches!(app.store.patch_session(sid,&patch).await.unwrap(),crate::store::session::SessionPatchOutcome::Saved(_)));
        assert_eq!(read_api(Extension(app.clone())).await.unwrap().0["settings"]["chat_name"],"Renamed");
        assert_eq!(guidance(&app,"shared_default",sid).await.unwrap().as_deref(),Some("Review sentinel"));
        assert_eq!(guidance(&app,"shared_default","working").await.unwrap().as_deref(),Some("Contribute sentinel"));
        let disable=config(2,json!(1),"disable",false);
        assert!(configure_api(Extension(app.clone()),Json(disable.clone())).await.is_err());
        let mut stale=disable.clone();stale["confirm_deactivation"]=json!(true);stale["expected_session_revision"]=json!(0);
        assert!(configure_api(Extension(app.clone()),Json(stale)).await.is_err());
        assert_eq!(app.store.main_chat_state().await.unwrap()["settings"]["enabled"],true);
        let mut disable=disable;disable["confirm_deactivation"]=json!(true);
        configure_api(Extension(app.clone()),Json(disable.clone())).await.unwrap();
        configure_api(Extension(app.clone()),Json(disable)).await.unwrap();
        let session=app.store.get(&key("session",sid)).await.unwrap().unwrap();
        assert_eq!(session["main_chat"],false);assert_eq!(session["pinned"],true);
        let mut stale_checkpoint=session.clone();stale_checkpoint["main_chat"]=json!(true);
        app.store.put(&key("session",sid),&stale_checkpoint).await.unwrap();
        assert_eq!(app.store.get(&key("session",sid)).await.unwrap().unwrap()["main_chat"],false);
        assert!(app.store.main_chat_state().await.unwrap()["session_id"].is_null());
        assert!(guidance(&app,"shared_default",sid).await.unwrap().is_none());
        write_api(Extension(app.clone()),Json(json!({"expected_revision":3,"request_id":"disabled-note","contribution":c}))).await.unwrap();
        let enabled=configure_api(Extension(app.clone()),Json(config(4,Value::Null,"reenable",true))).await.unwrap().0;
        assert_ne!(enabled["session_id"],sid);
        assert_eq!(app.store.main_chat_state().await.unwrap()["notes_version"],2);
        let lifecycle=crate::store::session::SessionLifecycle{client_request_id:"demoted-delete".into(),expected_revision:2};
        assert!(matches!(app.store.lifecycle_session(sid,&lifecycle,false).await.unwrap(),crate::store::session::SessionPatchOutcome::Saved(_)));
        let active=enabled["session_id"].as_str().unwrap();
        assert!(matches!(app.store.lifecycle_session(active,&crate::store::session::SessionLifecycle{client_request_id:"protected".into(),expected_revision:0},false).await.unwrap(),crate::store::session::SessionPatchOutcome::Conflict(_)));
    }
    #[tokio::test]
    async fn agent_authority_and_live_override_clear() {
        let (_root,app)=crate::runtime_tests::fixture().await;let other=second(&app).await;
        let guest=app.store.create_guest_account().await.unwrap();
        let identity=json!({"_user_id":guest["user_id"]});
        assert!(agent_scope(&app,&identity,"agent-two",true).await.is_err());
        assert!(agent_scope(&app,&identity,"agent-two",false).await.is_err());
        assert!(agent_scope(&app,&json!({"_user_id":"admin"}),"agent-two",true).await.is_ok());
        configure_api(Extension(other.clone()),Json(config(0,Value::Null,"enable",true))).await.unwrap();
        let mut updated=config(1,json!(0),"override",true);
        updated["skill_overrides"]=json!({"contribute":"Updated sentinel","review":null});
        configure_api(Extension(other.clone()),Json(updated)).await.unwrap();
        assert_eq!(guidance(&other,"agent-two","working").await.unwrap().as_deref(),Some("Updated sentinel"));
        assert!(app.store.main_chat_state().await.unwrap()["skill_overrides"]["contribute"].is_null());
        std::fs::create_dir_all(app.store.root.join("config")).unwrap();
        std::fs::write(app.store.root.join("config/prompts.json"),br#"{ "main_chat_skills": {} }"#).unwrap();
        let main=other.store.main_chat_state().await.unwrap()["session_id"].as_str().unwrap().to_owned();
        assert!(guidance(&other,"agent-two",&main).await.unwrap().is_none());
        assert!(tool(&other,&main,"none","main_notes_read",&json!({})).await.is_ok());
        std::fs::write(app.store.root.join("config/prompts.json"),br#"{ "main_chat_skills": { "review":"Live JSON sentinel" } }"#).unwrap();
        assert_eq!(guidance(&other,"agent-two",&main).await.unwrap().as_deref(),Some("Live JSON sentinel"));
    }
}
