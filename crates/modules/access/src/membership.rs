//! Agent-owned joining policy; app login proves identity, never Operator authority.
use crate::{
    storage::{RecordCodec, Repository},
    Principal, Relationship, Role,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemberAccess {
    pub visitor_access: bool,
    pub app_login_members: bool,
    pub conversation_privacy: ConversationPrivacy,
    /// The owner's own limit on people active on this agent in the last 30 days, below
    /// their tier's (`OwnerLimits::agent_users`); absent: the tier's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_users: Option<u32>,
    /// The owner's own limit on people with a run going at once; absent: the tier's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_users_at_once: Option<u32>,
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConversationPrivacy {
    #[default]
    Private,
    OperatorReviewable,
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn member_access(&self, agent: &str) -> Result<MemberAccess> {
        self.for_agent(agent).await?;
        let bytes: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(self.agent_pool)
            .await?;
        MemberAccess::from_metadata(&self.unseal_agent(&bytes)?)
    }
    pub async fn resolved_membership(&self, user: &str, agent: &str) -> Result<Option<Value>> {
        let owner = self.agent_owner(agent).await?;
        let raw = self.agent_membership(user, agent).await?;
        if user != owner
            && !raw.as_ref().is_some_and(|value| {
                !["removed", "app_login", "visitor"]
                    .contains(&value["origin"].as_str().unwrap_or("explicit"))
            })
        {
            let account = self
                .account_by_id(user)
                .await?
                .ok_or_else(|| anyhow!("Account not found"))?;
            let principal = Principal::from_account(&account)?;
            let policy = self.member_access(agent).await?;
            return Ok(membership_from(user, agent, &owner, raw, || {
                Ok((principal, policy))
            })?);
        }
        membership_from(user, agent, &owner, raw, || {
            unreachable!("owner and explicit members need no account read")
        })
    }
}

impl MemberAccess {
    /// Saved limits are whole numbers of at least 1 within the tier fields' bounds.
    pub fn validate(&self) -> Result<()> {
        if self.max_users.is_some_and(|n| n == 0 || n > 1_000_000)
            || self.max_users_at_once.is_some_and(|n| n == 0 || n > 200)
        {
            return Err(anyhow!("Invalid agent user limits"));
        }
        Ok(())
    }
    /// The agent's joining policy from its metadata record.
    pub fn from_metadata(metadata: &Value) -> Result<Self> {
        Ok(if metadata["member_access"].is_null() {
            MemberAccess::default()
        } else {
            serde_json::from_value(metadata["member_access"].clone())?
        })
    }
}

/// `resolved_membership` from records already read: `raw` is the stored membership and
/// `fallback` supplies the account and joining policy, asked for only when neither the
/// owner nor an explicit membership decides.
pub(crate) fn membership_from(
    user: &str,
    agent: &str,
    owner: &str,
    raw: Option<Value>,
    fallback: impl FnOnce() -> Result<(Principal, MemberAccess)>,
) -> Result<Option<Value>> {
    {
        if user == owner {
            return Ok(Some(
                json!({"user_id":user,"agent_id":agent,"relationship":"owner","capability_level_id":"operator","revision":raw.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(1),"origin":"owner"}),
            ));
        }
        if let Some(value) = &raw {
            if !["removed", "app_login", "visitor"]
                .contains(&value["origin"].as_str().unwrap_or("explicit"))
            {
                return Ok(raw);
            }
        }
        let (principal, policy) = fallback()?;
        let level = if principal.role != Role::Guest && policy.app_login_members {
            "member"
        } else if policy.visitor_access {
            "visitor"
        } else {
            return Ok(None);
        };
        Ok(Some(
            json!({"user_id":user,"agent_id":agent,"relationship":if level=="member" {Relationship::Member} else {Relationship::Caller},
            "capability_level_id":level,"revision":raw.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(1),"origin":if level=="member" {"app_login"} else {"visitor"}}),
        ))
    }
}
