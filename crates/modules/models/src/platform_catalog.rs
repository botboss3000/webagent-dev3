//! Role-free platform publication metadata. Credentials deliberately cannot occur in snapshots.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use crate::model_selection::Transport;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: String,
    pub display_name: String,
    pub upstream_model: String,
    pub transport: Transport,
    pub capabilities: BTreeSet<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub revision: u64,
    pub entries: BTreeMap<String, Entry>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Roster {
    pub id: String,
    pub name: String,
    pub revision: u64,
    pub draft: BTreeMap<String, Entry>,
    pub published_revision: Option<u64>,
    pub history: Vec<Snapshot>,
    pub retired: bool,
}
impl Roster {
    pub fn validate(&self) -> Result<()> {
        if !webagent_access::valid_id(&self.id) || self.name.trim().is_empty()
            || self.name.len()>200 || self.revision==0 || self.draft.len()>256
            || self.history.len()>256 || (self.id=="base" && self.retired) {
            bail!("Invalid platform roster");
        }
        for entries in std::iter::once(&self.draft).chain(self.history.iter().map(|s|&s.entries)) {
            if entries.len()>256 {bail!("Platform roster exceeds limit");}
            for (id,e) in entries {
                if id!=&e.id || !webagent_access::valid_id(id) || e.display_name.trim().is_empty()
                    || e.display_name.len()>200 || !valid_model(&e.upstream_model)
                    // A bridge link answers only its owner's chats, so it cannot be published to others.
                    || e.transport==Transport::ClaudeBridge
                    || e.capabilities.iter().any(|c|!matches!(c.as_str(),"text"|"decision"|"image_in"|"image_out"|"voice"|"video")) {
                    bail!("Invalid platform model metadata");
                }
            }
        }
        let mut revisions=BTreeSet::new();
        if self.history.iter().any(|s|s.revision==0 || s.revision>self.revision || !revisions.insert(s.revision))
            || self.published_revision.is_some_and(|r|!revisions.contains(&r)) {
            bail!("Invalid platform publication history");
        }
        Ok(())
    }
    pub fn published(&self) -> Option<&Snapshot> {
        if self.retired {return None;}
        self.published_revision.and_then(|r|self.history.iter().find(|s|s.revision==r))
    }
    /// Revision fences also cover drafts: rollback always creates a NEW publication.
    pub fn publish(&mut self, expected:u64, rollback:Option<u64>) -> Result<()> {
        if self.revision!=expected {bail!("Platform roster revision changed");}
        if self.retired || self.history.len()>=256 {bail!("Platform roster cannot publish");}
        self.validate()?;
        let entries=match rollback {
            Some(r)=>self.history.iter().find(|s|s.revision==r)
                .ok_or_else(||anyhow::anyhow!("Publication not found"))?.entries.clone(),
            None=>self.draft.clone(),
        };
        self.revision=self.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        self.history.push(Snapshot{revision:self.revision,entries});
        self.published_revision=Some(self.revision);
        self.validate()
    }
}
pub use crate::valid_model;

#[cfg(test)]
mod tests {
    use super::*;
    fn roster()->Roster {Roster{id:"base".into(),name:"Base".into(),revision:1,draft:BTreeMap::new(),published_revision:None,history:vec![],retired:false}}
    #[test] fn drafts_do_not_publish_and_stale_publish_fails() {
        let mut r=roster(); assert!(r.published().is_none());
        r.publish(1,None).unwrap(); assert_eq!(r.published().unwrap().revision,2);
        assert!(r.publish(1,None).is_err());
    }
    #[test] fn rollback_creates_new_revision_and_base_cannot_retire() {
        let mut r=roster();r.publish(1,None).unwrap();r.publish(2,Some(2)).unwrap();
        assert_eq!(r.published_revision,Some(3));assert_eq!(r.history.len(),2);
        r.retired=true;assert!(r.validate().is_err());
    }
    #[test] fn typesafe_decision_only_entry_validates_and_uses_api_key() {
        let transport: Transport=serde_json::from_str("\"typesafe\"").unwrap();
        assert!(transport.uses_api_key());
        let mut roster=roster();
        roster.draft.insert("jev".into(), Entry {
            id:"jev".into(),display_name:"Jev".into(),upstream_model:"jev-latest".into(),
            transport,capabilities:BTreeSet::from(["decision".into()]),
        });
        roster.validate().unwrap();
        assert!(!roster.draft["jev"].capabilities.contains("text"));
    }
    #[test] fn secrets_and_roles_are_rejected_and_namespaced_models_validate() {
        assert!(valid_model("organization/model-name"));assert!(!valid_model("model\n"));
        assert!(serde_json::from_value::<Entry>(serde_json::json!({"id":"x","display_name":"X","upstream_model":"m","transport":"deepseek","capabilities":["text"],"api_key":"secret"})).is_err());
    }
}

