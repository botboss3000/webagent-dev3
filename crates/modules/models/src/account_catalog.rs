//! Caller catalog projections and preferences. No connection or credential is client writable here.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use webagent_access::{Principal, Tier};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preferences {
    pub revision:u64,
    pub selected_platform_ids:BTreeSet<String>,
    pub aliases:BTreeMap<String,String>,
    pub default_entry_id:Option<String>,
}
impl Preferences {
    fn validate(&self)->Result<()> {
        if self.selected_platform_ids.len()>256 || self.aliases.len()>512
            || self.selected_platform_ids.iter().chain(self.aliases.keys()).any(|id|id.len()>256 || id.is_empty() || id.chars().any(char::is_control))
            || self.aliases.values().any(|name|name.trim().is_empty() || name.len()>200 || name.chars().any(char::is_control)) {
            bail!("Invalid account model preferences");
        }
        Ok(())
    }
}
pub fn platform_id(roster:&str,entry:&str)->String {format!("platform:{roster}:{entry}")}
pub fn byo_id(entry:&str)->String {format!("byo:{entry}")}
/// Client input names only an opaque choice; provider and credential references
/// are deliberately absent. Keep this separate from writable model records.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub origin: Origin,
    pub entry_id: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin { Platform, Byo }

/// Internal resolver result, never serialized into an account projection.
#[derive(Clone, Debug)]
pub enum ResolvedSource {
    Platform { roster_id:String, publication_revision:u64, entry:crate::platform_catalog::Entry },
    Byo { entry:crate::model_selection::ModelEntry, connection:crate::model_selection::Connection },
}
impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_,C> {
    async fn catalog_principal(&self,user:&str)->Result<(Principal,Tier)> {
        let account=self.access().account_by_id(user).await?.ok_or_else(||anyhow::anyhow!("Account unavailable"))?;
        let principal=Principal::from_account(&account)?;
        if !principal.active() {bail!("Account inactive");}
        let tier=self.access().entitlement_tier(&principal.tier_id).await?.ok_or_else(||anyhow::anyhow!("Tier unavailable"))?;
        Ok((principal,tier))
    }
    /// Rehydrate an account choice from live authority. This is not an adoption
    /// grant: the caller must additionally check management authority and persist
    /// the agent appointment atomically. Never reuse an agent projection here.
    pub async fn resolve_account_model_source(&self,user:&str,source:&Source)->Result<ResolvedSource> {
        let (principal,tier)=self.catalog_principal(user).await?;
        match source.origin {
            Origin::Platform => {
                let mut parts=source.entry_id.split(':');
                let prefix=parts.next();let roster_id=parts.next();let entry_id=parts.next();
                let (Some(roster_id),Some(entry_id))=(roster_id,entry_id) else {bail!("Invalid platform source");};
                if prefix!=Some("platform") || parts.next().is_some()
                    || !webagent_access::valid_id(roster_id) || !webagent_access::valid_id(entry_id) {
                    bail!("Invalid platform source");
                }
                let policy=tier.model_policy.as_ref().ok_or_else(||anyhow::anyhow!("Platform access not granted"))?;
                if policy.roster_id.as_deref()!=Some(roster_id) || !policy.allowed_entry_ids.contains(entry_id)
                    || (roster_id=="base" && !principal.administrator()) {
                    bail!("Platform source not entitled");
                }
                let roster=self.platform_roster(roster_id).await?.ok_or_else(||anyhow::anyhow!("Platform source unavailable"))?;
                let publication=roster.published().ok_or_else(||anyhow::anyhow!("Platform source unpublished or retired"))?;
                let entry=publication.entries.get(entry_id).ok_or_else(||anyhow::anyhow!("Platform entry unavailable"))?;
                Ok(ResolvedSource::Platform{roster_id:roster_id.into(),publication_revision:publication.revision,entry:entry.clone()})
            }
            Origin::Byo => {
                let id=source.entry_id.strip_prefix("byo:").ok_or_else(||anyhow::anyhow!("Invalid BYO source"))?;
                let catalog=self.model_catalog().await?;
                let entry=catalog.entries.get(id).filter(|e|e.owner_id==user && tier.allows_byo_model(&e.upstream_model))
                    .ok_or_else(||anyhow::anyhow!("BYO source unavailable"))?;
                let connection=catalog.connections.get(&entry.connection_id).filter(|c|c.owner_id==user && !c.revoked)
                    .ok_or_else(||anyhow::anyhow!("BYO connection unavailable"))?;
                Ok(ResolvedSource::Byo{entry:entry.clone(),connection:connection.clone()})
            }
        }
    }
    pub async fn account_model_preferences(&self,user:&str)->Result<Preferences> {
        use sqlx::Row;
        if !webagent_access::valid_id(user) {bail!("Invalid account identity");}
        let row=sqlx::query("SELECT value FROM docs WHERE key=?").bind(format!("account_models:v1:{user}"))
            .fetch_optional(self.app_pool).await?;
        let preferences:Preferences=match row {Some(row)=>serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?,None=>Preferences::default()};
        preferences.validate()?;Ok(preferences)
    }
    /// Safe public metadata only; drafts and shared agent credentials cannot enter the BYO lane.
    pub async fn account_model_choices(&self,user:&str)->Result<Value> {
        let (principal,tier)=self.catalog_principal(user).await?;
        let preferences=self.account_model_preferences(user).await?;
        let mut platform=Vec::new();
        if let Some(policy)=&tier.model_policy {
            if let Some(id)=&policy.roster_id {
                if id!="base" || principal.administrator() {
                    if let Some(roster)=self.platform_roster(id).await? {
                        if let Some(snapshot)=roster.published() {
                            for entry in snapshot.entries.values().filter(|e|policy.allowed_entry_ids.contains(&e.id)) {
                                platform.push(json!({"id":platform_id(id,&entry.id),"origin":"platform",
                                    "display_name":entry.display_name,"transport":entry.transport,"capabilities":entry.capabilities,
                                    "publication_revision":snapshot.revision,
                                    "configured":self.platform_credential_configured(id,entry).await?}));
                            }
                        }
                    }
                }
            }
        }
        let catalog=self.model_catalog().await?;
        let can_byo=tier.model_policy.as_ref().map_or(!tier.allowed_models.is_empty(),|p|p.allow_byo);
        let mut byo=Vec::new();
        if can_byo {
            for entry in catalog.entries.values().filter(|e|e.owner_id==user) {
                if let Some(connection)=catalog.connections.get(&entry.connection_id).filter(|c|c.owner_id==user && !c.revoked) {
                    if tier.allows_byo_model(&entry.upstream_model) {
                        byo.push(json!({"id":byo_id(&entry.id),"origin":"byo","display_name":entry.display_name,
                            "upstream_model":entry.upstream_model,"transport":connection.transport,"capabilities":entry.capabilities}));
                    }
                }
            }
        }
        Ok(json!({"revision":preferences.revision,"available_platform_models":platform,"entries":byo,
            "selected_platform_ids":preferences.selected_platform_ids,"aliases":preferences.aliases,
            "default_entry_id":preferences.default_entry_id,"can_byo":can_byo,
            "max_byo_entries":tier.model_policy.as_ref().map(|p|p.max_byo_entries)}))
    }
    /// Caller authority is checked by the API; storage also rechecks live account entitlement.
    pub async fn save_account_model_preferences(&self,user:&str,input:&Preferences)->Result<Preferences> {
        use sqlx::Row;
        input.validate()?;
        let choices=self.account_model_choices(user).await?;
        let platform:BTreeSet<String>=choices["available_platform_models"].as_array().unwrap().iter()
            .filter_map(|v|v["id"].as_str().map(str::to_owned)).collect();
        let byo:BTreeSet<String>=choices["entries"].as_array().unwrap().iter()
            .filter_map(|v|v["id"].as_str().map(str::to_owned)).collect();
        if !input.selected_platform_ids.is_subset(&platform)
            || input.aliases.keys().any(|id|!platform.contains(id) && !byo.contains(id))
            || input.default_entry_id.as_ref().is_some_and(|id|!byo.contains(id) && !input.selected_platform_ids.contains(id)) {
            bail!("Model choice is not available to this account");
        }
        let key=format!("account_models:v1:{user}");
        let mut tx=self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(&key).execute(&mut *tx).await?;
        let row=sqlx::query("SELECT value FROM docs WHERE key=?").bind(&key).fetch_optional(&mut *tx).await?;
        let previous:Preferences=match row {Some(row)=>serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?,None=>Preferences::default()};
        if previous.revision!=input.revision {bail!("Account catalog revision changed");}
        let mut saved=input.clone();saved.revision=input.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(key).bind(self.seal(&serde_json::to_value(&saved)?)?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(saved)
    }
}
#[cfg(test)]
mod tests {
    use super::*;

}

#[cfg(test)]
mod source_tests {
    use super::*;


}

/// Typed adoption input. Upstream/provider/credential data never comes from this request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adoption {
    pub client_request_id:String,
    pub expected_roster_revision:u64,
    pub source:Source,
    pub alias:String,
}
impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_,C> {
    /// Caller serializes policy mutations with the application gate. Catalog grants,
    /// local alias/provenance and retry receipt commit together in the application DB.
    /// Safe roster-local metadata only. Authority and current roster membership
    /// are mandatory; stale adoption documents never confer availability.
    pub async fn agent_model_adoptions(&self,user:&str,agent:&str)->Result<Value> {
        use sqlx::Row;
        let authority=self.agent_authority(user,agent).await?;
        if authority["can_manage"]!=true && !self.live_access(user,agent).await?.admitted() {
            bail!("Agent access required");
        }
        let catalog=self.model_catalog().await?;
        let mut result=serde_json::Map::new();
        if let Some(roster)=catalog.agent_models.get(agent) {
            for id in &roster.entry_ids {
                let Some(entry)=catalog.entries.get(id) else {continue};
                if !entry.allowed_agents.contains(agent) {continue;}
                let Some(connection)=catalog.connections.get(&entry.connection_id) else {continue};
                if connection.revoked || !connection.allowed_agents.contains(agent) {continue;}
                let access=self.live_access(user,agent).await?;
                if authority["can_manage"]!=true {
                    if let Some(pin)=catalog.platform_sources.get(id) {
                        let source=Source{origin:Origin::Platform,entry_id:platform_id(&pin.roster_id,&pin.entry.id)};
                        if self.resolve_account_model_source(user,&source).await.is_err() {continue;}
                    } else if !access.model_allowed(&entry.upstream_model) {continue;}
                }
                let row=sqlx::query("SELECT value FROM docs WHERE key=?")
                    .bind(format!("model_adoption:v1:{agent}:{id}")).fetch_optional(self.app_pool).await?;
                if let Some(row)=row {
                    let value=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
                    // Closed projection: do not forward arbitrary stored fields.
                    result.insert(id.clone(),json!({"origin":value["origin"],"alias":value["alias"],"source_id":value["source_id"]}));
                }
            }
        }
        Ok(Value::Object(result))
    }
    /// Only creation calls this path. Existing rosters, including cleared roles,
    /// are never reseeded; adoption and the initial appointment share one commit.
    pub async fn seed_new_agent_default(&self,user:&str,agent:&str)->Result<bool> {
        let catalog=self.model_catalog().await?;
        if let Some(roster)=catalog.agent_models.get(agent) {
            return Ok(roster.roles.contains_key(&crate::model_selection::ModelRole::Standard));
        }
        let preferences=self.account_model_preferences(user).await?;
        let Some(id)=preferences.default_entry_id else {return Ok(false);};
        let source=Source{origin:if id.starts_with("platform:"){Origin::Platform}else{Origin::Byo},entry_id:id.clone()};
        let resolved=match self.resolve_account_model_source(user,&source).await {Ok(value)=>value,Err(_)=>return Ok(false)};
        let name=match resolved {ResolvedSource::Platform{entry,..}=>entry.display_name,ResolvedSource::Byo{entry,..}=>entry.display_name};
        let input=Adoption{client_request_id:"new-agent-default-v1".into(),expected_roster_revision:0,
            source,alias:preferences.aliases.get(&id).cloned().unwrap_or(name)};
        self.adopt_account_source(user,agent,&input,true).await?;Ok(true)
    }
    pub async fn adopt_byo_source(&self,user:&str,agent:&str,input:&Adoption)->Result<Value> {
        self.adopt_account_source(user,agent,input,false).await
    }
    async fn adopt_account_source(&self,user:&str,agent:&str,input:&Adoption,initial_standard:bool)->Result<Value> {
        use sqlx::Row;
        use crate::model_selection::{Catalog,AgentModels};
        if input.client_request_id.is_empty() || input.client_request_id.len()>128
            || input.client_request_id.chars().any(char::is_control)
            || input.alias.trim().is_empty() || input.alias.len()>200 || input.alias.chars().any(char::is_control) {
            bail!("Invalid adoption input");
        }
        let authority=self.agent_authority(user,agent).await?;
        if authority["can_manage"]!=true {bail!("Agent management required");}
        let resolved=self.resolve_account_model_source(user,&input.source).await?;
        let platform_pin=match &resolved {
            ResolvedSource::Platform{..}=>Some(self.pin_platform_source(user,&input.source).await?),_=>None,
        };
        let (source,source_connection)=match resolved {
            ResolvedSource::Byo{entry,connection}=>(entry,connection),
            ResolvedSource::Platform{roster_id,entry,..}=>{
                let id=platform_id(&roster_id,&entry.id);
                (crate::model_selection::ModelEntry{id:id.clone(),owner_id:"platform_catalog".into(),connection_id:id.clone(),
                    upstream_model:entry.upstream_model,display_name:entry.display_name,capabilities:entry.capabilities,allowed_agents:BTreeSet::new(),revision:1},
                 crate::model_selection::Connection{id:id.clone(),owner_id:"platform_catalog".into(),transport:entry.transport,
                    credential_ref:format!("platform_key:{roster_id}:{}",entry.id),allowed_agents:BTreeSet::new(),revision:1,revoked:false})
            }
        };
        let receipt_key=format!("receipt:model-adoption:{user}:{agent}:{}",input.client_request_id);
        let digest=webagent_configuration::digest(&serde_json::to_value(input)?);
        let catalog_key="model_selection:catalog:v1";
        let mut tx=self.app_pool.begin().await?;
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(catalog_key).execute(&mut *tx).await?;
        if let Some(row)=sqlx::query("SELECT value FROM docs WHERE key=?").bind(&receipt_key).fetch_optional(&mut *tx).await? {
            let receipt=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if receipt["digest"]!=digest {bail!("Request identifier reused with different adoption");}
            return Ok(receipt["result"].clone());
        }
        let mut catalog=self.read_catalog_connection(&mut tx).await?;
        catalog.migrate_stored(&mut tx).await?;
        if let Some(pin)=&platform_pin {
            if let Some(existing)=catalog.entries.get(&source.id) {
                if existing.owner_id!="platform_catalog" || existing.connection_id!=source_connection.id {bail!("Platform identity collision");}
            }
            catalog.entries.entry(source.id.clone()).or_insert_with(||source.clone());
            catalog.connections.entry(source_connection.id.clone()).or_insert_with(||source_connection.clone());
            catalog.platform_sources.insert(source.id.clone(),pin.clone());
        } else if catalog.entries.get(&source.id)!=Some(&source) || catalog.connections.get(&source_connection.id)!=Some(&source_connection) {
            bail!("Source changed; reload before adopting");
        }
        let previous_revision=catalog.agent_models.get(agent).map_or(0,|roster|roster.revision);
        if previous_revision!=input.expected_roster_revision {bail!("Agent roster revision changed");}
        let entry=catalog.entries.get_mut(&source.id).unwrap();
        if entry.allowed_agents.insert(agent.into()) {
            entry.revision=entry.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        }
        let connection=catalog.connections.get_mut(&source_connection.id).unwrap();
        if connection.allowed_agents.insert(agent.into()) {
            connection.revision=connection.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        }
        let roster=catalog.agent_models.entry(agent.into()).or_insert_with(||AgentModels{limits:std::collections::BTreeMap::new(),efforts:std::collections::BTreeMap::new(),
            agent_id:agent.into(),owner_id:authority["owner_id"].as_str().unwrap().into(),entry_ids:vec![],roles:BTreeMap::new(),revision:0});
        if !roster.entry_ids.contains(&source.id) {roster.entry_ids.push(source.id.clone());}
        if initial_standard {
            if previous_revision!=0 || !roster.roles.is_empty() {bail!("Existing roster cannot be seeded");}
            roster.roles.insert(crate::model_selection::ModelRole::Standard,crate::model_selection::RoleAssignment{
                entry_id:source.id.clone(),backup_entry_ids:vec![]});
        }
        roster.revision=previous_revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        let origin=if platform_pin.is_some(){"platform"}else{"byo"};
        let result=json!({"entry_id":source.id,"origin":origin,"alias":input.alias.trim(),"roster_revision":roster.revision});
        catalog.revision=catalog.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        catalog.validate()?;
        let provenance=json!({"origin":origin,"source_id":input.source.entry_id,"adopted_by":user,"alias":input.alias.trim()});
        self.stage_catalog_connection(&mut tx,&catalog).await?;
        for (key,value) in [(format!("model_adoption:v1:{agent}:{}",source.id),provenance),
            (receipt_key,json!({"digest":digest,"result":result}))] {
            sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                .bind(key).bind(self.seal(&value)?).execute(&mut *tx).await?;
        }
        if platform_pin.is_some() {
            let preferences_key=format!("account_models:v1:{user}");
            let row=sqlx::query("SELECT value FROM docs WHERE key=?").bind(&preferences_key).fetch_optional(&mut *tx).await?;
            let mut preferences:Preferences=match row {Some(row)=>serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?,None=>Preferences::default()};
            if preferences.selected_platform_ids.insert(input.source.entry_id.clone()) {
                preferences.revision=preferences.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
                preferences.validate()?;
                sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
                    .bind(preferences_key).bind(self.seal(&serde_json::to_value(preferences)?)?).execute(&mut *tx).await?;
            }
        }
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('','model_adopted',?)")
            .bind(self.seal(&json!({"actor":user,"agent_id":agent,"entry_id":source.id,"origin":origin}))?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(result)
    }
}
#[cfg(test)]
mod adoption_tests {
    use super::*;

}

/// Private admission snapshot. Serializable for encrypted run storage, never an
/// account response; it pins metadata and credential generation without secrets.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlatformPin {
    pub roster_id:String,
    pub publication_revision:u64,
    pub entry:crate::platform_catalog::Entry,
    pub credential_generation:String,
}
impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_,C> {
    pub async fn pin_platform_source(&self,user:&str,source:&Source)->Result<PlatformPin> {
        let ResolvedSource::Platform{roster_id,publication_revision,entry}=self.resolve_account_model_source(user,source).await? else {
            bail!("Platform source required");
        };
        let credential_generation=self.platform_credential_generation(&roster_id,&entry).await?;
        Ok(PlatformPin{roster_id,publication_revision,entry,credential_generation})
    }
    /// Live entitlement and agent relationship are required on every dispatch.
    /// This internal primitive does not itself appoint or authorize a roster row.
    pub async fn platform_dispatch_tokens(&self,user:&str,agent:&str,pin:&PlatformPin)->Result<Value> {
        let access=self.live_access(user,agent).await?;
        if !access.admitted() {bail!("Agent access revoked");}
        let source=Source{origin:Origin::Platform,entry_id:platform_id(&pin.roster_id,&pin.entry.id)};
        let ResolvedSource::Platform{entry,..}=self.resolve_account_model_source(user,&source).await? else {
            bail!("Platform source required");
        };
        if entry.transport!=pin.entry.transport {bail!("Platform transport changed");}
        // Rename or publication alone cannot reroute the accepted upstream model.
        self.platform_tokens_pinned(&pin.roster_id,&pin.entry,&pin.credential_generation).await
    }
}
#[cfg(test)]
mod platform_pin_tests {
    use super::*;

}
