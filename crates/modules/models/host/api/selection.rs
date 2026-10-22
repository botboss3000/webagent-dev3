//! Agent-scoped model administration. Installation oversight never implies credential ownership.
pub use webagent_models::api::selection::{ApiKey,Discovery};
use crate::State;
use crate::{App, Api, ApiError};
use crate::access::{Principal, Relationship};
use crate::model_selection::Catalog;
use axum::{extract::{Path, Query}, Extension, Json, Router, routing::get, http::StatusCode};
use serde_json::{json, Value};
use std::collections::HashMap;

// MODEL-IDENTITY: reload account status before trusting the authenticated identity.
async fn principal(app:&App,identity:&Value)->Result<Principal,ApiError> { webagent_models::api::selection::principal(&app.models_runtime(),identity).await.map_err(Into::into) }
fn denied() -> ApiError { ApiError(StatusCode::FORBIDDEN, "Agent or model administration unavailable".into()) }
fn selected(params: &HashMap<String,String>) -> &str {
    params.get("agent_id").map(String::as_str).unwrap_or("shared_default")
}
// AGENT-AUTHORITY: resolve the requested tenant before opening model records.
async fn authority(app:&App,identity:&Value,agent:&str,write:bool)->Result<Value,ApiError> { webagent_models::api::selection::authority(&app.models_runtime(),identity,agent,write).await.map_err(Into::into) }
// This projection contains only caller-owned records and grants for the selected agent.
// Unowned grants to other tenants are deliberately absent and cannot be overwritten.

// MODEL-VISIBILITY: members receive only roster entries within their live tier ceiling.

// MODEL-READ: the catalog projection excludes other tenants and private credentials.
async fn read(State(app): State<App>, Extension(identity): Extension<Value>,
    Query(params): Query<HashMap<String,String>>) -> Api { webagent_models::api::selection::read(&app.models_runtime(),identity,params).await.map_err(Into::into) }
// MODEL-WRITE: serialize scoped merges; catalog and record revisions fence stale editors.
async fn save(State(app): State<App>, Extension(identity): Extension<Value>,
    Query(params): Query<HashMap<String,String>>, Json(input): Json<Catalog>) -> Api { webagent_models::api::selection::save(&app.models_runtime(),identity,params,input).await.map_err(Into::into) }
// AGENT-DISCOVERY: list only agents the caller can use or explicitly administer.
async fn agents(State(app): State<App>, Extension(identity): Extension<Value>) -> Api {
    use sqlx::Row;
    let caller=principal(&app,&identity).await?;
    let mut values=Vec::new();
    for id in app.store.agent_ids().await? {
        let Ok(rights)=authority(&app,&identity,&id,false).await else {continue};
        let view=app.store.for_agent(&id).await?;
        let row=sqlx::query("SELECT name,description,status,revision FROM agents WHERE id=?")
            .bind(&id).fetch_one(&view.agent_pool).await.map_err(anyhow::Error::from)?;
        values.push(json!({"id":id,"can_manage":rights["can_manage"],"name":row.get::<String,_>("name"),"description":row.get::<String,_>("description"),
            "status":row.get::<String,_>("status"),"revision":row.get::<i64,_>("revision")}));
    }
    // Even a caller with no memberships can create their first owned agent.
    Ok(Json(json!({"agents":values,"caller_id":caller.user_id,"can_create_agent":caller.role != crate::access::Role::Guest})))
}

