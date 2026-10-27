//! Agent Operator roster endpoints; app role never grants tenant authority.
use super::*;
use axum::Extension;
use sqlx::Row;
use std::collections::BTreeMap;
async fn operator(app:&App,identity:&Value,agent:&str)->Result<Store,ApiError> {
    let actor=identity["_user_id"].as_str().ok_or_else(||bad("Sign in required"))?;
    if !app.store.agent_authority(actor,agent).await.is_ok_and(|r|r["can_manage"]==true) {
        return Err(ApiError(StatusCode::NOT_FOUND,"Agent not found".into()));
    }
    Ok(app.store.for_agent(agent).await?)
}
async fn read(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Query(query):Query<HashMap<String,String>>)->Api {
    let store=operator(&app,&identity,&id).await?;
    if let Some(username)=query.get("username") {
        if username.is_empty() || username.len()>128 {return Err(bad("Exact username required"));}
        let account=store.account_by_username(username).await?.filter(|a|a["role"]!="guest")
            .ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Account not found".into()))?;
        return Ok(Json(json!({"user":{"user_id":account["user_id"],"username":account["username"]}})));
    }
    let offset=query.get("offset").map(|v|v.parse::<usize>()).transpose().map_err(|_|bad("Invalid offset"))?.unwrap_or(0);
    let members=store.agent_memberships(&id).await?;
    let total=members.len();let mut page=Vec::new();
    for mut member in members.into_iter().skip(offset).take(100) {
        let user=member["user_id"].as_str().unwrap_or("");
        let effective=store.access_repository().resolved_membership(user,&id).await?;
        member["effective_level_id"]=effective.as_ref().map(|v|v["capability_level_id"].clone()).unwrap_or(Value::Null);
        member["access_origin"]=effective.as_ref().map(|v|v["origin"].clone()).unwrap_or_else(||json!("none"));
        if let Some(user)=member["user_id"].as_str() {
            if let Some(account)=store.account_by_id(user).await? {member["username"]=account["username"].clone();}
        }
        page.push(member);
    }
    Ok(Json(json!({"members":page,"next_offset":if offset+100<total {Some(offset+100)}else{None},
        "capability_levels":store.capability_levels(&id).await?,"owner_id":store.agent_authority(identity["_user_id"].as_str().unwrap(),&id).await?["owner_id"]})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberUpdate {user_id:String,level_id:Option<String>,#[serde(default)] blocked:bool,expected_revision:u64}
async fn update(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<MemberUpdate>)->Api {
    let _guard=app.gate.lock().await;
    let store=operator(&app,&identity,&id).await?;
    let member=store.access_repository().manage_agent_member(identity["_user_id"].as_str().unwrap(),&body.user_id,&id,body.level_id.as_deref(),body.blocked,body.expected_revision).await
        .map_err(|_|conflict("Membership changed or appointment rejected; reload before retrying"))?;
    crate::phase3_api::quiesce_user(&app,&body.user_id,Some(&id)).await?;
    Ok(Json(json!({"member":member})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LadderUpdate {expected_revision:u64,ladder:crate::capabilities::CapabilityLevels,#[serde(default)]confirm_expansion:bool,#[serde(default)]reassignments:BTreeMap<String,String>}
async fn ladder(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(mut body):Json<LadderUpdate>)->Api {
    let _guard=app.gate.lock().await;
    let store=operator(&app,&identity,&id).await?;
    body.ladder.validate().map_err(|_|bad("Invalid capability ladder"))?;
    let previous=store.capability_levels(&id).await?;
    for (level_id,next) in &body.ladder.levels {
        let old=previous.levels.get(level_id).map(|v|serde_json::to_value(v)).transpose().map_err(anyhow::Error::from)?.unwrap_or(json!({}));
        super::ability_admin::grant_ceiling(&app,&identity,&serde_json::to_value(&next.ability_permissions).map_err(anyhow::Error::from)?,
            &serde_json::to_value(&next.tool_permissions).map_err(anyhow::Error::from)?,&old["ability_permissions"],&old["tool_permissions"],false).await?;
    }
    let mut tx=store.agent_pool.begin().await.map_err(anyhow::Error::from)?;
    sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(&id).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    let raw:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(&id).fetch_one(&mut *tx).await.map_err(anyhow::Error::from)?;
    let mut metadata=store.unseal_agent(&raw)?;
    let before=crate::capabilities::CapabilityLevels::from_metadata(&metadata)?;
    if before.revision!=body.expected_revision || body.ladder.revision!=body.expected_revision {return Err(conflict("Capability ladder changed"));}
    let actor=identity["_user_id"].as_str().unwrap();
    let owner=metadata["owner_id"].as_str().unwrap_or(if id=="shared_default" {"admin"} else {""});
    let actor_blob:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?").bind(actor).bind(&id).fetch_optional(&mut *tx).await.map_err(anyhow::Error::from)?;
    if actor!=owner && !actor_blob.as_ref().map(|b|store.unseal_agent(b)).transpose()?.is_some_and(|v|v["relationship"]=="member"&&v["agent_administrator"]==true) {return Err(ApiError(StatusCode::FORBIDDEN,"Agent Operator required".into()));}
    let expansion=before.order!=body.ladder.order && before.order.iter().filter(|level|body.ladder.levels.contains_key(*level)).any(|level| {
        let old=before.effective(level).ok();let next=body.ladder.effective(level).ok();
        old.zip(next).is_some_and(|(a,b)|a!=b)
    });
    if expansion && !body.confirm_expansion {return Err(conflict("Reordering changes effective access; preview and confirm"));}
    let rows=sqlx::query("SELECT user_id,value FROM agent_memberships WHERE agent_id=?").bind(&id).fetch_all(&mut *tx).await.map_err(anyhow::Error::from)?;
    for row in rows {
        let mut membership=store.unseal_agent(&row.get::<Vec<u8>,_>("value"))?;
        let level=crate::capabilities::membership_level(Some(&membership),serde_json::from_value(membership["relationship"].clone()).map_err(anyhow::Error::from)?)?;
        if body.ladder.levels.contains_key(&level){continue;}
        let replacement=body.reassignments.get(&level).filter(|r|r.as_str()!="operator"&&body.ladder.levels.contains_key(*r)).ok_or_else(||bad("Choose a replacement for every removed assigned level"))?;
        membership["capability_level_id"]=json!(replacement);
        if membership["relationship"]!="revoked" && membership["origin"]!="removed" {
            membership["relationship"]=json!(if replacement=="visitor" {"caller"} else {"member"});
        }
        membership["agent_administrator"]=json!(false);
        membership["revision"]=json!(membership["revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||bad("Membership revision overflow"))?);
        sqlx::query("UPDATE agent_memberships SET value=? WHERE user_id=? AND agent_id=?").bind(store.seal_agent(&membership)?).bind(row.get::<String,_>("user_id")).bind(&id).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    }
    body.ladder.revision=body.expected_revision.checked_add(1).ok_or_else(||bad("Ladder revision overflow"))?;
    metadata.as_object_mut().ok_or_else(||anyhow!("Invalid agent metadata"))?.remove("ability_permissions");metadata.as_object_mut().unwrap().remove("tool_permissions");
    metadata["capability_levels"]=serde_json::to_value(&body.ladder).map_err(anyhow::Error::from)?;
    sqlx::query("UPDATE agents SET metadata=?,revision=revision+1 WHERE id=?").bind(store.seal_agent(&metadata)?).bind(&id).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(format!("agent:{id}:ladder_audit:{}",body.ladder.revision))
        .bind(store.seal_agent(&json!({"actor_id":actor,"revision":body.ladder.revision,"order":body.ladder.order}))?).execute(&mut *tx).await.map_err(anyhow::Error::from)?;
    tx.commit().await.map_err(anyhow::Error::from)?;
    crate::phase3_api::quiesce_agent(&app,&id).await?;
    Ok(Json(json!({"capability_levels":body.ladder})))
}
async fn summary(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>)->Api {
    let user=identity["_user_id"].as_str().unwrap_or("");
    if !app.store.live_access(user,&id).await.is_ok_and(|a|a.admitted()) {return Err(ApiError(StatusCode::NOT_FOUND,"Agent unavailable".into()));}
    let store=app.store.for_agent(&id).await?;
    Ok(Json(json!({"conversation_privacy":store.access_repository().member_access(&id).await?.conversation_privacy,
        "tools_notice":app.store.capability_levels(&id).await?.tools_notice()})))
}
pub fn router()->Router<App> {
    Router::new().route("/agents/{id}/members",get(read).put(update))
        .route("/agents/{id}/capability-ladder",put(ladder))
        .route("/agents/{id}/access-summary",get(summary))
}

#[cfg(test)]
mod reassignment_tests {
    use super::*;
    #[tokio::test]
    async fn deleting_a_level_reassigns_visitor_relationship_without_unblocking_or_restoring_removed_users() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let identity=json!({"_user_id":"admin"});let agent="shared_default";
        let mut levels=app.store.capability_levels(agent).await.unwrap();
        let mut custom=levels.levels["member"].clone();custom.id="custom".into();custom.name="Custom".into();
        levels.levels.insert("custom".into(),custom);levels.order.insert(1,"custom".into());
        let Json(saved)=ladder(State(app.clone()),Extension(identity.clone()),Path(agent.into()),Json(LadderUpdate{
            expected_revision:levels.revision,ladder:levels,confirm_expansion:true,reassignments:BTreeMap::new()})).await.unwrap();
        let mut users=Vec::new();
        for name in ["explicit","blocked","removed"] {
            let account=app.store.create_user_account(name,"fixture-password-hash").await.unwrap();
            let user=account["user_id"].as_str().unwrap().to_owned();
            app.store.access_repository().manage_agent_member("admin",&user,agent,Some("custom"),false,0).await.unwrap();
            if name!="explicit" {app.store.access_repository().manage_agent_member("admin",&user,agent,None,name=="blocked",1).await.unwrap();}
            users.push(user);
        }
        let mut levels:crate::capabilities::CapabilityLevels=serde_json::from_value(saved["capability_levels"].clone()).unwrap();
        levels.levels.remove("custom");levels.order.retain(|id|id!="custom");
        ladder(State(app.clone()),Extension(identity),Path(agent.into()),Json(LadderUpdate{
            expected_revision:levels.revision,ladder:levels,confirm_expansion:true,reassignments:BTreeMap::from([("custom".into(),"visitor".into())])})).await.unwrap();
        for (index,user) in users.iter().enumerate() {
            let member=app.store.agent_membership(user,agent).await.unwrap().unwrap();
            assert_eq!(member["capability_level_id"],"visitor");
            assert_eq!(member["relationship"],if index==1 {"revoked"} else {"caller"});
            assert_eq!(member["agent_administrator"],false);
            assert_eq!(member["origin"],["explicit","blocked","removed"][index]);
        }
    }
}
