//! Agent-owned capability levels narrow use without appointing administrators.
//! Compatibility projects explicit grants for existing adapters; new plugins fail closed.
use crate::{self as access,Permission,Relationship};
use crate::storage::{RecordCodec,Repository};
use anyhow::{anyhow,bail,Result};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use sqlx::Row;
use std::collections::BTreeMap;

#[derive(Clone,Debug,Eq,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityLevel {
    pub id:String,
    pub name:String,
    pub revision:u64,
    pub ability_permissions:BTreeMap<String,Permission>,
    pub tool_permissions:BTreeMap<String,Permission>,
    #[serde(default)]
    pub ability_exposure:BTreeMap<String,Exposure>,
    #[serde(default)]
    pub tool_exposure:BTreeMap<String,Exposure>,
    #[serde(default)]
    pub skill_exposure:BTreeMap<String,Exposure>,
}
impl CapabilityLevel {
    pub fn validate(&self)->Result<()> {
        if !access::valid_id(&self.id) || self.name.trim().is_empty() || self.name.len()>100
            || self.revision==0 || self.ability_permissions.len()>128 || self.tool_permissions.len()>256
            || self.ability_permissions.keys().chain(self.tool_permissions.keys())
                .any(|id|id!="*" && !access::valid_id(id)) {bail!("Invalid capability level");}
        for map in [&self.ability_exposure,&self.tool_exposure,&self.skill_exposure] {
            if map.len()>1024 || map.keys().any(|key|key.is_empty() || key.len()>256 || key.chars().any(char::is_control)) {bail!("Invalid capability exposure map");}
        }
        Ok(())
    }
    pub fn decorate_visibility(&self,visibility:&mut Value) {
        visibility["_capability_exposure"]=json!({"visibility":self.ability_exposure,"tool_modes":self.tool_exposure,"skill_visibility":self.skill_exposure});
    }
    pub fn ability_enabled(&self,id:&str)->bool {exposure(&self.ability_exposure,id)!=Exposure::Off}
    pub fn tool_enabled(&self,id:&str)->bool {exposure(&self.tool_exposure,id)!=Exposure::Off}
    pub fn ability_permission(&self,id:&str)->Permission {
        self.ability_permissions.get(id).or_else(||self.ability_permissions.get("*"))
            .copied().unwrap_or(Permission::Deny)
    }
    pub fn tool_permission(&self,id:&str)->Permission {
        self.tool_permissions.get(id).or_else(||self.tool_permissions.get("*"))
            .copied().unwrap_or(Permission::Auto)
    }
}
#[derive(Clone,Debug,Eq,PartialEq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityLevels {
    pub revision:u64,
    pub levels:BTreeMap<String,CapabilityLevel>,
    #[serde(default)]
    pub order:Vec<String>,
}

