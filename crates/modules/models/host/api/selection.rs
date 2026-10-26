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


// AGENT-VAULT: named API keys are owner-private records in the managed agent's database.
use webagent_models::api::credentials::{self as vault,Label,Save};
async fn credentials(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>)->Api { vault::list(&app.models_runtime(),identity,id).await.map_err(Into::into) }
async fn credential_save(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<Save>)->Api { vault::save(&app.models_runtime(),identity,id,body).await.map_err(Into::into) }
async fn credential_load(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<Label>)->Api { vault::load(&app.models_runtime(),identity,id,body).await.map_err(Into::into) }
async fn credential_delete(State(app):State<App>,Extension(identity):Extension<Value>,Path(id):Path<String>,Json(body):Json<Label>)->Api { vault::delete(&app.models_runtime(),identity,id,body).await.map_err(Into::into) }

async fn discover(State(app):State<App>,Extension(identity):Extension<Value>,Json(input):Json<Discovery>)->Api { webagent_models::api::selection::discover(&app.models_runtime(),identity,input).await.map_err(Into::into) }

/// Nest only under the authenticated browser API (including its CSRF middleware).
pub fn router() -> Router<App> {
    Router::new().route("/catalog", get(read).put(save))
        .route("/discovery", axum::routing::post(discover))
        .route("/agents/{id}/members", get(members).put(member_set))
        .route("/agents/{id}/credentials", get(credentials).put(credential_save).delete(credential_delete))
        .route("/agents/{id}/credentials/load", axum::routing::post(credential_load))
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
    use crate::agents_api::{create_agent,NewAgent};
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


#[cfg(test)] use webagent_models::api::selection::scoped;
#[cfg(test)]
async fn byo_credential_permission(app:&App,caller:&Principal)->Result<(),ApiError> {
    webagent_models::api::selection::byo_credential_permission(&app.models_runtime(),caller).await.map_err(Into::into)
}
