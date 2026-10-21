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
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Draft { pub expected_revision:u64, pub name:String, pub entries:BTreeMap<String,Entry> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fence { pub expected_revision:u64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rollback { pub expected_revision:u64, pub revision:u64 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyUpdate { pub expected_revision:u64, pub api_key:Option<String> }
pub async fn mutate<C:RuntimeStorage>(app:&Runtime<'_,C>, identity:&Value, id:&str, expected:u64, operation:Mutation)->Api {
    let _guard=app.gate.lock().await;
    super::administrator(app,identity).await?;
    if !webagent_access::valid_id(id) {return Err(super::bad("Invalid catalog identity"));}
    let current=app.store.platform_roster(id).await?;
    if current.as_ref().map_or(0,|r|r.revision)!=expected {
        return Err(super::conflict("Catalog revision changed; reload before saving"));
    }
    // Validate on a private copy before persistence; immutable history is never client writable.
    match &operation {
        Mutation::Draft{name,entries} => {
            let mut probe=current.clone().unwrap_or(crate::platform_catalog::Roster{
                id:id.into(),name:name.clone(),revision:1,draft:BTreeMap::new(),
                published_revision:None,history:vec![],retired:false});
            if probe.retired {return Err(super::bad("Catalog retired"));}
            probe.name=name.clone();probe.draft=entries.clone();
            probe.validate().map_err(|_|super::bad("Invalid catalog metadata"))?;
        }
        Mutation::Publish{rollback} => {
            let mut probe=current.clone().ok_or_else(||super::bad("Save a draft first"))?;
            probe.publish(expected,*rollback).map_err(|_|super::bad("Publication unavailable"))?;
        }
        Mutation::Retire => {
            if id=="base" || current.is_none() {return Err(super::bad("Catalog cannot retire"));}
        }
    }
    let roster=app.store.mutate_platform_roster(id,expected,operation,
        identity["_user_id"].as_str().unwrap_or("")).await?;
    Ok(Json(json!({"roster":roster})))
}
pub async fn list<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value)->Api {
    super::administrator(&app,&identity).await?;
    Ok(Json(json!({"rosters":app.store.platform_rosters().await?})))
}
pub async fn history<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    super::administrator(&app,&identity).await?;
    let roster=app.store.platform_roster(&id).await?
        .ok_or_else(||ApiError(axum::http::StatusCode::NOT_FOUND,"Catalog not found".into()))?;
    Ok(Json(json!({"history":roster.history,"published_revision":roster.published_revision})))
}
pub async fn read<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    super::administrator(&app,&identity).await?;
    let roster=app.store.platform_roster(&id).await?
        .ok_or_else(||ApiError(axum::http::StatusCode::NOT_FOUND,"Catalog not found".into()))?;
    Ok(Json(json!({"roster":roster})))
}
pub async fn draft<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,input:Draft)->Api {
    mutate(&app,&identity,&id,input.expected_revision,Mutation::Draft{name:input.name,entries:input.entries}).await
}
pub async fn publish<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,input:Fence)->Api {
    mutate(&app,&identity,&id,input.expected_revision,Mutation::Publish{rollback:None}).await
}
pub async fn rollback<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,input:Rollback)->Api {
    mutate(&app,&identity,&id,input.expected_revision,Mutation::Publish{rollback:Some(input.revision)}).await
}
pub async fn retire<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String,input:Fence)->Api {
    mutate(&app,&identity,&id,input.expected_revision,Mutation::Retire).await
}
pub async fn credential<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,(id,entry_id):(String,String),input:KeyUpdate)->Api {
    let _guard=app.gate.lock().await;
    super::administrator(&app,&identity).await?;
    if !webagent_access::valid_id(&id) || !webagent_access::valid_id(&entry_id) {
        return Err(super::bad("Invalid catalog identity"));
    }
    let roster=app.store.platform_roster(&id).await?
        .ok_or_else(||super::bad("Catalog not found"))?;
    if roster.revision!=input.expected_revision {return Err(super::conflict("Catalog revision changed"));}
    let entry=roster.draft.get(&entry_id).ok_or_else(||super::bad("Draft entry not found"))?;
    if roster.retired || !entry.transport.uses_api_key()
        || input.api_key.as_ref().is_some_and(|key|key.is_empty() || key.len()>4096
            || !key.bytes().all(|b|b.is_ascii_graphic())) {
        return Err(super::bad("Invalid credential input or unavailable transport"));
    }
    app.store.platform_credential_set(&id,&entry_id,input.expected_revision,
        input.api_key.as_deref(),identity["_user_id"].as_str().unwrap_or("")).await?;
    Ok(Json(json!({"saved":true,"revision":input.expected_revision+1})))
}
pub async fn validate<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    super::administrator(&app,&identity).await?;
    let roster=app.store.platform_roster(&id).await?
        .ok_or_else(||super::bad("Catalog not found"))?;
    let mut entries=Vec::new();
    for entry in roster.draft.values() {
        let configured=app.store.platform_credential_configured(&id,entry).await?;
        entries.push(json!({"entry_id":entry.id,"configured":configured,
            "warning":if configured {Value::Null} else {json!("Credential not configured")}}));
    }
    Ok(Json(json!({"revision":roster.revision,"entries":entries,
        "provider_acceptance_verified":false})))
}
pub async fn preview<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,id:String)->Api {
    super::administrator(&app,&identity).await?;
    let roster=app.store.platform_roster(&id).await?.ok_or_else(||super::bad("Catalog not found"))?;
    let published=roster.published().map(|snapshot|&snapshot.entries);
    let mut added=Vec::new();let mut changed=Vec::new();let mut removed=Vec::new();
    for (id,entry) in &roster.draft {
        match published.and_then(|entries|entries.get(id)) {
            None=>added.push(id.clone()),Some(old) if old!=entry=>changed.push(id.clone()),_=>{}
        }
    }
    if let Some(entries)=published {for id in entries.keys() {if !roster.draft.contains_key(id) {removed.push(id.clone());}}}
    let tiers:Vec<_>=app.store.entitlement_tiers().await?.into_iter()
        .filter(|tier|tier.model_policy.as_ref().is_some_and(|policy|policy.roster_id.as_deref()==Some(&roster.id)))
        .map(|tier|tier.id).collect();
    Ok(Json(json!({"revision":roster.revision,"published_revision":roster.published_revision,
        "retired":roster.retired,"added_entry_ids":added,"changed_entry_ids":changed,
        "removed_entry_ids":removed,"bound_tier_ids":tiers,"provider_acceptance_verified":false})))
}