/// Exposure narrows independently of execution permission.
#[derive(Clone,Copy,Debug,Eq,PartialEq,Ord,PartialOrd,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Exposure { Off, Discoverable, Visible }
impl Exposure {
    pub fn meet(self,other:Self)->Self {self.min(other)}
}
fn exposure(map:&BTreeMap<String,Exposure>,id:&str)->Exposure {
    map.get(id).or_else(||map.get("*")).copied().unwrap_or(Exposure::Visible)
}
pub(crate) fn meet_permissions(target:&mut BTreeMap<String,Permission>,upper:&BTreeMap<String,Permission>,default:Permission) {
    let keys:std::collections::BTreeSet<_>=target.keys().chain(upper.keys()).cloned()
        .chain(std::iter::once("*".into())).collect();
    let previous=target.clone();
    for key in keys {
        let left=previous.get(&key).or_else(||previous.get("*")).copied().unwrap_or(default);
        let right=upper.get(&key).or_else(||upper.get("*")).copied().unwrap_or(default);
        target.insert(key,left.meet(right));
    }
}
fn meet_exposure(target:&mut BTreeMap<String,Exposure>,upper:&BTreeMap<String,Exposure>) {
    let keys:std::collections::BTreeSet<_>=target.keys().chain(upper.keys()).cloned()
        .chain(std::iter::once("*".into())).collect();
    let previous=target.clone();
    for key in keys {target.insert(key.clone(),exposure(&previous,&key).meet(exposure(upper,&key)));}
}
impl Default for CapabilityLevels {
    fn default()->Self {
        // Unsaved levels (revision 0) fail closed: they grant no abilities, so the
        // agent has no tools until an Operator saves explicit levels.
        Self {revision:0,order:vec!["operator".into(),"member".into(),"visitor".into()],levels:[("visitor","Visitor"),("member","Member"),("operator","Operator")]
            .into_iter().map(|(id,name)|(id.into(),CapabilityLevel {id:id.into(),name:name.into(),revision:1,
                ability_permissions:BTreeMap::new(),tool_permissions:BTreeMap::new(),
                ability_exposure:BTreeMap::new(),tool_exposure:BTreeMap::new(),skill_exposure:BTreeMap::new()})).collect()}
    }
}
/// Shown in chat and the effective-abilities response for agents without saved levels.
pub const NO_TOOLS_NOTICE:&str="No tools are available: this agent's capability levels aren't configured.";
impl CapabilityLevels {
    /// Levels were never saved for this agent, so they grant nothing.
    pub fn unset(&self)->bool {self.revision==0}
    pub fn tools_notice(&self)->Option<&'static str> {self.unset().then_some(NO_TOOLS_NOTICE)}
    pub fn from_metadata(metadata:&Value)->Result<Self> {
        let mut value=match metadata.get("capability_levels") {
            Some(v)=>serde_json::from_value::<Self>(v.clone())?,None=>Self::default(),
        };
        // One-time legacy projection; persisted writes use Operator and explicit order.
        if let Some(mut level)=value.levels.remove("administrator") {
            if value.levels.contains_key("operator") {bail!("Conflicting operator levels");}
            level.id="operator".into();level.name="Operator".into();
            value.levels.insert("operator".into(),level);
        }
        if value.order.is_empty() {
            value.order.push("operator".into());
            value.order.extend(value.levels.keys().filter(|id| !["operator","member","visitor"].contains(&id.as_str())).cloned());
            value.order.extend(["member".into(),"visitor".into()]);
        }
        // Fold the retired base policy into the Operator ceiling without expanding access.
        let operator=value.levels.get_mut("operator").ok_or_else(||anyhow!("Operator level missing"))?;
        for (field,ability) in [("ability_permissions",true),("tool_permissions",false)] {
            if let Some(base)=metadata.get(field).filter(|v|!v.is_null()) {
                let mut map:BTreeMap<String,Permission>=serde_json::from_value(base.clone())?;
                map.entry("*".into()).or_insert(Permission::Auto);
                meet_permissions(if ability {&mut operator.ability_permissions}else{&mut operator.tool_permissions},&map,if ability{Permission::Deny}else{Permission::Auto});
            }
        }
        value.validate()?;
        for (id,level) in &value.levels {level.validate()?;if id!=&level.id {bail!("Capability level identity mismatch");}}
        if ["visitor","member","operator"].iter().any(|id|!value.levels.contains_key(*id)) {
            bail!("Built-in capability levels must remain present");
        }
        Ok(value)
    }
    pub fn validate(&self)->Result<()> {
        if self.levels.contains_key("administrator") || self.levels.contains_key("base") || self.levels.len()>64 || self.order.len()!=self.levels.len()
            || self.order.first().map(String::as_str)!=Some("operator")
            || self.order.last().map(String::as_str)!=Some("visitor")
            || !self.levels.contains_key("member") {bail!("Invalid capability ladder");}
        let unique:std::collections::BTreeSet<_>=self.order.iter().collect();
        if unique.len()!=self.order.len() || self.order.iter().any(|id|!self.levels.contains_key(id)) {
            bail!("Capability order must contain each level exactly once");
        }
        for (id,level) in &self.levels {level.validate()?;if id!=&level.id {bail!("Capability level identity mismatch");}}
        Ok(())
    }
    pub fn effective(&self,id:&str)->Result<CapabilityLevel> {
        self.validate()?;
        let position=self.order.iter().position(|item|item==id).ok_or_else(||anyhow!("Capability level not found"))?;
        let mut result=self.levels[id].clone();
        for upper_id in &self.order[..position] {
            let upper=&self.levels[upper_id];
            meet_permissions(&mut result.ability_permissions,&upper.ability_permissions,Permission::Deny);
            meet_permissions(&mut result.tool_permissions,&upper.tool_permissions,Permission::Auto);
            meet_exposure(&mut result.ability_exposure,&upper.ability_exposure);
            meet_exposure(&mut result.tool_exposure,&upper.tool_exposure);
            meet_exposure(&mut result.skill_exposure,&upper.skill_exposure);
        }
        Ok(result)
    }
}
pub fn membership_level(membership:Option<&Value>,relationship:Relationship)->Result<String> {
    // A named level never appoints an Operator; the membership authority does.
    if relationship==Relationship::Owner || membership.is_some_and(|v|v["agent_administrator"]==true && relationship==Relationship::Member) {
        return Ok("operator".into());
    }
    if let Some(id)=membership.and_then(|v|v.get("capability_level_id")) {
        let id=id.as_str().ok_or_else(||anyhow!("Invalid membership level"))?;
        if !access::valid_id(id) || id=="operator" || id=="administrator" {bail!("Invalid membership level");}return Ok(id.into());
    }
    Ok(match relationship {
        Relationship::Owner=>"operator",Relationship::Member=>"member",
        Relationship::Caller|Relationship::Revoked=>"visitor",
    }.into())
}
impl<C: RecordCodec> Repository<'_, C> {
    pub async fn capability_levels(&self,agent:&str)->Result<CapabilityLevels> {
        let view=self.for_agent(agent).await?;
        let blob:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent).fetch_one(view.agent_pool).await?;
        CapabilityLevels::from_metadata(&self.unseal_agent(&blob)?)
    }
    pub async fn save_capability_level(&self,agent:&str,mut level:CapabilityLevel,expected:u64)->Result<CapabilityLevels> {
        level.validate()?;
        let view=self.for_agent(agent).await?;let mut tx=view.agent_pool.begin().await?;
        // Take the writer lock before reading either the catalog or record revision.
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(agent).execute(&mut *tx).await?;
        let row=sqlx::query("SELECT metadata,revision FROM agents WHERE id=?").bind(agent).fetch_one(&mut *tx).await?;
        let mut metadata=self.unseal_agent(&row.get::<Vec<u8>,_>("metadata"))?;
        let mut levels=CapabilityLevels::from_metadata(&metadata)?;
        if levels.revision!=expected {bail!("Capability levels revision changed");}
        if let Some(old)=levels.levels.get(&level.id) {
            if level.revision!=old.revision {bail!("Capability level revision changed");}
            level.revision=old.revision.checked_add(1).ok_or_else(||anyhow!("Level revision overflow"))?;
        } else if level.revision!=1 || levels.levels.len()>=64 {bail!("Invalid new capability level");}
        levels.revision=expected.checked_add(1).ok_or_else(||anyhow!("Level revision overflow"))?;
        if !levels.levels.contains_key(&level.id) {
            let before=levels.order.len()-1;levels.order.insert(before,level.id.clone());
        }
        levels.levels.insert(level.id.clone(),level.clone());levels.validate()?;metadata.as_object_mut().ok_or_else(||anyhow!("Invalid agent metadata"))?.remove("ability_permissions");metadata.as_object_mut().unwrap().remove("tool_permissions");metadata["capability_levels"]=serde_json::to_value(&levels)?;
        sqlx::query("UPDATE agents SET metadata=?,revision=revision+1,updated_at=strftime('%s','now') WHERE id=?")
            .bind(self.seal_agent(&metadata)?).bind(agent).execute(&mut *tx).await?;
        // The agent-owned audit receipt commits with its policy, not in another tenant's DB.
        let receipt=json!({"agent_id":agent,"actor_id":self.current_user_id(),"level_id":level.id,"revision":levels.revision});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!("agent:{agent}:capability_audit:{}",levels.revision)).bind(self.seal_agent(&receipt)?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(levels)
    }
    pub async fn assign_capability_level(&self,actor:&str,user:&str,agent:&str,level:&str,expected:u64)->Result<Value> {
        validate_level_id(level)?;
        if level=="operator" || level=="administrator" {bail!("Use explicit Operator appointment");}
        let owner=self.agent_owner(agent).await?;
        let mut tx=self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?").bind(agent).execute(&mut *tx).await?;
        let metadata:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(agent).fetch_one(&mut *tx).await?;
        let levels=CapabilityLevels::from_metadata(&self.unseal_agent(&metadata)?)?;
        if !levels.levels.contains_key(level) {bail!("Capability level not found");}
        let account=self.account_by_id(actor).await?.ok_or_else(||anyhow!("Actor not found"))?;
        let actor_blob:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
            .bind(actor).bind(agent).fetch_optional(&mut *tx).await?;
        let delegated=actor_blob.as_ref().map(|b|self.unseal_agent(b)).transpose()?.is_some_and(|v|v["relationship"]=="member" && v["agent_administrator"]==true);
        if !access::Principal::from_account(&account)?.active() || !(actor==owner || delegated) || owner==user {bail!("Agent Operator required; owner is protected");}
        let blob:Vec<u8>=sqlx::query_scalar("SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
            .bind(user).bind(agent).fetch_one(&mut *tx).await?;
        let mut membership=self.unseal_agent(&blob)?;
        if membership["revision"]!=expected || membership["relationship"]=="revoked" || membership["agent_administrator"]==true {bail!("Membership changed; remove Operator appointment explicitly");}
        membership["revision"]=json!(expected.checked_add(1).ok_or_else(||anyhow!("Membership revision overflow"))?);
        membership["capability_level_id"]=json!(level);
        sqlx::query("UPDATE agent_memberships SET value=? WHERE user_id=? AND agent_id=?")
            .bind(self.seal_agent(&membership)?).bind(user).bind(agent).execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET revision=revision+1 WHERE id=?").bind(agent).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)").bind(format!("agent:{agent}:membership_audit:{}",uuid::Uuid::new_v4()))
            .bind(self.seal_agent(&json!({"actor_id":actor,"user_id":user,"level_id":level,"revision":membership["revision"]}))?).execute(&mut *tx).await?;
        tx.commit().await?;Ok(membership)
    }
}

pub fn validate_level_id(level:&str)->Result<()> {
    if !crate::valid_id(level) {bail!("Invalid capability level");} Ok(())
}