// IDENTITY-WRITE: identity and named execution modes; tenant authority and revision fence edits.
async fn identity_get(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>) -> Api {
    use sqlx::Row;
    let rights = authority(&app, &identity, &id, true).await?;
    let store = app.store.for_agent(&id).await?;
    let row = sqlx::query("SELECT name,description,revision FROM agents WHERE id=?")
        .bind(&id).fetch_one(&store.agent_pool).await.map_err(anyhow::Error::from)?;
    let config=crate::execution_modes::config(&store,&id).await?;
    let defaults=crate::context_compaction::Config::read(&app.compaction_config_path)?;
    let override_settings=crate::context_compaction::agent_thresholds(&store,&id).await?;
    // Keep Identity readable if a later app-default edit invalidates an override.
    let effective=override_settings.clone().unwrap_or(crate::context_compaction::AgentThresholds{
        trigger_tokens:defaults.trigger_tokens,tail_tokens:defaults.tail_tokens});
    let compaction_valid=effective.apply(&defaults).is_ok();
    let tool_output_defaults=crate::tool_output::Policy::read(&store.root)?;
    let tool_output_override=crate::tool_output::agent_override(&store,&id).await?;
    let tool_output_overridden=tool_output_override.is_some();
    let tool_output_policy=match tool_output_override {
        Some(value)=>serde_json::from_value::<crate::tool_output::Policy>(value)
            .map_err(anyhow::Error::from)?,
        None=>tool_output_defaults.clone(),
    };
    tool_output_policy.validate()?;
    let tool_output_eligible_tools=crate::tool_output::eligible_tools();
    let settings=crate::gate_controls::agent_visibility(&store,&id).await?;
    let closing=crate::gate_controls::agent_closing(&store,&id).await?;
    let manager=crate::manager::config::incoming(&store,&id).await?;
    let closing_protocol=crate::gate_registry::closing_config()?;
    let fixed=crate::gate_registry::visibility_config()?.guidance;
    Ok(Json(json!({"member_access":store.access_repository().member_access(&id).await?,"capability_levels":store.capability_levels(&id).await?,"shared_chat":crate::shared_chat::policy(&store,&id).await?,"manager_incoming_message":manager,"closing_message":closing,"closing_protocol":{"flags":closing_protocol.flags,"notification_max_words":closing_protocol.notification_max_words},"message_visibility":{"mode":settings.mode,"guidance":settings.guidance,"fixed_protocol":fixed},"execution_modes":config["execution_modes"],"default_execution_mode":config["default_execution_mode"],"id":id,"name":row.get::<String,_>("name"),
        "compaction_thresholds":{"trigger_tokens":effective.trigger_tokens,"tail_tokens":effective.tail_tokens},
        "compaction_defaults":{"trigger_tokens":defaults.trigger_tokens,"tail_tokens":defaults.tail_tokens,"summary_tokens":defaults.summary_tokens},
        "tool_result_deduplication":crate::tool_output::dedup_policy(&store,&id).await?,"tool_output_policy":tool_output_policy,"tool_output_defaults":tool_output_defaults,
        "tool_output_overridden":tool_output_overridden,"tool_output_eligible_tools":tool_output_eligible_tools,
        "compaction_overridden":override_settings.is_some(),"compaction_valid":compaction_valid,
        "description":row.get::<String,_>("description"),"revision":row.get::<i64,_>("revision"),
        "can_manage":rights["can_manage"]})))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityUpdate { member_access:Option<crate::access::membership::MemberAccess>,tool_result_deduplication: Option<crate::tool_output::DedupPolicy>, revision: i64, shared_chat: Option<crate::shared_chat::SharedChatPolicy>, name: Option<String>, description: Option<String>, execution_modes: Option<Value>, default_execution_mode: Option<String>, message_visibility: Option<crate::gate_controls::AgentVisibility>, closing_message: Option<crate::gate_controls::AgentClosing>, compaction_thresholds: Option<crate::context_compaction::AgentThresholds>, manager_incoming_message: Option<crate::manager::config::IncomingMessage>, tool_output_policy: Option<crate::tool_output::Policy>, reset_tool_output_policy: Option<bool> }
async fn identity_put(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>, Json(body): Json<IdentityUpdate>) -> Api {
    if body.tool_output_policy.is_some() && body.reset_tool_output_policy.is_some() {
        return Err(crate::bad("Set a tool output policy or reset it, not both"));
    }
    if let Some(policy)=body.tool_output_policy.as_ref() {
        policy.validate().map_err(|_|crate::bad("Invalid tool output retention policy"))?;
    }
    if body.revision < 0 || (body.name.is_none() && body.description.is_none() && body.execution_modes.is_none() && body.default_execution_mode.is_none() && body.message_visibility.is_none() && body.closing_message.is_none() && body.compaction_thresholds.is_none() && body.manager_incoming_message.is_none() && body.tool_output_policy.is_none() && body.reset_tool_output_policy.is_none() && body.shared_chat.is_none() && body.member_access.is_none() && body.tool_result_deduplication.is_none())
        || body.name.as_ref().is_some_and(|s| s.trim().is_empty() || s.len()>200)
        || body.description.as_ref().is_some_and(|s| s.len()>2000) {
        return Err(ApiError(StatusCode::BAD_REQUEST,"Name must contain 1–200 bytes; description at most 2000 bytes".into()));
    }
    let _guard = app.gate.lock().await;
    authority(&app, &identity, &id, true).await?;
    let store = app.store.for_agent(&id).await?;
    let blob:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(&id).fetch_one(&store.agent_pool).await.map_err(anyhow::Error::from)?;
    let mut metadata=store.unseal(&blob)?;
    if let Some(settings)=body.tool_result_deduplication {
        // Preserve legacy tool overrides before replacing only the independently fenced agent policy.
        let tools=crate::store::Store::tool_result_overrides(&metadata).map_err(anyhow::Error::from)?;
        metadata["tool_result_deduplication_tools"]=tools;
        metadata["tool_result_deduplication"]=serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings)=body.shared_chat {
        if settings.admin_view||settings.admin_join {return Err(crate::bad("Use conversation privacy for new chats; legacy administrative access is disabled"));}
        metadata["shared_chat"]=serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings)=body.member_access {metadata["member_access"]=serde_json::to_value(settings).map_err(anyhow::Error::from)?;}
    let modes=crate::execution_modes::normalize(body.execution_modes.as_ref().unwrap_or(&metadata["execution_modes"]))
        .map_err(|e|crate::bad(&e.to_string()))?;
    let requested=body.default_execution_mode.as_deref().unwrap_or(metadata["default_execution_mode"].as_str().unwrap_or("ask"));
    let default=if modes.as_array().unwrap().iter().any(|m|m["id"]==requested) {requested.to_owned()}
        else if body.default_execution_mode.is_some() {return Err(crate::bad("Unknown default mode"));} else {"ask".to_owned()};
    metadata["execution_modes"]=modes; metadata["default_execution_mode"]=json!(default);
    if let Some(thresholds)=body.compaction_thresholds {
        let defaults=crate::context_compaction::Config::read(&app.compaction_config_path)?;
        thresholds.apply(&defaults).map_err(|_|crate::bad("Invalid compaction thresholds: tail plus summary must be below trigger"))?;
        metadata["compaction_thresholds"]=serde_json::to_value(thresholds).map_err(anyhow::Error::from)?;
    }
    if let Some(policy)=body.tool_output_policy {
        metadata["tool_output_policy"]=serde_json::to_value(policy).map_err(anyhow::Error::from)?;
    } else if body.reset_tool_output_policy==Some(true) {
        if let Some(fields)=metadata.as_object_mut() { fields.remove("tool_output_policy"); }
    }
    if let Some(settings)=body.message_visibility {
        settings.validate().map_err(|e|crate::bad(&e.to_string()))?;
        metadata["message_visibility"]=serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    if let Some(settings)=body.closing_message {
        settings.validate().map_err(|e|crate::bad(&e.to_string()))?;
        metadata["closing_message"]=serde_json::to_value(settings).map_err(anyhow::Error::from)?;
    }
    let manager_prompt=body.manager_incoming_message.as_ref().map(|settings|settings.criteria.clone());
    if let Some(settings)=body.manager_incoming_message {
        settings.validate().map_err(|e|crate::bad(&e.to_string()))?;
        if settings.enabled {
            let mut scoped=app.clone();scoped.store=store.clone();
            // Validate the actual judge appointment before accepting an enabled gate.
            let mut plan=store.model_catalog().await?.resolve_role(&id,crate::model_selection::ModelRole::System)
                .map_err(|_|crate::bad("Assign an authorized System model before enabling incoming-message review"))?;
            scoped.pin_model_plan(&mut plan).await
                .map_err(|_|crate::bad("System model is unavailable"))?;
        }
        let next=json!({"enabled":settings.enabled});
        // Prompt edits also withdraw any review pinned to the earlier Identity revision.
        if manager_prompt.is_some() {
            metadata["manager_incoming_message_revision"]=json!(metadata["manager_incoming_message_revision"]
                .as_u64().unwrap_or(0).checked_add(1).ok_or_else(||crate::bad("Manager settings revision exhausted"))?);
        }
        metadata["manager_incoming_message"]=next;
    }
    let mut transaction=store.agent_pool.begin().await.map_err(anyhow::Error::from)?;
    let changed = sqlx::query("UPDATE agents SET name=COALESCE(?,name),description=COALESCE(?,description),metadata=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?")
        .bind(body.name.as_deref().map(str::trim)).bind(body.description.as_deref()).bind(store.seal_agent(&metadata)?)
        .bind(crate::now() as i64).bind(&id).bind(body.revision)
        .execute(&mut *transaction).await.map_err(anyhow::Error::from)?;
    if changed.rows_affected()!=1 {return Err(ApiError(StatusCode::CONFLICT,"Identity changed; reload before saving".into()));}
    if let Some(prompt)=manager_prompt {
        crate::manager::config::save_prompt(&store.root,&id,&prompt)?;
    }
    transaction.commit().await.map_err(anyhow::Error::from)?;
    identity_get(State(app.clone()),Extension(identity),Path(id)).await
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct NewAgent { client_request_id: String, name: String, description: Option<String> }
// AGENT-CREATION: the caller owns the new directory record; retries preserve that identity.
async fn create_agent(State(app): State<App>, Extension(identity): Extension<Value>, Json(body): Json<NewAgent>) -> Api {
    if principal(&app,&identity).await?.role == crate::access::Role::Guest { return Err(denied()); }
    if body.client_request_id.is_empty() || body.client_request_id.len()>128 || body.name.trim().is_empty()
        || body.name.len()>200 || body.description.as_ref().is_some_and(|s| s.len()>2000) {
        return Err(crate::bad("Invalid agent fields"));
    }
    let owner=identity["_user_id"].as_str().unwrap_or("");
    let receipt=format!("receipt:model-agent:{owner}:{}",body.client_request_id);
    let request=json!({"name":body.name,"description":body.description});
    let _guard=app.gate.lock().await;
    if let Some(saved)=app.store.get(&receipt).await? {
        if saved["digest"]!=crate::digest(&request) { return Err(crate::conflict("Request identifier reused with different input")); }
        return Ok(Json(saved["result"].clone()));
    }
    // The agent row and receipt live in separate encrypted stores. A stable
    // request-derived ID and embedded digest make crash-window retries safe.
    let id=format!("agent-{}", &crate::digest(&json!({"owner":owner,"request_id":body.client_request_id}))[..32]);
    let metadata=app.store.seal(&json!({"owner_id":owner,"ability_modes":{"codebase_admin":"off"},
        "skill_modes":{},"tool_modes":{},"ability_revision":0,"member_access":{"visitor_access":false,"app_login_members":true,"conversation_privacy":"operator_reviewable"},"creation_digest":crate::digest(&request)}))?;
    let agent_pool=crate::agent_storage::open_agent_pool(&app.store.root,&id).await?;
    let existing: Option<Vec<u8>> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(&id).fetch_optional(&agent_pool).await.map_err(anyhow::Error::from)?;
    if let Some(blob)=existing {
        let registered:Option<String>=sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?").bind(&id).fetch_optional(&app.store.app_pool).await.map_err(anyhow::Error::from)?;
        let old=if registered.is_some(){app.store.for_agent(&id).await?.unseal_agent(&blob)?}else{app.store.unseal(&blob)?};
        if old["creation_digest"]!=crate::digest(&request) || old["owner_id"]!=owner {
            agent_pool.close().await;
            return Err(crate::conflict("Request identifier reused with different input"));
        }
    } else {
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES(?,?,?,'openai_subscription','','active',?,1,?,?)")
            .bind(&id).bind(body.name.trim()).bind(body.description.as_deref().unwrap_or(""))
            .bind(metadata).bind(crate::now() as i64).bind(crate::now() as i64)
            .execute(&agent_pool).await.map_err(anyhow::Error::from)?;
    }
    agent_pool.close().await;
    app.store.register_agent(&id,owner).await?;
    match app.store.agent_membership(owner,&id).await? {
        None => { app.store.set_agent_membership(owner,&id,Relationship::Owner,0).await?; }
        Some(value) if value["relationship"]=="owner" => {}
        Some(_) => return Err(crate::conflict("Agent ownership changed")),
    }
    let seeded=app.store.seed_new_agent_default(owner,&id).await?;
    let result=json!({"id":id,"name":body.name.trim(),"assignment_required":!seeded});
    app.store.put(&receipt,&json!({"digest":crate::digest(&request),"result":result})).await?;
    Ok(Json(result))
}

// CONNECTION-VISIBILITY: status follows tenant grants and members' effective model availability.
async fn connection_status(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>,
    Query(params): Query<HashMap<String,String>>) -> Api { webagent_models::api::selection::connection_status(&app.models_runtime(),identity,id,params).await.map_err(Into::into) }
// Reattach the owner's pending device code after a page reload. Draft
// connections have no roster entry yet, so connection status alone cannot
// recover their verification details.
async fn pending_subscription_authorization(State(app): State<App>, Extension(identity): Extension<Value>,
    Query(params): Query<HashMap<String,String>>) -> Api { webagent_models::api::selection::pending_subscription_authorization(&app.models_runtime(),identity,params).await.map_err(Into::into) }

// Subscription discovery is scoped to the selected owned connection.
async fn subscription_models(State(app): State<App>, Extension(identity): Extension<Value>,
    Path(id): Path<String>, Query(params): Query<HashMap<String,String>>) -> Api { webagent_models::api::selection::subscription_models(&app.models_runtime(),identity,id,params).await.map_err(Into::into) }


// CREDENTIAL-AUTHORIZATION: only the connection owner starts a private device flow.
async fn connection_authorize(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>) -> Api { webagent_models::api::selection::connection_authorize(&app.models_runtime(),identity,id).await.map_err(Into::into) }

// The connection owner may cancel a pending device flow without revoking a
// previously connected account. The polling task observes the cleared login.
async fn connection_authorization_reset(State(app): State<App>, Extension(identity): Extension<Value>,
    Path(id): Path<String>) -> Api { webagent_models::api::selection::connection_authorization_reset(&app.models_runtime(),identity,id).await.map_err(Into::into) }

// The agent's owner may clear every owned subscription credential represented
// by its roster. Reset also cancels an owned draft flow for this agent, which
// may no longer have a visible row after a page reload.
async fn subscription_credentials_reset(State(app): State<App>, Extension(identity): Extension<Value>,
    Query(params): Query<HashMap<String,String>>) -> Api { webagent_models::api::selection::subscription_credentials_reset(&app.models_runtime(),identity,params).await.map_err(Into::into) }


// CREDENTIAL-ROTATION: only the owner can replace a secret and invalidate pinned references.
async fn connection_key(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>, Json(body): Json<ApiKey>) -> Api { webagent_models::api::selection::connection_key(&app.models_runtime(),identity,id,body).await.map_err(Into::into) }


// Default-agent compatibility is a one-time, explicit catalog migration. No run
// silently falls back to the old model setting after this boundary.
pub(crate) async fn migrate_legacy_default(store: &crate::store::Store) -> anyhow::Result<()> { store.models_repository().migrate_legacy_default(||store.agent_model(),crate::DEFAULT_AGENT_MODEL).await }

// TENANT-ROSTER: membership identities are visible only to this agent's administrators.
async fn members(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>) -> Api {
    let rights=authority(&app,&identity,&id,true).await?;
    Ok(Json(json!({"members":app.store.agent_memberships(&id).await?,"authority":rights})))
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberUpdate {
    user_id:String, relationship:Relationship,
    #[serde(default)] agent_administrator:bool,
    revision:u64,
}
// TENANT-ASSIGNMENT: storage rechecks actor authority and immutable ownership atomically.
async fn member_set(State(app): State<App>, Extension(identity): Extension<Value>, Path(id): Path<String>,
    Json(body):Json<MemberUpdate>) -> Api {
    let _guard=app.gate.lock().await;
    authority(&app,&identity,&id,true).await?;
    let result=app.store.assign_agent_membership(identity["_user_id"].as_str().unwrap_or(""),
        &body.user_id,&id,body.relationship,body.agent_administrator,body.revision).await
        .map_err(|_|crate::conflict("Membership update rejected: check authority, account and revision"))?;
    // MEMBERSHIP-REVOCATION: stop active work now, preserving uncertain effects for review.
    crate::phase3_api::quiesce_user(&app,&body.user_id,Some(&id)).await?;
    Ok(Json(json!({"membership":result})))
}


async fn discover(State(app):State<App>,Extension(identity):Extension<Value>,Json(input):Json<Discovery>)->Api { webagent_models::api::selection::discover(&app.models_runtime(),identity,input).await.map_err(Into::into) }

/// Nest only under the authenticated browser API (including its CSRF middleware).
pub fn router() -> Router<App> {
    Router::new().route("/catalog", get(read).put(save))
        .route("/discovery", axum::routing::post(discover))
        .route("/agents", get(agents).post(create_agent))
        .route("/agents/{id}/identity", get(identity_get).put(identity_put))
        .route("/agents/{id}/members", get(members).put(member_set))
        .route("/connections/{id}", get(connection_status))
        .route("/connections/authorization/pending", get(pending_subscription_authorization))
        .route("/connections/{id}/models", get(subscription_models))
        .route("/connections/{id}/api-key", axum::routing::put(connection_key))
        .route("/connections/{id}/authorization", axum::routing::post(connection_authorize))
        .route("/connections/{id}/authorization/reset", axum::routing::post(connection_authorization_reset))
        .route("/connections/subscription/reset", axum::routing::post(subscription_credentials_reset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_tests::{fixture, selection_fixture};
    #[tokio::test]
    async fn authorization_reset_is_owner_scoped_and_cancels_only_its_flow() {
        let (_root, app) = fixture().await;
        selection_fixture(&app).await;
        let admin = json!({"_user_id":"admin"});
        let outsider = json!({"_user_id":"other"});
        *app.login.write().await = Some(json!({"connection_id":"one","status":"pending","device_auth_id":"fixture"}));
        assert!(connection_authorization_reset(State(app.clone()),Extension(outsider),
            Path("one".into())).await.is_err());
        assert_eq!(app.login.read().await.as_ref().unwrap()["status"],"pending");
        connection_authorization_reset(State(app.clone()),Extension(admin.clone()),
            Path("two".into())).await.unwrap();
        assert_eq!(app.login.read().await.as_ref().unwrap()["status"],"pending");
        connection_authorization_reset(State(app.clone()),Extension(admin.clone()),
            Path("one".into())).await.unwrap();
        assert!(app.login.read().await.is_none());
        connection_authorization_reset(State(app.clone()),Extension(admin),
            Path("one".into())).await.unwrap();
        assert!(app.login.read().await.is_none());
    }
    #[tokio::test]
    async fn pending_authorization_reuses_its_code_without_rotating_credentials() {
        let (_root,app)=fixture().await;
        selection_fixture(&app).await;
        let identity=json!({"_user_id":"admin"});
        let before=app.store.model_catalog().await.unwrap().connections["one"].credential_ref.clone();
        *app.login.write().await=Some(json!({"connection_id":"one","status":"pending",
            "device_auth_id":"fixture","verification_url":"https://example.invalid/verify","user_code":"FIXTURE"}));
        let Json(reused)=connection_authorize(State(app.clone()),Extension(identity.clone()),Path("one".into())).await.unwrap();
        assert_eq!(reused["user_code"],"FIXTURE");
        let Json(pending)=pending_subscription_authorization(State(app.clone()),Extension(identity.clone()),
            Query(HashMap::new())).await.unwrap();
        assert_eq!(pending["pending"],true);
        assert_eq!(pending["connection_id"],"one");
        assert_eq!(pending["user_code"],"FIXTURE");
        assert_eq!(app.store.model_catalog().await.unwrap().connections["one"].credential_ref,before);
        app.login.write().await.as_mut().unwrap()["connection_id"]=json!("two");
        assert_eq!(connection_authorize(State(app.clone()),Extension(identity),Path("one".into()))
            .await.unwrap_err().0,StatusCode::CONFLICT);
        assert_eq!(app.store.model_catalog().await.unwrap().connections["one"].credential_ref,before);
    }
    #[tokio::test]
    async fn subscription_reset_detaches_agent_credentials_and_cancels_own_pending_flow() {
        let (_root,app)=fixture().await;
        selection_fixture(&app).await;
        let admin=json!({"_user_id":"admin"});
        let other=json!({"_user_id":"other"});
        let before=app.store.model_catalog().await.unwrap();
        *app.login.write().await=Some(json!({"connection_id":"two","status":"pending","device_auth_id":"fixture"}));
        assert!(subscription_credentials_reset(State(app.clone()),Extension(other),
            Query(HashMap::new())).await.is_err());
        assert_eq!(app.store.model_catalog().await.unwrap().connections["one"].credential_ref,
            before.connections["one"].credential_ref);
        let Json(result)=subscription_credentials_reset(State(app.clone()),Extension(admin.clone()),
            Query(HashMap::new())).await.unwrap();
        assert_eq!(result["connection_ids"],json!(["one","two"]));
        let after=app.store.model_catalog().await.unwrap();
        for id in ["one","two"] {
            assert_ne!(after.connections[id].credential_ref,before.connections[id].credential_ref);
            let Json(status)=connection_status(State(app.clone()),Extension(admin.clone()),
                Path(id.into()),Query(HashMap::new())).await.unwrap();
            assert_eq!(status["connected"],false);
        }
        assert!(app.login.read().await.is_none());
        assert_eq!(after.agent_models["shared_default"].entry_ids,before.agent_models["shared_default"].entry_ids);
    }
    #[tokio::test]
    async fn subscription_status_shows_saved_email_only_to_credential_owner() {
        let (_root, app) = fixture().await;
        let catalog = selection_fixture(&app).await;
        let reference = &catalog.connections["one"].credential_ref;
        app.store.put(reference, &json!({"access_token":"fixture-token","email":"owner@example.test"})).await.unwrap();
        let owner = json!({"_user_id":"admin"});
        let Json(status) = connection_status(State(app.clone()),Extension(owner),
            Path("one".into()),Query(HashMap::new())).await.unwrap();
        assert_eq!(status["connected"],true);
        assert_eq!(status["account_email"],"owner@example.test");
        assert!(!status.to_string().contains("fixture-token"));
        let outsider = json!({"_user_id":"other"});
        assert!(connection_status(State(app),Extension(outsider),
            Path("one".into()),Query(HashMap::new())).await.is_err());
    }
    #[tokio::test]
    async fn visibility_preferences_preserve_fixed_protocol_and_revision_fence() {
        let (_root,app)=fixture().await;let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(before["message_visibility"]["mode"],"manual");
        let patch=|revision,guidance:&str|IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision,name:None,description:None,execution_modes:None,default_execution_mode:None,message_visibility:Some(crate::gate_controls::AgentVisibility{mode:crate::gate_controls::PromotionMode::None,guidance:guidance.into()}),closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None};
        let revision=before["revision"].as_i64().unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(patch(revision,"Keep progress concise"))).await.unwrap();
        assert_eq!(saved["message_visibility"]["fixed_protocol"],before["message_visibility"]["fixed_protocol"]);
        assert_eq!(saved["message_visibility"]["mode"],"none");
        assert!(saved["message_visibility"].get("format").is_none());
        let settings=crate::gate_controls::agent_visibility(&app.store,"shared_default").await.unwrap();assert_eq!(settings.mode,crate::gate_controls::PromotionMode::None);assert_eq!(settings.guidance,"Keep progress concise");
        let Json(reloaded)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();assert_eq!(reloaded,saved);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(patch(revision,"stale"))).await.unwrap_err().0,StatusCode::CONFLICT);
        let outsider=tenant_user(&app,"visibility-outsider").await;
        assert!(identity_put(State(app.clone()),Extension(json!({"_user_id":outsider})),Path("shared_default".into()),Json(patch(revision+1,"forbidden"))).await.is_err());
        assert!(identity_put(State(app.clone()),Extension(identity),Path("shared_default".into()),Json(patch(revision+1,&"x".repeat(4097)))).await.is_err());
    }
    /// Agent toggle and tool inclusion remain independent, including legacy shared-key metadata.
    #[tokio::test]
    async fn dedup_policy_and_tool_overrides_are_independent_and_migrate_atomically() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let id="shared_default".to_owned();let identity=json!({"_user_id":app.store.current_user_id()});
        let blob:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(&id)
            .fetch_one(&app.store.agent_pool).await.unwrap();
        let mut metadata=app.store.unseal(&blob).unwrap();
        metadata["tool_result_deduplication"]=json!({"enabled":false,"file_read":false});
        metadata.as_object_mut().unwrap().remove("tool_result_deduplication_tools");
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?").bind(app.store.seal_agent(&metadata).unwrap())
            .bind(&id).execute(&app.store.agent_pool).await.unwrap();
        let legacy=identity_get(State(app.clone()),Extension(identity.clone()),Path(id.clone())).await.unwrap().0;
        assert_eq!(legacy["tool_result_deduplication"],json!({"enabled":false}));
        let before=app.store.agent_abilities().await.unwrap();
        assert_eq!(before["tool_result_deduplication"],json!({"file_read":false}));
        let update=|revision:i64,enabled:bool|serde_json::from_value::<IdentityUpdate>(json!({
            "revision":revision,"tool_result_deduplication":{"enabled":enabled}})).unwrap();
        let saved=identity_put(State(app.clone()),Extension(identity.clone()),Path(id.clone()),
            Json(update(legacy["revision"].as_i64().unwrap(),true))).await.unwrap().0;
        assert_eq!(saved["tool_result_deduplication"],json!({"enabled":true}));
        let controls=app.store.agent_abilities().await.unwrap();
        assert_eq!(controls["tool_result_deduplication"]["file_read"],false);
        // Exercise the actual authenticated ability route without exposing its private module.
        use tower::ServiceExt;
        let token=crate::uuid();
        app.store.put(&format!("auth:{}",crate::digest(&json!(token))),
            &json!({"csrf":"fixture","expires":crate::now()+3600,"generation":"fixture-generation"})).await.unwrap();
        let request=axum::http::Request::builder().method("PUT")
            .uri(format!("/api/v3/agents/{id}/abilities/codebase_admin/tools/file_read/result-deduplication"))
            .header("content-type","application/json").header("cookie",format!("v3_session={token}"))
            .header("x-csrf-token","fixture")
            .body(axum::body::Body::from(json!({"expected_revision":controls["revision"],"included":true}).to_string())).unwrap();
        assert_eq!(crate::api::router(app.clone()).oneshot(request).await.unwrap().status(),StatusCode::OK);
        let after_tool=identity_get(State(app.clone()),Extension(identity.clone()),Path(id.clone())).await.unwrap().0;
        assert_eq!(after_tool["tool_result_deduplication"],json!({"enabled":true}));
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path(id.clone()),
            Json(update(saved["revision"].as_i64().unwrap(),false))).await.unwrap_err().0,StatusCode::CONFLICT);
        identity_put(State(app.clone()),Extension(identity.clone()),Path(id.clone()),
            Json(update(after_tool["revision"].as_i64().unwrap(),false))).await.unwrap();
        let final_policy=crate::tool_output::dedup_policy(&app.store,&id).await.unwrap();assert!(!final_policy.enabled);
        let final_tools=app.store.agent_abilities().await.unwrap();
        assert_eq!(final_tools["tool_result_deduplication"]["file_read"],true);
        for key in ["visibility","tool_modes","tool_execution"] {assert_eq!(final_tools[key],before[key]);}
        let blob:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(&id)
            .fetch_one(&app.store.agent_pool).await.unwrap();let migrated=app.store.unseal(&blob).unwrap();
        assert_eq!(migrated["tool_result_deduplication"],json!({"enabled":false}));
        assert_eq!(migrated["tool_result_deduplication_tools"],json!({"file_read":true}));
        assert!(serde_json::from_value::<IdentityUpdate>(json!({"revision":1,
            "tool_result_deduplication":{"enabled":true,"file_read":false}})).is_err());
    }

    #[tokio::test]
    async fn identity_compaction_thresholds_are_agent_scoped_and_validated() {
        let (_root,app)=fixture().await;
        let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(before["compaction_overridden"],false);
        let defaults=&before["compaction_defaults"];
        let trigger=defaults["trigger_tokens"].as_u64().unwrap() as usize + 1000;
        let tail=defaults["tail_tokens"].as_u64().unwrap() as usize;
        let update=|revision,trigger,tail|IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,
            revision,name:None,description:None,execution_modes:None,default_execution_mode:None,
            message_visibility:None,closing_message:None,manager_incoming_message:None,
            compaction_thresholds:Some(crate::context_compaction::AgentThresholds{trigger_tokens:trigger,tail_tokens:tail}), tool_output_policy:None,reset_tool_output_policy:None};
        let revision=before["revision"].as_i64().unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(revision,trigger,tail))).await.unwrap();
        assert_eq!(saved["compaction_overridden"],true);
        assert_eq!(saved["compaction_thresholds"]["trigger_tokens"].as_u64(),Some(trigger as u64));
        let store=app.store.for_agent("shared_default").await.unwrap();
        let override_settings=crate::context_compaction::agent_thresholds(&store,"shared_default").await.unwrap().unwrap();
        assert_eq!(override_settings.trigger_tokens,trigger);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(revision,trigger,tail))).await.unwrap_err().0,StatusCode::CONFLICT);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),trigger,trigger))).await.unwrap_err().0,StatusCode::BAD_REQUEST);
        let outsider=tenant_user(&app,"compaction-outsider").await;
        assert_eq!(identity_put(State(app.clone()),Extension(json!({"_user_id":outsider})),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),trigger+1000,tail))).await.unwrap_err().0,StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn duplicate_result_identity_toggle_is_revision_fenced_and_manager_only() {
        let (_root,app)=fixture().await;let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(before["tool_result_deduplication"]["enabled"],true);
        let update=|revision|IdentityUpdate { member_access:None,tool_result_deduplication:Some(crate::tool_output::DedupPolicy{enabled:false}),shared_chat:None,
            revision,name:None,description:None,execution_modes:None,default_execution_mode:None,message_visibility:None,
            closing_message:None,compaction_thresholds:None,manager_incoming_message:None,tool_output_policy:None,reset_tool_output_policy:None};
        let revision=before["revision"].as_i64().unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(update(revision))).await.unwrap();
        assert_eq!(saved["tool_result_deduplication"]["enabled"],false);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(update(revision))).await.unwrap_err().0,StatusCode::CONFLICT);
        let outsider=tenant_user(&app,"duplicate-result-outsider").await;
        assert_eq!(identity_put(State(app.clone()),Extension(json!({"_user_id":outsider})),Path("shared_default".into()),Json(update(saved["revision"].as_i64().unwrap()))).await.unwrap_err().0,StatusCode::FORBIDDEN);
    }
    #[tokio::test]
    async fn tool_output_policy_roundtrips_validates_and_resets_with_identity_revision() {
        let (_root,app)=fixture().await;
        let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        let defaults=crate::tool_output::Policy::read(&app.store.root).unwrap();
        assert_eq!(before["tool_output_overridden"],false);
        assert_eq!(before["tool_output_defaults"],json!(defaults));
        assert_eq!(before["tool_output_policy"],json!(defaults));
        let edited=crate::tool_output::Policy {
            enabled:false,
            scope:crate::tool_output::Scope::Selected,
            tools:vec!["file_read".into()],
            lifetime_calls:9,
            ordinary_chars:32_000,
            promoted_chars:18_000,
            refresh_on_recall:false,
            promote_on_edit:false,
        };
        let update=|revision,tool_output_policy,reset_tool_output_policy|IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,
            revision,name:None,description:None,execution_modes:None,default_execution_mode:None,
            message_visibility:None,closing_message:None,compaction_thresholds:None,
            manager_incoming_message:None,tool_output_policy,reset_tool_output_policy,
        };
        let revision=before["revision"].as_i64().unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(revision,Some(edited.clone()),None))).await.unwrap();
        assert_eq!(saved["tool_output_overridden"],true);
        assert_eq!(saved["tool_output_policy"],json!(edited));
        let Json(reloaded)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(reloaded,saved);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(revision,Some(edited.clone()),None))).await.unwrap_err().0,StatusCode::CONFLICT);

        let mut invalid=edited.clone();
        invalid.lifetime_calls=0;
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),Some(invalid),None))).await.unwrap_err().0,StatusCode::BAD_REQUEST);
        let mut invalid=edited.clone();
        invalid.tools=vec!["file_write".into()];
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),Some(invalid),None))).await.unwrap_err().0,StatusCode::BAD_REQUEST);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),Some(edited.clone()),Some(true)))).await.unwrap_err().0,StatusCode::BAD_REQUEST);

        let outsider=tenant_user(&app,"tool-output-outsider").await;
        assert_eq!(identity_put(State(app.clone()),Extension(json!({"_user_id":outsider})),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),Some(edited.clone()),None))).await.unwrap_err().0,StatusCode::FORBIDDEN);

        let Json(reset)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),
            Json(update(saved["revision"].as_i64().unwrap(),None,Some(true)))).await.unwrap();
        assert_eq!(reset["tool_output_overridden"],false);
        assert_eq!(reset["tool_output_policy"],json!(defaults));
        let Json(reloaded)=identity_get(State(app.clone()),Extension(identity),Path("shared_default".into())).await.unwrap();
        assert_eq!(reloaded,reset);
        let store=app.store.for_agent("shared_default").await.unwrap();
        assert_eq!(crate::tool_output::effective(&store,"shared_default").await.unwrap(),defaults);
    }

    #[tokio::test]
    async fn notification_guidance_roundtrips_with_identity_revision() {
        let (_root,app)=fixture().await;
        let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(before["closing_message"]["notification_guidance"],"");
        let revision=before["revision"].as_i64().unwrap();
        let update=|revision,guidance:&str|IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision,name:None,description:None,
            execution_modes:None,default_execution_mode:None,message_visibility:None,
            closing_message:Some(crate::gate_controls::AgentClosing{
                notification_guidance:guidance.into()}),manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None};
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),
            Path("shared_default".into()),Json(update(revision,"Name the next step"))).await.unwrap();
        assert_eq!(saved["closing_message"]["notification_guidance"],"Name the next step");
        let Json(reloaded)=identity_get(State(app.clone()),Extension(identity.clone()),
            Path("shared_default".into())).await.unwrap();
        assert_eq!(reloaded,saved);
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),
            Path("shared_default".into()),Json(update(revision,"stale"))).await.unwrap_err().0,StatusCode::CONFLICT);
        assert!(identity_put(State(app.clone()),Extension(identity),
            Path("shared_default".into()),Json(update(saved["revision"].as_i64().unwrap(),&"x".repeat(4097)))).await.is_err());
    }
    #[tokio::test]
    async fn identity_modes_persist_default_and_enforce_live_boundary() {
        let (_root,app)=fixture().await;
        let identity=json!({"_user_id":"admin"});
        let Json(before)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        let Json(saved)=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,
            revision:before["revision"].as_i64().unwrap(),name:None,description:None,
            execution_modes:Some(json!([{"id":"review","label":"Review","permission_policy":"read_only","prompt":"Inspect only"}])),
            default_execution_mode:Some("review".into()),message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.unwrap();
        let Json(reloaded)=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        assert_eq!(saved,reloaded);
        let Json(session)=crate::api::create_session(State(app.clone()),Json(json!({"client_request_id":crate::uuid()}))).await.unwrap();
        let sid=session["id"].as_str().unwrap();
        let mode=crate::session_mode(&app,sid).await.unwrap();
        assert_eq!(mode["execution_mode"],"review"); assert_eq!(mode["mode"],"read_only");
        assert_eq!(mode["definition"]["prompt"],"Inspect only");
        let mut modes=saved["execution_modes"].clone();
        modes.as_array_mut().unwrap().last_mut().unwrap()["permission_policy"]=json!("write");
        identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,
            revision:saved["revision"].as_i64().unwrap(),name:None,description:None,execution_modes:Some(modes),default_execution_mode:None,message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.unwrap();
        assert_eq!(crate::session_mode(&app,sid).await.unwrap()["mode"],"read_only");
        let Json(next)=crate::api::change_session_mode(&app,sid,"review",Some(0),None).await.unwrap();
        assert_eq!(next["mode"],"write");
        assert!(crate::api::change_session_mode(&app,sid,"unknown",None,None).await.is_err());
    }
    #[tokio::test]
    async fn byo_disabled_blocks_credential_writes_without_mutating_catalog() {
        let (_root,app)=fixture().await;
        let catalog=selection_fixture(&app).await;
        let mut tier=app.store.entitlement_tier("basic").await.unwrap().unwrap();
        let revision=tier.revision;
        tier.model_policy=Some(crate::access::ModelPolicy{roster_id:None,
            allowed_entry_ids:Default::default(),allow_byo:false,max_byo_entries:0});
        app.store.save_entitlement_tier(tier,revision).await.unwrap();
        let user=tenant_user(&app,"byo-disabled").await;
        let account=app.store.account_by_id(&user).await.unwrap().unwrap();
        app.store.update_account_access(&user,account["access_revision"].as_u64().unwrap(),
            crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
        // Ownership is immutable; use a fresh ordinary-account connection instead.
        let mut connection=catalog.connections["one"].clone();
        connection.id="ordinary-byo".into();connection.owner_id=user.clone();connection.revision=1;
        let mut next=catalog.clone();next.connections.insert(connection.id.clone(),connection);
        let catalog=app.store.save_model_catalog(&next,next.revision).await.unwrap();
        let identity=json!({"_user_id":user});
        let caller=principal(&app,&identity).await.unwrap();
        assert!(byo_credential_permission(&app,&caller).await.is_err());
        assert!(connection_authorize(State(app.clone()),Extension(identity.clone()),
            Path("ordinary-byo".into())).await.is_err());
        assert!(connection_key(State(app.clone()),Extension(identity),
            Path("ordinary-byo".into()),Json(ApiKey{api_key:"fixture-no-network-key".into()})).await.is_err());
        assert_eq!(app.store.model_catalog().await.unwrap(),catalog);
    }
    #[tokio::test]
    async fn identity_is_scoped_validated_and_revision_fenced() {
        let (_root, app) = fixture().await;
        let identity = json!({"_user_id":"admin"});
        let Json(before) = identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap();
        let revision = before["revision"].as_i64().unwrap();
        let Json(saved) = identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision,name:Some("  Renamed  ".into()),description:Some("Description".into()),execution_modes:None,default_execution_mode:None,message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.unwrap();
        assert_eq!(saved["name"],"Renamed"); assert_eq!(saved["description"],"Description");
        assert!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision,name:Some("Stale".into()),description:None,execution_modes:None,default_execution_mode:None,message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.is_err());
        assert!(identity_put(State(app.clone()),Extension(identity),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision:revision+1,name:Some(" ".into()),description:None,execution_modes:None,default_execution_mode:None,message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.is_err());
        let outsider = tenant_user(&app,"identity-outsider").await;
        assert!(identity_put(State(app.clone()),Extension(json!({"_user_id":outsider})),Path("shared_default".into()),Json(IdentityUpdate { member_access:None, tool_result_deduplication:None, shared_chat:None,revision:revision+1,name:Some("Forbidden".into()),description:None,execution_modes:None,default_execution_mode:None,message_visibility:None,closing_message:None,manager_incoming_message:None,compaction_thresholds:None, tool_output_policy:None,reset_tool_output_policy:None})).await.is_err());
    }
    #[tokio::test]
    async fn deepseek_keys_are_validated_encrypted_scoped_and_not_returned() {
        let (_root, app) = fixture().await;
        let mut catalog = selection_fixture(&app).await;
        catalog.connections.get_mut("one").unwrap().transport = crate::model_selection::Transport::Deepseek;
        app.store.save_model_catalog(&catalog,catalog.revision).await.unwrap();
        let identity=json!({"_user_id":"admin"});
        let result=connection_key(State(app.clone()),Extension(identity.clone()),Path("one".into()),Json(ApiKey{api_key:"bad key".into()})).await;
        assert!(result.is_err());
        assert!(connection_key(State(app.clone()),Extension(identity.clone()),Path("two".into()),Json(ApiKey{api_key:"test-key-no-network".into()})).await.is_err());
        assert!(connection_authorize(State(app.clone()),Extension(identity.clone()),Path("one".into())).await.is_err());
        let Json(result)=connection_key(State(app.clone()),Extension(identity.clone()),Path("one".into()),Json(ApiKey{api_key:"test-key-no-network".into()})).await.unwrap();
        assert_eq!(result,json!({"saved":true}));
        let catalog=app.store.model_catalog().await.unwrap();
        let reference=&catalog.connections["one"].credential_ref;
        assert_eq!(app.store.get(reference).await.unwrap().unwrap()["api_key"],"test-key-no-network");
        let Json(status)=connection_status(State(app.clone()),Extension(identity.clone()),Path("one".into()),Query(HashMap::new())).await.unwrap();
        assert_eq!(status["connected"],true);assert!(!status.to_string().contains("test-key"));
        assert!(!serde_json::to_string(&catalog).unwrap().contains("test-key"));
        let mut catalog=catalog;catalog.connections.get_mut("one").unwrap().revoked=true;
        app.store.save_model_catalog(&catalog,catalog.revision).await.unwrap();
        assert!(connection_key(State(app.clone()),Extension(identity),Path("one".into()),Json(ApiKey{api_key:"test-key-no-network".into()})).await.is_err());
    }

    // TENANT-API-FIXTURE: use real encrypted stores; no network or live account data.
    #[tokio::test]
    async fn discovery_rejects_forged_credentials_and_endpoint_overrides_without_network() {
        let (_root,app)=fixture().await;
        assert!(serde_json::from_value::<Discovery>(json!({"provider":"openrouter","api_key":"secret-key","base_url":"http://127.0.0.1"})).is_err());
        let identity=json!({"_user_id":"admin"});
        assert!(discover(State(app.clone()),Extension(identity.clone()),Json(Discovery{
            provider:None,api_key:None,connection_id:Some("not-owned".into())})).await.is_err());
        assert!(discover(State(app),Extension(identity),Json(Discovery{
            provider:Some(crate::provider::ProviderId::Openrouter),api_key:Some("secret-key".into()),connection_id:Some("ambiguous".into())})).await.is_err());
    }
    #[tokio::test]
    async fn typesafe_discovery_accepts_decision_model_without_chat_endpoint() {
        let (_root,app)=fixture().await;
        let identity=json!({"_user_id":"admin"});
        let Json(result)=discover(State(app),Extension(identity),Json(Discovery{
            provider:Some(crate::provider::ProviderId::Typesafe),
            api_key:Some("test-key-no-network".into()),connection_id:None,
        })).await.unwrap();
        let model=&result["models"][0];
        assert_eq!(model["id"],"jev-latest");
        assert_eq!(model["decision_capable"],true);
        assert_eq!(model["text_capable"],false);
    }
    #[tokio::test]
    async fn new_agent_uses_account_default_without_reseeding_existing_rosters() {
        let (_root,app)=fixture().await;
        let catalog=app.store.model_catalog().await.unwrap();
        let entry=catalog.entries.values().find(|e|e.owner_id=="admin").unwrap();
        let id=entry.id.clone();
        let mut preferences=app.store.account_model_preferences("admin").await.unwrap();
        preferences.default_entry_id=Some(crate::account_catalog::byo_id(&id));
        app.store.save_account_model_preferences("admin",&preferences).await.unwrap();
        let identity=json!({"_user_id":"admin"});
        let input=||NewAgent{client_request_id:"seeded-creation".into(),name:"Seeded".into(),description:None};
        let Json(created)=create_agent(State(app.clone()),Extension(identity.clone()),Json(input())).await.unwrap();
        assert_eq!(created["assignment_required"],false);
        let agent=created["id"].as_str().unwrap();
        let mut catalog=app.store.model_catalog().await.unwrap();
        assert_eq!(catalog.agent_models[agent].roles[&crate::model_selection::ModelRole::Standard].entry_id,id);
        let Json(retry)=create_agent(State(app.clone()),Extension(identity),Json(input())).await.unwrap();
        assert_eq!(retry,created);
        assert_eq!(app.store.model_catalog().await.unwrap(),catalog);
        catalog.agent_models.get_mut(agent).unwrap().roles.clear();
        let cleared=app.store.save_model_catalog(&catalog,catalog.revision).await.unwrap();
        assert!(!app.store.seed_new_agent_default("admin",agent).await.unwrap());
        assert_eq!(app.store.model_catalog().await.unwrap(),cleared);
    }
    async fn tenant_user(app: &App, name: &str) -> String {
        let account=app.store.create_user_account(name,"long-hash-placeholder").await.unwrap();
        let id=account["user_id"].as_str().unwrap();
        app.store.update_account_access(id,1,crate::access::AccountStatus::Active,
            crate::access::Role::User,"operator").await.unwrap();
        id.to_owned()
    }
    fn agent_query(agent: &str) -> Query<HashMap<String,String>> {
        Query(HashMap::from([("agent_id".into(),agent.into())]))
    }
    // TENANT-API-PRIVACY: grants, roster, tier and administrator checks compose independently.
    #[tokio::test]
    async fn tenant_members_cannot_edit_or_read_private_model_connections() {
        let (_root,app)=fixture().await;
        let mut catalog=selection_fixture(&app).await;
        let owner=tenant_user(&app,"tenant-owner").await;
        let member=tenant_user(&app,"tenant-member").await;
        let outsider=tenant_user(&app,"outsider").await;
        let identity=json!({"_user_id":owner});
        let Json(created)=create_agent(State(app.clone()),Extension(identity.clone()),Json(NewAgent {
            client_request_id:"tenant-api".into(),name:"Tenant".into(),description:None})).await.unwrap();
        let agent=created["id"].as_str().unwrap();
        let agent_store=app.store.for_agent(agent).await.unwrap();
        let raw:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(agent).fetch_one(&agent_store.agent_pool).await.unwrap();
        let mut metadata=agent_store.unseal_agent(&raw).unwrap();metadata["member_access"]["app_login_members"]=json!(false);
        sqlx::query("UPDATE agents SET metadata=? WHERE id=?").bind(agent_store.seal_agent(&metadata).unwrap()).bind(agent).execute(&agent_store.agent_pool).await.unwrap();
        // Both connections are granted; only one is placed on the tenant roster.
        for id in ["one","two"] {
            catalog.connections.get_mut(id).unwrap().allowed_agents.insert(agent.into());
            catalog.entries.get_mut(id).unwrap().allowed_agents.insert(agent.into());
        }
        let mut roster=catalog.agent_models["agent-a"].clone();
        roster.agent_id=agent.into();roster.owner_id=owner.clone();roster.revision=1;
        roster.entry_ids=vec!["one".into()];
        roster.roles=std::collections::BTreeMap::from([(crate::model_selection::ModelRole::Standard,
            crate::model_selection::RoleAssignment {entry_id:"one".into(),backup_entry_ids:vec![]})]);
        catalog.agent_models.insert(agent.into(),roster);
        let mut private=catalog.connections["one"].clone();
        private.id="private".into();private.allowed_agents=std::collections::BTreeSet::from(["agent-a".into()]);
        catalog.connections.insert("private".into(),private);
        catalog=app.store.save_model_catalog(&catalog,catalog.revision).await.unwrap();
        app.store.assign_agent_membership(&owner,&member,agent,Relationship::Member,false,0).await.unwrap();
        let member_identity=json!({"_user_id":member});
        let Json(view)=read(State(app.clone()),Extension(member_identity.clone()),agent_query(agent)).await.unwrap();
        assert!(view["catalog"]["connections"].get("private").is_none());
        assert!(view["catalog"]["connections"].get("two").is_none());
        assert_eq!(view["catalog"]["connections"]["one"]["credential_ref"],"");
        assert_eq!(view["available_entries"],json!(["one"]));
        assert!(matches!(save(State(app.clone()),Extension(member_identity.clone()),agent_query(agent),
            Json(scoped(&catalog,&member,agent))).await,Err(ApiError(StatusCode::FORBIDDEN,_))));
        assert!(matches!(read(State(app.clone()),Extension(json!({"_user_id":outsider})),agent_query(agent)).await,
            Err(ApiError(StatusCode::FORBIDDEN,_))));
        assert!(matches!(members(State(app.clone()),Extension(member_identity.clone()),Path(agent.into())).await,
            Err(ApiError(StatusCode::FORBIDDEN,_))));
        assert!(connection_status(State(app.clone()),Extension(member_identity.clone()),Path("one".into()),agent_query(agent)).await.is_ok());
        assert!(connection_status(State(app.clone()),Extension(member_identity.clone()),Path("two".into()),agent_query(agent)).await.is_err());
        assert!(connection_status(State(app.clone()),Extension(identity.clone()),Path("private".into()),agent_query(agent)).await.is_err());
        // The tenant owner may manage shared entries, but cannot replace their owner's credentials.
        assert!(matches!(connection_key(State(app.clone()),Extension(identity.clone()),Path("one".into()),
            Json(ApiKey {api_key:"valid-fixture-secret".into()})).await,Err(ApiError(StatusCode::FORBIDDEN,_))));
        assert!(matches!(connection_authorize(State(app.clone()),Extension(identity),Path("one".into())).await,
            Err(ApiError(StatusCode::FORBIDDEN,_))));
        let revision=app.store.account_by_id(&member).await.unwrap().unwrap()["access_revision"].as_u64().unwrap();
        app.store.update_account_access(&member,revision,crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
        assert!(connection_status(State(app.clone()),Extension(member_identity.clone()),Path("one".into()),agent_query(agent)).await.is_err());
        let Json(view)=read(State(app),Extension(member_identity),agent_query(agent)).await.unwrap();
        assert_eq!(view["available_entries"],json!([]));
    }
    // TENANT-API-MUTATION: scoped editors cannot forge shared references or overwrite newer revisions.
    #[tokio::test]
    async fn scoped_catalog_rejects_reference_forgery_and_stale_revisions() {
        let (_root,app)=fixture().await;
        let catalog=selection_fixture(&app).await;
        let manager=tenant_user(&app,"delegated-manager").await;
        app.store.assign_agent_membership("admin",&manager,"agent-a",Relationship::Member,true,0).await.unwrap();
        let identity=json!({"_user_id":manager});
        let mut forged=scoped(&catalog,&manager,"agent-a");
        forged.connections.get_mut("one").unwrap().credential_ref="credential:two".into();
        assert!(matches!(save(State(app.clone()),Extension(identity.clone()),agent_query("agent-a"),Json(forged)).await,
            Err(ApiError(StatusCode::FORBIDDEN,_))));
        let original=scoped(&catalog,&manager,"agent-a");
        let mut forged=original.clone();
        forged.entries.get_mut("one").unwrap().connection_id="two".into();
        assert!(matches!(save(State(app.clone()),Extension(identity.clone()),agent_query("agent-a"),Json(forged)).await,
            Err(ApiError(StatusCode::FORBIDDEN,_))));
        // A valid scoped round trip preserves grants that this recipient cannot see.
        let _ = save(State(app.clone()),Extension(identity.clone()),agent_query("agent-a"),Json(original.clone())).await.unwrap();
        let persisted=app.store.model_catalog().await.unwrap();
        assert!(persisted.connections["one"].allowed_agents.contains("shared_default"));
        assert!(matches!(save(State(app.clone()),Extension(identity),agent_query("agent-a"),Json(original)).await,
            Err(ApiError(StatusCode::CONFLICT,_))));
        let mut forged=scoped(&persisted,"admin","agent-a");
        forged.connections.get_mut("one").unwrap().credential_ref="credential:two".into();
        assert!(matches!(save(State(app),Extension(json!({"_user_id":"admin"})),agent_query("agent-a"),Json(forged)).await,
            Err(ApiError(StatusCode::BAD_REQUEST,_))));
    }
}


#[cfg(test)]
mod manager_identity_tests {
    use super::*;
    #[tokio::test]
    async fn manager_identity_is_off_by_default_scoped_and_revision_fenced() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let identity=json!({"_user_id":"admin"});
        let before=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap().0;
        assert_eq!(before["manager_incoming_message"],json!({"enabled":false,"criteria":""}));
        let revision=before["revision"].as_i64().unwrap();
        let body=|revision,criteria:&str|serde_json::from_value::<IdentityUpdate>(json!({
            "revision":revision,"manager_incoming_message":{"enabled":true,"criteria":criteria}})).unwrap();
        assert!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(body(revision," "))).await.is_err());
        let saved=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(body(revision,"Clothing questions only"))).await.unwrap().0;
        let reloaded=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap().0;
        assert_eq!(saved,reloaded);
        let prompt_path=app.store.root.join("config/prompts.json");
        let prompt_file:Value=serde_json::from_slice(&std::fs::read(&prompt_path).unwrap()).unwrap();
        assert_eq!(prompt_file["manager_incoming_message"]["shared_default"],"Clothing questions only");
        assert_eq!(identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(body(revision,"Stale"))).await.unwrap_err().0,StatusCode::CONFLICT);
        let user=app.store.create_user_account("manager-setting-outsider","fixture-only-password").await.unwrap();
        assert_eq!(identity_put(State(app.clone()),Extension(json!({"_user_id":user["user_id"]})),Path("shared_default".into()),Json(body(revision+1,"Forbidden"))).await.unwrap_err().0,StatusCode::FORBIDDEN);
        assert!(identity_put(State(app.clone()),Extension(identity),Path("shared_default".into()),Json(body(revision+1,&"x".repeat(8193)))).await.is_err());
    }
}

