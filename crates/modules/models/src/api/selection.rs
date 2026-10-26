//! Models endpoint policy and mutations. Root supplies authenticated identity and route extraction.
use crate::{runtime::Runtime,storage::RuntimeStorage,model_selection::Catalog};
use super::{Api,ApiError};
use webagent_access::{Principal,Relationship};
use axum::{Json,http::StatusCode};
use serde::Deserialize;
use serde_json::{json,Value};
use std::collections::{HashMap,BTreeMap};
use crate::platform_catalog::{Entry,Mutation};
use crate::account_catalog::Preferences;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiKey { pub api_key: String }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Discovery { pub provider:Option<crate::provider::ProviderId>, pub api_key:Option<String>, pub connection_id:Option<String> }
pub async fn byo_credential_permission<C:RuntimeStorage>(app:&Runtime<'_,C>, caller:&Principal) -> Result<(),ApiError> {
    let tier=app.store.entitlement_tier(&caller.tier_id).await?.ok_or_else(||denied())?;
    if tier.model_policy.as_ref().is_some_and(|p|!p.allow_byo) {return Err(denied());}
    Ok(())
}
pub async fn response<C:RuntimeStorage>(app: &Runtime<'_,C>, identity: &Value, agent: &str, catalog: &Catalog) -> Api {
    let rights=authority(app,identity,agent,false).await?;
    let caller=principal(app,identity).await?;
    let mut view=scoped(catalog,&caller.user_id,agent);
    let access=app.store.live_access(&caller.user_id,agent).await.ok();
    let mut available=Vec::new();
    for entry in view.entries.values() {
        if !entry.allowed_agents.contains(agent)
            || !view.connections.get(&entry.connection_id).is_some_and(|c|!c.revoked && c.allowed_agents.contains(agent))
            || (rights["can_manage"]!=true && !catalog.agent_models.get(agent).is_some_and(|m|m.entry_ids.contains(&entry.id))) {continue;}
        let allowed=if let Some(pin)=catalog.platform_sources.get(&entry.id) {
            app.store.resolve_account_model_source(&caller.user_id,&crate::account_catalog::Source{
                origin:crate::account_catalog::Origin::Platform,
                entry_id:crate::account_catalog::platform_id(&pin.roster_id,&pin.entry.id)}).await.is_ok()
        } else {access.as_ref().is_some_and(|a|a.model_allowed(&entry.upstream_model))};
        if allowed {available.push(entry.id.clone());}
    }
    if rights["can_manage"] != true {
        view.entries.retain(|id,_|available.contains(id));
        view.connections.retain(|id,_|view.entries.values().any(|e|e.connection_id == *id));
        for c in view.connections.values_mut() { c.credential_ref.clear(); }
        for a in view.agent_models.values_mut() {
            a.entry_ids.retain(|id|available.contains(id));
            a.limits.retain(|id,_|available.contains(id));
            a.efforts.retain(|id,_|available.contains(id));
            a.roles.retain(|_,r|available.contains(&r.entry_id));
            for r in a.roles.values_mut() {r.backup_entry_ids.retain(|id|available.contains(id));}
        }
    }
    let adoptions=app.store.agent_model_adoptions(&caller.user_id,agent).await?;
    Ok(Json(json!({"adoptions":adoptions,"catalog":view,"caller_id":caller.user_id,"can_manage":rights["can_manage"],
        "can_create_agent":caller.role != webagent_access::Role::Guest,"agent_owner_id":rights["owner_id"],
        "available_entries":available,"supported_transports":["typesafe","openai_subscription","deepseek","openai_api","openrouter","abacus","gemini","groq","together","mistral","fireworks","xai","perplexity","deepinfra"]})))
}
pub fn scoped(catalog: &Catalog, caller: &str, agent: &str) -> Catalog {
    let mut view = catalog.clone();
    view.connections.retain(|_,c|c.owner_id == caller || c.allowed_agents.contains(agent));
    view.entries.retain(|_,e|e.owner_id == caller ||
        (e.allowed_agents.contains(agent) && view.connections.contains_key(&e.connection_id)));
    view.connections.retain(|id,c|c.owner_id == caller || view.entries.values().any(|e|e.connection_id == *id));
    for c in view.connections.values_mut().filter(|c|c.owner_id != caller) {
        c.allowed_agents.retain(|id|id == agent);
    }
    for e in view.entries.values_mut().filter(|e|e.owner_id != caller) {
        e.allowed_agents.retain(|id|id == agent);
    }
    view.platform_sources.retain(|id,_|view.entries.contains_key(id));
    view.groups.clear();
    view.assignments.clear();
    view.agent_models.retain(|id,_|id == agent);
    view
}
pub async fn authority<C:RuntimeStorage>(app: &Runtime<'_,C>, identity: &Value, agent: &str, write: bool) -> Result<Value, ApiError> {
    let caller = principal(app,identity).await?;
    let value = app.store.agent_authority(&caller.user_id,agent).await.map_err(|_|denied())?;
    if write {
        if value["can_manage"] != true { return Err(denied()); }
    } else if value["can_manage"] != true && !app.store.live_access(&caller.user_id,agent).await
        .is_ok_and(|access|access.admitted()) { return Err(denied()); }
    Ok(value)
}
pub fn selected(params: &HashMap<String,String>) -> &str {
    params.get("agent_id").map(String::as_str).unwrap_or("shared_default")
}
pub async fn principal<C:RuntimeStorage>(app: &Runtime<'_,C>, identity: &Value) -> Result<Principal, ApiError> {
    let account = app.store.account_by_id(identity["_user_id"].as_str().unwrap_or("")).await?
        .ok_or_else(|| denied())?;
    let value = Principal::from_account(&account)?;
    if !value.active() { return Err(denied()); }
    Ok(value)
}
pub async fn read<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,params:HashMap<String,String>)->Api {
    let agent=selected(&params);
    authority(&app,&identity,agent,false).await?;
    response(&app,&identity,agent,&app.store.model_catalog().await?).await
}
pub async fn save<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,params:HashMap<String,String>,input:Catalog)->Api {
    let agent=selected(&params);
    let _guard=app.gate.lock().await;
    let rights=authority(&app,&identity,agent,true).await?;
    let caller=principal(&app,&identity).await?;
    let old=app.store.model_catalog().await?;
    if old.revision != input.revision {return Err(super::conflict("Model catalog revision changed; reload before saving"));}
    let view=scoped(&old,&caller.user_id,agent);
    if input.platform_sources!=view.platform_sources {return Err(super::bad("Platform provenance is server controlled"));}
    if input.entries.keys().chain(input.connections.keys()).any(|id|id.starts_with("platform:") && !view.entries.contains_key(id) && !view.connections.contains_key(id)) {return Err(denied());}
    let mut next=old.clone();
    // Shared records are read-only to recipients, even to installation administrators.
    macro_rules! merge {
        ($field:ident) => {
            for (id,record) in &view.$field {
                if !input.$field.contains_key(id) {return Err(super::bad("Revoke or unlink records explicitly; deletion is not supported"));}
                if record.owner_id != caller.user_id && input.$field.get(id) != Some(record) {return Err(denied());}
            }
            for (id,record) in &input.$field {
                if let Some(previous)=old.$field.get(id) {
                    if !view.$field.contains_key(id) {return Err(denied());}
                    if previous.owner_id != caller.user_id {continue;}
                    if record.owner_id != previous.owner_id {return Err(denied());}
                } else if record.owner_id != caller.user_id {return Err(denied());}
                for grant in &record.allowed_agents {
                    app.store.for_agent(grant).await.map_err(|_|super::bad("Grant agent does not exist"))?;
                }
                next.$field.insert(id.clone(),record.clone());
            }
        };
    }
    merge!(connections);
    merge!(entries);
    for (id,connection) in &mut next.connections {
        if let Some(previous)=old.connections.get(id) {
            if connection.credential_ref != previous.credential_ref || connection.transport != previous.transport {
                return Err(super::bad("Credential references and provider transport are server controlled"));
            }
        } else {
            // Client-supplied references must never select another tenant's stored credential.
            connection.credential_ref=format!("provider_credential:{}",uuid::Uuid::new_v4().to_string());
        }
    }
    for (id,entry) in &input.entries {
        if old.entries.get(id) != Some(entry) && entry.owner_id == caller.user_id {
            let connection=next.connections.get(&entry.connection_id).ok_or_else(||super::bad("Connection missing"))?;
            if connection.owner_id != caller.user_id {return Err(denied());}
            if !entry.allowed_agents.is_subset(&connection.allowed_agents) {return Err(super::bad("Entry grants exceed connection grants"));}
        }
    }
    // BYO-POLICY: the existing editor is not an alternate quota/permission lane.
    let tier=app.store.entitlement_tier(&caller.tier_id).await?
        .ok_or_else(||denied())?;
    let previous_count=old.entries.values().filter(|e|e.owner_id==caller.user_id).count();
    let next_count=next.entries.values().filter(|e|e.owner_id==caller.user_id).count();
    if !tier.allows_byo_growth(previous_count,next_count) {return Err(denied());}
    for (id,entry) in &next.entries {
        if entry.owner_id!=caller.user_id {continue;}
        let increased_use=old.entries.get(id).is_none_or(|previous|
            entry.upstream_model!=previous.upstream_model
            || entry.connection_id!=previous.connection_id
            || !entry.allowed_agents.is_subset(&previous.allowed_agents));
        if increased_use && !tier.allows_byo_model(&entry.upstream_model) {return Err(denied());}
    }
    if !input.assignments.is_empty() || !input.groups.is_empty()
        || input.agent_models.keys().any(|id|id != agent) {return Err(denied());}
    if let Some(models)=input.agent_models.get(agent) {
        if models.owner_id != rights["owner_id"].as_str().unwrap_or("") || models.agent_id != agent {return Err(denied());}
        let previous=old.agent_models.get(agent);
        let mut new_uses=std::collections::BTreeSet::new();
        for id in &models.entry_ids {
            if previous.is_none_or(|r|!r.entry_ids.contains(id)) {new_uses.insert(id);}
        }
        for (role,appointment) in &models.roles {
            if previous.and_then(|r|r.roles.get(role))!=Some(appointment) {
                new_uses.insert(&appointment.entry_id);
                new_uses.extend(&appointment.backup_entry_ids);
            }
        }
        for id in new_uses {
            if let Some(pin)=old.platform_sources.get(id) {
                app.store.resolve_account_model_source(&caller.user_id,&crate::account_catalog::Source{
                    origin:crate::account_catalog::Origin::Platform,
                    entry_id:crate::account_catalog::platform_id(&pin.roster_id,&pin.entry.id)}).await.map_err(|_|denied())?;
            } else {
                let entry=next.entries.get(id).ok_or_else(||super::bad("Role entry missing"))?;
                if !tier.allows_byo_model(&entry.upstream_model) {return Err(denied());}
            }
        }
        next.agent_models.insert(agent.to_owned(),models.clone());
    } else if view.agent_models.contains_key(agent) {return Err(super::bad("Clear roles explicitly"));}
    let saved=app.store.save_model_catalog(&next,input.revision).await
        .map_err(|_|super::bad("Catalog update rejected: check roster, roles, grants and revisions"))?;
    response(&app,&identity,agent,&saved).await
}
pub async fn connection_status<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,params:HashMap<String,String>)->Api {
    let caller=principal(&app,&identity).await?;
    let agent=selected(&params);
    let rights=authority(&app,&identity,agent,false).await?;
    let catalog=app.store.model_catalog().await?;
    let connection=catalog.connections.get(&id).ok_or_else(|| ApiError(StatusCode::NOT_FOUND,"Connection not found".into()))?;
    if connection.owner_id != caller.user_id && (!connection.allowed_agents.contains(agent)
        || !catalog.entries.values().any(|e|e.connection_id==id && e.allowed_agents.contains(agent))) {return Err(denied());}
    if rights["can_manage"] != true {
        let access=app.store.live_access(&caller.user_id,agent).await.map_err(|_|denied())?;
        let visible=!connection.revoked && connection.allowed_agents.contains(agent)
            && catalog.agent_models.get(agent).is_some_and(|models|models.entry_ids.iter().any(|entry_id|
                catalog.entries.get(entry_id).is_some_and(|entry|entry.connection_id == id
                    && entry.allowed_agents.contains(agent) && access.model_allowed(&entry.upstream_model))));
        if !visible {return Err(denied());}
    }
    let credential = if connection.revoked { None } else { app.store.get(&connection.credential_ref).await? };
    let connected = credential.is_some();
    // The subscription identity comes from the saved provider token and is visible only to its owner.
    let account_email = if connection.owner_id == caller.user_id
        && connection.transport == crate::model_selection::Transport::OpenaiSubscription {
        credential.as_ref().and_then(|value| value["email"].as_str())
    } else { None };
    let login=app.login.read().await;
    let authorization=login.as_ref().filter(|v| connection.owner_id == caller.user_id && v["connection_id"]==id).map(|v|json!({
        "status":v["status"],"verification_url":v["verification_url"],"user_code":v["user_code"]}));
    Ok(Json(json!({"id":id,"revoked":connection.revoked,
        "connected":connected,"account_email":account_email,"authorization":authorization})))
}
pub async fn pending_subscription_authorization<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,params:HashMap<String,String>)->Api {
    let caller=principal(&app,&identity).await?;
    let agent=selected(&params);
    authority(&app,&identity,agent,true).await?;
    let login=app.login.read().await;
    let Some(pending)=login.as_ref().filter(|v|v["status"]=="pending") else {
        return Ok(Json(json!({"pending":false})));
    };
    let Some(id)=pending["connection_id"].as_str() else {
        return Ok(Json(json!({"pending":false})));
    };
    let catalog=app.store.model_catalog().await?;
    let visible=catalog.connections.get(id).is_some_and(|c|c.owner_id==caller.user_id
        && c.allowed_agents.contains(agent) && !c.revoked
        && c.transport==crate::model_selection::Transport::OpenaiSubscription);
    if !visible { return Ok(Json(json!({"pending":false}))); }
    Ok(Json(json!({"pending":true,"connection_id":id,
        "verification_url":pending["verification_url"],"user_code":pending["user_code"],
        "expires_at":pending["expires_at"]})))
}
pub async fn subscription_models<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,params:HashMap<String,String>)->Api {
    let caller=principal(&app,&identity).await?;
    let agent=selected(&params);
    authority(&app,&identity,agent,true).await?;
    let catalog=app.store.model_catalog().await?;
    let connection=catalog.connections.get(&id).filter(|c|c.owner_id==caller.user_id
        && c.allowed_agents.contains(agent) && !c.revoked
        && c.transport==crate::model_selection::Transport::OpenaiSubscription)
        .ok_or_else(denied)?;
    let tokens=app.tokens_at(&connection.credential_ref).await
        .map_err(|_|ApiError(StatusCode::PRECONDITION_REQUIRED,"Connect ChatGPT to list models".into()))?;
    Ok(Json(app.provider.subscription_models(&tokens).await
        .map_err(|_|ApiError(StatusCode::BAD_GATEWAY,"Subscription model catalog unavailable".into()))?))
}
pub async fn connection_authorize<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    let caller=principal(&app,&identity).await?;
    let _guard=app.gate.lock().await;
    let mut catalog=app.store.model_catalog().await?;
    let connection=catalog.connections.get_mut(&id).ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Connection not found".into()))?;
    if connection.owner_id != caller.user_id {return Err(denied());}
    byo_credential_permission(&app,&caller).await?;
    if connection.revoked { return Err(super::conflict("Connection is revoked")); }
    if connection.transport != crate::model_selection::Transport::OpenaiSubscription { return Err(super::bad("This provider uses an API key, not device authorization")); }
    if let Some(pending) = app.login.read().await.as_ref().filter(|v|v["status"]=="pending") {
        if pending["connection_id"] == id {
            return Ok(Json(json!({"verification_url":pending["verification_url"],"user_code":pending["user_code"]})));
        }
        return Err(super::conflict("Authorization is pending for another connection. Reset it before retrying"));
    }
    // A failed device-start request must not disconnect the credential already
    // attached to this connection. The gate keeps polling from publishing until
    // the new reference has been committed.
    let reference=format!("provider_credential:{}",uuid::Uuid::new_v4().to_string());
    let started=app.provider_start_for(reference.clone(),Some(id.clone())).await?;
    catalog.connections.get_mut(&id).expect("connection checked above").credential_ref=reference;
    if let Err(error)=app.store.save_model_catalog(&catalog,catalog.revision).await {
        let mut login=app.login.write().await;
        if login.as_ref().is_some_and(|v|v["connection_id"]==id) { *login=None; }
        return Err(error.into());
    }
    Ok(started)
}
pub async fn connection_authorization_reset<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    let caller=principal(&app,&identity).await?;
    let catalog=app.store.model_catalog().await?;
    let connection=catalog.connections.get(&id).ok_or_else(||ApiError(StatusCode::NOT_FOUND,"Connection not found".into()))?;
    if connection.owner_id != caller.user_id {return Err(denied());}
    let mut login=app.login.write().await;
    if login.as_ref().is_some_and(|v|v["connection_id"]==id) {
        *login=None;
    }
    Ok(Json(json!({"reset":true})))
}
pub async fn subscription_credentials_reset<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,params:HashMap<String,String>)->Api {
    let caller=principal(&app,&identity).await?;
    let agent=selected(&params);
    authority(&app,&identity,agent,true).await?;
    let _guard=app.gate.lock().await;
    let mut catalog=app.store.model_catalog().await?;
    let mut ids=std::collections::BTreeSet::new();
    if let Some(roster)=catalog.agent_models.get(agent) {
        for entry_id in &roster.entry_ids {
            if let Some(entry)=catalog.entries.get(entry_id) { ids.insert(entry.connection_id.clone()); }
        }
    }
    let pending_id=app.login.read().await.as_ref()
        .and_then(|v|v["connection_id"].as_str().map(str::to_owned));
    let cancel_owned_pending=pending_id.as_ref().is_some_and(|id|
        catalog.connections.get(id).is_some_and(|c|c.owner_id==caller.user_id
            && c.transport==crate::model_selection::Transport::OpenaiSubscription));
    if let Some(id)=pending_id.as_ref() {
        if catalog.connections.get(id).is_some_and(|c|c.allowed_agents.contains(agent)) {
            ids.insert(id.clone());
        }
    }
    ids.retain(|id|catalog.connections.get(id).is_some_and(|c|
        c.owner_id==caller.user_id && c.allowed_agents.contains(agent) && !c.revoked
        && c.transport==crate::model_selection::Transport::OpenaiSubscription));
    for id in &ids {
        catalog.connections.get_mut(id).expect("connection checked above").credential_ref=
            format!("provider_credential:{}",uuid::Uuid::new_v4().to_string());
    }
    if !ids.is_empty() { app.store.save_model_catalog(&catalog,catalog.revision).await?; }
    let mut login=app.login.write().await;
    if cancel_owned_pending { *login=None; }
    Ok(Json(json!({"reset":true,"connection_ids":ids})))
}
pub async fn connection_key<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,body:ApiKey)->Api {
    let caller=principal(&app,&identity).await?;
    if body.api_key.len() < 8 || body.api_key.len() > 512 || !body.api_key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(super::bad("Invalid API key: use 8–512 printable non-space characters"));
    }
    let _guard = app.gate.lock().await;
    let mut catalog = app.store.model_catalog().await?;
    let connection = catalog.connections.get_mut(&id).ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Connection not found".into()))?;
    if connection.owner_id != caller.user_id {return Err(denied());}
    byo_credential_permission(&app,&caller).await?;
    if connection.revoked { return Err(super::conflict("Connection is revoked")); }
    if !connection.transport.uses_api_key() { return Err(super::bad("This connection does not use an API key")); }
    let reference = format!("provider_credential:{}", uuid::Uuid::new_v4().to_string());
    // Store the secret only in the encrypted credential plane. No key in catalog/events.
    app.store.put(&reference, &json!({"api_key":body.api_key})).await?;
    connection.credential_ref = reference;
    app.store.save_model_catalog(&catalog, catalog.revision).await?;
    Ok(Json(json!({"saved":true})))
}
pub async fn discover<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,input:Discovery)->Api {
    let caller=principal(&app,&identity).await?;
    if !caller.administrator(){byo_credential_permission(&app,&caller).await?;}
    let (provider,tokens)=if let Some(id)=input.connection_id {
        if input.provider.is_some()||input.api_key.is_some(){return Err(super::bad("Use one discovery credential source"));}
        let catalog=app.store.model_catalog().await?;
        let connection=catalog.connections.get(&id).filter(|c|c.owner_id==caller.user_id&&!c.revoked&&c.transport.uses_api_key())
            .ok_or_else(denied)?;
        (connection.transport.provider_id(),app.store.get(&connection.credential_ref).await?.ok_or_else(denied)?)
    } else {
        let provider=input.provider.ok_or_else(||super::bad("Provider required"))?;
        let key=input.api_key.ok_or_else(||super::bad("API key required"))?;
        if !key.bytes().all(|b|b.is_ascii_graphic())||!(8..=512).contains(&key.len())
            ||(provider.chat_endpoint().is_none()&&provider!=crate::provider::ProviderId::Typesafe){return Err(super::bad("Invalid discovery credential or provider"));}
        (provider,json!({"api_key":key}))
    };
    Ok(Json(app.provider.discover_models(provider,&tokens,tokio_util::sync::CancellationToken::new()).await
        .map_err(|_|ApiError(StatusCode::BAD_GATEWAY,"Provider discovery unavailable; check provider and credential".into()))?))
}

pub fn denied()->ApiError {ApiError(StatusCode::FORBIDDEN,"Agent or model administration unavailable".into())}