// Application-owned encrypted documents, separate from private agent connections.
const PREFIX: &str = "platform_catalog:v1:";
impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_,C> {
    pub async fn platform_rosters(&self) -> Result<Vec<Roster>> {
        use sqlx::Row;
        let rows = sqlx::query("SELECT value FROM docs WHERE substr(key,1,?)=? ORDER BY key")
            .bind(PREFIX.len() as i64).bind(PREFIX).fetch_all(self.app_pool).await?;
        let mut result=Vec::new();
        for row in rows {
            let roster:Roster=serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
            roster.validate()?;result.push(roster);
        }
        Ok(result)
    }
    pub async fn platform_roster(&self, id: &str) -> Result<Option<Roster>> {
        use sqlx::Row;
        if !webagent_access::valid_id(id) { bail!("Invalid platform roster identity"); }
        let row = sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(format!("{PREFIX}{id}")).fetch_optional(self.app_pool).await?;
        let Some(row) = row else { return Ok(None); };
        let roster: Roster = serde_json::from_value(self.unseal(&row.get::<Vec<u8>, _>("value"))?)?;
        roster.validate()?;
        if roster.id != id { bail!("Platform roster identity mismatch"); }
        Ok(Some(roster))
    }
    /// Typed lifecycle operations only: clients cannot replace publication history.
    pub async fn mutate_platform_roster(&self, id: &str, expected: u64,
        operation: Mutation, actor: &str) -> Result<Roster> {
        use sqlx::Row;
        if !webagent_access::valid_id(id) || !webagent_access::valid_id(actor) {
            bail!("Invalid platform roster identity");
        }
        let key = format!("{PREFIX}{id}");
        let mut tx = self.app_pool.begin().await?;
        // Take SQLite's write reservation even for first creation.
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(&key).execute(&mut *tx).await?;
        let row = sqlx::query("SELECT value FROM docs WHERE key=?").bind(&key).fetch_optional(&mut *tx).await?;
        let mut roster: Roster = match row {
            Some(row) => serde_json::from_value(self.unseal(&row.get::<Vec<u8>, _>("value"))?)?,
            None => {
                if expected != 0 { bail!("Platform roster revision changed"); }
                Roster { id:id.into(), name:id.into(), revision:0, draft:BTreeMap::new(),
                    published_revision:None, history:vec![], retired:false }
            }
        };
        if roster.id != id || roster.revision != expected { bail!("Platform roster revision changed"); }
        let kind = match operation {
            Mutation::Draft { name, entries } => {
                if roster.retired { bail!("Platform roster retired"); }
                roster.name=name; roster.draft=entries;
                roster.revision=expected.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
                "platform_catalog_draft"
            }
            Mutation::Publish { rollback } => { roster.publish(expected,rollback)?; "platform_catalog_publish" }
            Mutation::Retire => {
                if id=="base" || expected==0 { bail!("Platform roster cannot retire"); }
                roster.retired=true;
                roster.revision=expected.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
                "platform_catalog_retire"
            }
        };
        roster.validate()?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&key).bind(self.seal(&serde_json::to_value(&roster)?)?).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('',?,?)")
            .bind(kind).bind(self.seal(&serde_json::json!({"actor":actor,"roster_id":id,"revision":roster.revision}))?)
            .execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(roster)
    }
}
pub enum Mutation {
    Draft { name:String, entries:BTreeMap<String,Entry> },
    Publish { rollback:Option<u64> },
    Retire,
}
#[cfg(test)]
mod persistence_tests {
    use super::*;

}