#[cfg(test)]
mod manager_disabled_prompt_tests {
    use super::*;
    #[tokio::test]
    async fn save_prompt_while_off_without_system_model_and_reload_from_file() {
        let (_root,app)=crate::runtime_tests::fixture().await;
        let identity=json!({"_user_id":"admin"});
        let mut catalog=app.store.model_catalog().await.unwrap();
        catalog.agent_models.get_mut("shared_default").unwrap().roles.remove(&crate::model_selection::ModelRole::System);
        app.store.put("model_catalog",&serde_json::to_value(catalog).unwrap()).await.unwrap();
        let before=identity_get(State(app.clone()),Extension(identity.clone()),Path("shared_default".into())).await.unwrap().0;
        let body=serde_json::from_value::<IdentityUpdate>(json!({
            "revision":before["revision"],"manager_incoming_message":{"enabled":false,"criteria":"Clothing store questions"}})).unwrap();
        let saved=identity_put(State(app.clone()),Extension(identity.clone()),Path("shared_default".into()),Json(body)).await.unwrap().0;
        assert_eq!(saved["manager_incoming_message"],json!({"enabled":false,"criteria":"Clothing store questions"}));
        let path=app.store.root.join("config/prompts.json");
        let mut prompts:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        prompts["manager_incoming_message"]["shared_default"]=json!("Updated directly in prompts.json");
        std::fs::write(&path,serde_json::to_vec(&prompts).unwrap()).unwrap();
        let reload=identity_get(State(app.clone()),Extension(identity),Path("shared_default".into())).await.unwrap().0;
        assert_eq!(reload["manager_incoming_message"]["criteria"],"Updated directly in prompts.json");
        assert_eq!(reload["manager_incoming_message"]["enabled"],false);
        let store=app.store.for_agent("shared_default").await.unwrap();
        let raw:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'").fetch_one(&store.agent_pool).await.unwrap();
        assert!(store.unseal(&raw).unwrap()["manager_incoming_message"].get("criteria").is_none());
    }
}

#[cfg(test)] use webagent_models::api::selection::scoped;
#[cfg(test)]
async fn byo_credential_permission(app:&App,caller:&Principal)->Result<(),ApiError> {
    webagent_models::api::selection::byo_credential_permission(&app.models_runtime(),caller).await.map_err(Into::into)
}
