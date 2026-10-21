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
pub struct DefaultUpdate {
    pub expected_revision: u64,
    pub default_entry_id: Option<String>,
}
pub async fn read<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value)->Api {
    let user=identity["_user_id"].as_str().unwrap_or("");
    Ok(Json(app.store.account_model_choices(user).await?))
}
pub async fn save<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,input:Preferences)->Api {
    let _guard=app.gate.lock().await;
    let user=identity["_user_id"].as_str().unwrap_or("");
    let current=app.store.account_model_preferences(user).await?;
    if input.revision!=current.revision {return Err(super::conflict("Account catalog changed; reload before saving"));}
    app.store.save_account_model_preferences(user,&input).await
        .map_err(|_|super::bad("Model preferences invalid or no longer entitled"))?;
    Ok(Json(app.store.account_model_choices(user).await?))
}
pub async fn set_default<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,input:DefaultUpdate)->Api {
    let _guard=app.gate.lock().await;
    let user=identity["_user_id"].as_str().unwrap_or("");
    let mut preferences=app.store.account_model_preferences(user).await?;
    if input.expected_revision!=preferences.revision {
        return Err(super::conflict("Account catalog changed; reload before saving"));
    }
    preferences.default_entry_id=input.default_entry_id;
    app.store.save_account_model_preferences(user,&preferences).await
        .map_err(|_|super::bad("Default model is no longer available"))?;
    Ok(Json(app.store.account_model_choices(user).await?))
}
pub async fn adopt<C:RuntimeStorage>(app:&Runtime<'_,C>,identity:Value,agent:String,input:crate::account_catalog::Adoption)->Api {
    let _guard=app.gate.lock().await;
    let user=identity["_user_id"].as_str().unwrap_or("");
    Ok(Json(app.store.adopt_byo_source(user,&agent,&input).await
        .map_err(|_|super::bad("Adoption rejected; verify source, authority and roster revision"))?))
}
