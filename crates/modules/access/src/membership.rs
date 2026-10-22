//! Agent-owned joining policy; app login proves identity, never Operator authority.
use crate::{storage::{RecordCodec,Repository},Principal,Role,Relationship};
use anyhow::{anyhow,Result};
use serde::{Serialize,Deserialize};
use serde_json::{json,Value};

#[derive(Clone,Debug,Default,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct MemberAccess {
    pub visitor_access:bool,
    pub app_login_members:bool,
    pub conversation_privacy:ConversationPrivacy,
}
#[derive(Clone,Copy,Debug,Default,Eq,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ConversationPrivacy { #[default] Private, OperatorReviewable }

impl<C:RecordCodec> Repository<'_,C> {
    pub async fn member_access(&self,agent:&str)->Result<MemberAccess> {
        self.for_agent(agent).await?;
        let bytes:Vec<u8>=sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?").bind(agent).fetch_one(self.agent_pool).await?;
        let metadata=self.unseal_agent(&bytes)?;
        Ok(if metadata["member_access"].is_null() {MemberAccess::default()} else {serde_json::from_value(metadata["member_access"].clone())?})
    }
    pub async fn resolved_membership(&self,user:&str,agent:&str)->Result<Option<Value>> {
        let owner=self.agent_owner(agent).await?;
        let raw=self.agent_membership(user,agent).await?;
        if user==owner {return Ok(Some(json!({"user_id":user,"agent_id":agent,"relationship":"owner","capability_level_id":"operator","revision":raw.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(1),"origin":"owner"})));}
        if let Some(value)=&raw {
            if !["removed","app_login","visitor"].contains(&value["origin"].as_str().unwrap_or("explicit")) {return Ok(raw);}
        }
        let account=self.account_by_id(user).await?.ok_or_else(||anyhow!("Account not found"))?;
        let principal=Principal::from_account(&account)?;
        let policy=self.member_access(agent).await?;
        let level=if principal.role!=Role::Guest && policy.app_login_members {"member"}
            else if policy.visitor_access {"visitor"} else {return Ok(None)};
        Ok(Some(json!({"user_id":user,"agent_id":agent,"relationship":if level=="member" {Relationship::Member} else {Relationship::Caller},
            "capability_level_id":level,"revision":raw.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(1),"origin":if level=="member" {"app_login"} else {"visitor"}})))
    }
}