/// Private current credential state. Never serialized into publication metadata.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    generation: String,
    transport: Transport,
    api_key: Option<String>,
}
impl<C: crate::storage::RuntimeStorage> crate::storage::Repository<'_,C> {
    /// The caller must authorize installation administration. Rotation advances the
    /// roster fence and mints a generation; clearing retains a tombstone generation.
    pub async fn platform_credential_set(&self, roster_id:&str, entry_id:&str,
        expected:u64, api_key:Option<&str>, actor:&str) -> Result<String> {
        use sqlx::Row;
        if !webagent_access::valid_id(roster_id) || !webagent_access::valid_id(entry_id)
            || !webagent_access::valid_id(actor) || api_key.is_some_and(|key|
                key.is_empty() || key.len()>4096 || !key.bytes().all(|b|b.is_ascii_graphic())) {
            bail!("Invalid platform credential input");
        }
        let mut tx=self.app_pool.begin().await?;
        let key=format!("{PREFIX}{roster_id}");
        sqlx::query("UPDATE docs SET value=value WHERE key=?").bind(&key).execute(&mut *tx).await?;
        let row=sqlx::query("SELECT value FROM docs WHERE key=?").bind(&key)
            .fetch_optional(&mut *tx).await?.ok_or_else(||anyhow::anyhow!("Catalog not found"))?;
        let mut roster:Roster=serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
        roster.validate()?;
        if roster.id!=roster_id || roster.revision!=expected || roster.retired {bail!("Catalog revision changed or retired");}
        let entry=roster.draft.get(entry_id).ok_or_else(||anyhow::anyhow!("Draft entry not found"))?;
        // Subscription authorization is a separate interactive contract, not an API key.
        if !entry.transport.uses_api_key() {bail!("Transport does not accept API keys");}
        let generation=uuid::Uuid::new_v4().to_string();
        let credential=Credential{generation:generation.clone(),transport:entry.transport.clone(),api_key:api_key.map(str::to_owned)};
        let credential_key=format!("platform_credential:v1:{roster_id}:{entry_id}");
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(credential_key).bind(self.seal(&serde_json::to_value(credential)?)?).execute(&mut *tx).await?;
        roster.revision=expected.checked_add(1).ok_or_else(||anyhow::anyhow!("Revision exhausted"))?;
        sqlx::query("UPDATE docs SET value=? WHERE key=?").bind(self.seal(&serde_json::to_value(&roster)?)?)
            .bind(key).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('','platform_credential_changed',?)")
            .bind(self.seal(&serde_json::json!({"actor":actor,"roster_id":roster_id,"entry_id":entry_id,
                "revision":roster.revision,"configured":api_key.is_some()}))?).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(generation)
    }
    /// Internal admission metadata only. The generation invalidates accepted plans
    /// on rotation; neither this result nor publication history includes the key.
    pub async fn platform_credential_generation(&self,roster_id:&str,entry:&Entry)->Result<String> {
        use sqlx::Row;
        if !webagent_access::valid_id(roster_id) || !webagent_access::valid_id(&entry.id) {bail!("Invalid credential identity");}
        let row=sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(format!("platform_credential:v1:{roster_id}:{}",entry.id)).fetch_optional(self.app_pool).await?
            .ok_or_else(||anyhow::anyhow!("Platform credential unavailable"))?;
        let credential:Credential=serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
        if credential.transport!=entry.transport || credential.api_key.is_none() {bail!("Platform credential unavailable");}
        Ok(credential.generation)
    }
    /// Dispatch-only resolution: an older generation must never pick up a rotated
    /// secret. Return the provider's internal token shape, not a browser response.
    pub async fn platform_tokens_pinned(&self,roster_id:&str,entry:&Entry,generation:&str)->Result<serde_json::Value> {
        use sqlx::Row;
        if !webagent_access::valid_id(roster_id) || !webagent_access::valid_id(&entry.id) {bail!("Invalid credential identity");}
        let row=sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(format!("platform_credential:v1:{roster_id}:{}",entry.id)).fetch_optional(self.app_pool).await?
            .ok_or_else(||anyhow::anyhow!("Platform credential unavailable"))?;
        let credential:Credential=serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
        if credential.generation!=generation || credential.transport!=entry.transport {bail!("Pinned credential revoked or replaced");}
        let key=credential.api_key.ok_or_else(||anyhow::anyhow!("Platform credential unavailable"))?;
        Ok(serde_json::json!({"api_key":key}))
    }
    /// Safe status only. A metadata transport edit cannot reuse an incompatible key.
    pub async fn platform_credential_configured(&self, roster_id:&str, entry:&Entry)->Result<bool> {
        use sqlx::Row;
        if !webagent_access::valid_id(roster_id) || !webagent_access::valid_id(&entry.id) {bail!("Invalid credential identity");}
        let row=sqlx::query("SELECT value FROM docs WHERE key=?")
            .bind(format!("platform_credential:v1:{roster_id}:{}",entry.id)).fetch_optional(self.app_pool).await?;
        let Some(row)=row else {return Ok(false)};
        let credential:Credential=serde_json::from_value(self.unseal(&row.get::<Vec<u8>,_>("value"))?)?;
        Ok(credential.transport==entry.transport && credential.api_key.is_some())
    }
}
#[cfg(test)]
mod credential_tests {
    use super::*;

}
