//! Application-wide records for who people are, what their tier permits, and
//! which agents they may use. Conversations themselves belong to user storage.
use crate::storage::{RecordCodec, Repository};
use crate::{self as access, AccountStatus, Principal, Relationship, Role, Tier};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::Row;

fn account_view(value: &Value) -> Result<Value> {
    let principal = Principal::from_account(value)?;
    Ok(
        json!({"user_id":principal.user_id,"username":value["username"],
        "status":principal.status,"role":principal.role,
        "tier_id":principal.tier_id,"access_revision":principal.access_revision}),
    )
}
async fn audit(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    store: &Repository<'_, impl RecordCodec>,
    kind: &str,
    payload: &Value,
) -> Result<()> {
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('',?,?)")
        .bind(kind)
        .bind(store.seal(payload)?)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn access_audit(&self, limit: u32) -> Result<Vec<Value>> {
        if limit == 0 || limit > 200 {
            bail!("Invalid audit limit");
        }
        let rows = sqlx::query(
            "SELECT seq,kind,payload FROM events
            WHERE session_id='' AND kind IN ('account_created',
                'account_access_changed','tier_changed','agent_membership_changed',
                'profile_updated','password_changed',
                'agent_status_changed','agent_tool_permissions_changed',
                'agent_tool_callers_changed','agent_ability_permissions_changed')
            ORDER BY seq DESC LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(self.app_pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(
                    json!({"seq":row.get::<i64,_>("seq"),"kind":row.get::<String,_>("kind"),
                "detail":self.unseal(&row.get::<Vec<u8>,_>("payload"))?}),
                )
            })
            .collect()
    }
    pub async fn update_account_access(
        &self,
        id: &str,
        expected_revision: u64,
        status: AccountStatus,
        role: Role,
        tier_id: &str,
    ) -> Result<Value> {
        if !access::valid_id(id) || !access::valid_id(tier_id) || expected_revision == 0 {
            bail!("Invalid account update");
        }
        if id == "admin"
            && (status != AccountStatus::Active
                || role != Role::Administrator
                || tier_id != "operator")
        {
            bail!("The original administrator must remain active");
        }
        let mut tx = self.app_pool.begin().await?;
        self.accounts().lock_account_in(&mut tx, id).await?;
        let account = self
            .accounts()
            .account_state_in(&mut tx, id)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        if (account["role"] == "guest") != (role == Role::Guest) {
            bail!("Guest identity cannot be converted through access administration");
        }
        if Principal::from_account(&account)?.access_revision != expected_revision {
            bail!("Account revision changed");
        }
        if self.entitlement_tier(tier_id).await?.is_none() {
            bail!("Entitlement tier not found");
        }
        let role_value = serde_json::to_value(role)?;
        let account = self
            .accounts()
            .apply_access_in(
                &mut tx,
                id,
                webagent_accounts::AccessMutation {
                    expected_revision,
                    status,
                    role: role_value.as_str().ok_or_else(|| anyhow!("Invalid role"))?,
                    tier_id,
                    profile_is_admin: status == AccountStatus::Active
                        && role == Role::Administrator,
                },
            )
            .await?;
        audit(
            &mut tx,
            self,
            "account_access_changed",
            &json!({"user_id":id,
            "status":status,"role":role,"tier_id":tier_id,
            "access_revision":expected_revision+1}),
        )
        .await?;
        tx.commit().await?;
        account_view(&account)
    }
    pub async fn entitlement_tier(&self, id: &str) -> Result<Option<Tier>> {
        if !access::valid_id(id) {
            bail!("Invalid tier ID");
        }
        let policy = self.read_access_policy()?;
        if policy["tiers"].get(id).is_none() {
            return Ok(None);
        }
        Ok(Some(crate::account_ladder::effective(&policy, id)?))
    }
    pub async fn entitlement_tiers(&self) -> Result<Vec<Tier>> {
        let policy = self.read_access_policy()?;
        let tiers = policy["tiers"]
            .as_object()
            .ok_or_else(|| anyhow!("Policy tiers missing"))?;
        tiers
            .values()
            .map(|value| {
                let tier: Tier = serde_json::from_value(value.clone())?;
                tier.validate()?;
                Ok(tier)
            })
            .collect()
    }
    pub async fn save_entitlement_tier(
        &self,
        mut tier: Tier,
        expected_revision: u64,
    ) -> Result<Tier> {
        tier.revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| anyhow!("Tier revision overflow"))?;
        tier.validate()?;
        {
            let _lock = self.policy_lock()?;
            let mut policy = self.read_access_policy()?;
            if policy["tiers"][&tier.id]["revision"].as_u64().unwrap_or(0) != expected_revision {
                bail!("Tier revision changed");
            }
            let mut order = crate::account_ladder::order(&policy)?;
            if !order.contains(&tier.id) {
                let position = order
                    .iter()
                    .position(|id| id == "basic")
                    .or_else(|| order.iter().position(|id| id == "anonymous"))
                    .unwrap_or(order.len());
                order.insert(position, tier.id.clone());
            }
            policy["tiers"][&tier.id] = serde_json::to_value(&tier)?;
            policy["tier_order"] = json!(order);
            policy["revision"] = json!(policy["revision"]
                .as_u64()
                .unwrap_or(0)
                .checked_add(1)
                .ok_or_else(|| anyhow!("Policy revision overflow"))?);
            self.write_access_policy(&policy)?;
        }
        // File authority is published first; runtime tier revisions invalidate old grants.
        let mut tx = self.app_pool.begin().await?;
        for (id, account) in self.accounts().all_account_states_in(&mut tx).await? {
            if account["tier_id"] != tier.id {
                continue;
            }
            let revision = Principal::from_account(&account)?.access_revision;
            self.accounts()
                .bump_access_version_in(&mut tx, &id, revision)
                .await?;
        }
        audit(
            &mut tx,
            self,
            "tier_changed",
            &json!({"tier_id":tier.id,"revision":tier.revision}),
        )
        .await?;
        tx.commit().await?;
        Ok(tier)
    }
    pub async fn agent_membership(&self, user: &str, agent: &str) -> Result<Option<Value>> {
        if !access::valid_id(user) {
            bail!("Invalid membership identity");
        }
        self.for_agent(agent).await?;
        let row: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?",
        )
        .bind(user)
        .bind(agent)
        .fetch_optional(self.agent_pool)
        .await?;
        row.map(|blob| self.unseal_agent(&blob)).transpose()
    }
    pub async fn agent_owner(&self, agent: &str) -> Result<String> {
        self.for_agent(agent).await?;
        let owner: String = sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
            .bind(agent)
            .fetch_one(self.app_pool)
            .await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
            .bind(agent)
            .fetch_one(self.agent_pool)
            .await?;
        let metadata = self.unseal_agent(&blob)?;
        if metadata["owner_id"].as_str() != Some(owner.as_str())
            && !(agent == "shared_default" && owner == "admin" && metadata["owner_id"].is_null())
        {
            bail!("Agent owner conflict");
        }
        Ok(owner)
    }
    pub async fn agent_authority(&self, user: &str, agent: &str) -> Result<Value> {
        if !access::valid_id(user) {
            bail!("Invalid membership identity");
        }
        let owner = self.agent_owner(agent).await?;
        let account = self
            .account_by_id(user)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let principal = Principal::from_account(&account)?;
        let membership = self.agent_membership(user, agent).await?;
        if membership.as_ref().is_some_and(|v| {
            v["user_id"] != user
                || v["agent_id"] != agent
                || v["revision"].as_u64().is_none_or(|r| r == 0)
        }) {
            bail!("Invalid membership record");
        }
        let relationship: Relationship = membership
            .as_ref()
            .map(|v| serde_json::from_value(v["relationship"].clone()))
            .transpose()?
            .unwrap_or(Relationship::Revoked);
        let operator = owner == user
            || membership
                .as_ref()
                .is_some_and(|v| v["agent_administrator"] == true && v["relationship"] == "member");
        Ok(
            json!({"agent_id":agent,"owner_id":owner,"relationship":relationship,
            "revision":membership.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(0),
            "agent_operator":operator,"agent_administrator":operator,
            "installation_administrator":principal.administrator(),
            "can_manage":principal.active() && operator,
            "can_use":principal.active() && relationship!=Relationship::Revoked}),
        )
    }
    pub async fn agent_memberships(&self, agent: &str) -> Result<Vec<Value>> {
        self.for_agent(agent).await?;
        let rows =
            sqlx::query("SELECT value FROM agent_memberships WHERE agent_id=? ORDER BY user_id")
                .bind(agent)
                .fetch_all(self.agent_pool)
                .await?;
        rows.iter()
            .map(|row| {
                let mut value = self.unseal_agent(&row.get::<Vec<u8>, _>("value"))?;
                value["agent_operator"] =
                    json!(value["agent_administrator"] == true || value["relationship"] == "owner");
                value["capability_level_id"] = json!(crate::capabilities::membership_level(
                    Some(&value),
                    serde_json::from_value(value["relationship"].clone())?
                )?);
                Ok(value)
            })
            .collect()
    }
    pub async fn assign_agent_membership(
        &self,
        actor: &str,
        user: &str,
        agent: &str,
        relationship: Relationship,
        operator: bool,
        expected: u64,
    ) -> Result<Value> {
        validate_membership_actor(actor)?;
        self.write_agent_membership(
            user,
            agent,
            relationship,
            expected,
            Some((actor, operator)),
            None,
            "explicit",
        )
        .await
    }
    pub async fn set_agent_membership(
        &self,
        user: &str,
        agent: &str,
        relationship: Relationship,
        expected: u64,
    ) -> Result<Value> {
        self.write_agent_membership(user, agent, relationship, expected, None, None, "explicit")
            .await
    }
    pub async fn manage_agent_member(
        &self,
        actor: &str,
        user: &str,
        agent: &str,
        level: Option<&str>,
        blocked: bool,
        expected: u64,
    ) -> Result<Value> {
        if blocked && level.is_some() {
            bail!("Blocked membership cannot have a level");
        }
        let operator = level == Some("operator");
        self.write_agent_membership(
            user,
            agent,
            if blocked {
                Relationship::Revoked
            } else if level.is_some_and(|v| v != "visitor") {
                Relationship::Member
            } else {
                Relationship::Caller
            },
            expected,
            Some((actor, operator)),
            level,
            if blocked {
                "blocked"
            } else if level.is_none() {
                "removed"
            } else {
                "explicit"
            },
        )
        .await
    }
    async fn write_agent_membership(
        &self,
        user: &str,
        agent: &str,
        relationship: Relationship,
        expected: u64,
        actor: Option<(&str, bool)>,
        level: Option<&str>,
        origin: &str,
    ) -> Result<Value> {
        validate_membership_update(user, agent, relationship)?;
        let owner = self.agent_owner(agent).await?;
        if (owner == user && relationship != Relationship::Owner)
            || (owner != user && relationship == Relationship::Owner)
        {
            bail!("Agent owner relationship cannot be changed");
        }
        let target = self
            .account_by_id(user)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        if actor.is_some_and(|(_, operator)| operator) && target["role"] == "guest" {
            bail!("Anonymous identities cannot be Operators");
        }
        let mut tx = self.agent_pool.begin().await?;
        sqlx::query("UPDATE agents SET revision=revision WHERE id=?")
            .bind(agent)
            .execute(&mut *tx)
            .await?;
        let blob: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?",
        )
        .bind(user)
        .bind(agent)
        .fetch_optional(&mut *tx)
        .await?;
        let previous = blob.as_ref().map(|b| self.unseal_agent(b)).transpose()?;
        if previous
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0)
            != expected
        {
            bail!("Membership revision changed");
        }
        if let Some(level) = level {
            let bytes: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
                .bind(agent)
                .fetch_one(&mut *tx)
                .await?;
            if !crate::capabilities::CapabilityLevels::from_metadata(&self.unseal_agent(&bytes)?)?
                .levels
                .contains_key(level)
            {
                bail!("Capability level not found");
            }
        }
        let operator = if let Some((actor, operator)) = actor {
            let account = self
                .account_by_id(actor)
                .await?
                .ok_or_else(|| anyhow!("Actor not found"))?;
            let member: Option<Vec<u8>> = sqlx::query_scalar(
                "SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?",
            )
            .bind(actor)
            .bind(agent)
            .fetch_optional(&mut *tx)
            .await?;
            let delegated = member
                .as_ref()
                .map(|b| self.unseal_agent(b))
                .transpose()?
                .is_some_and(|v| v["relationship"] == "member" && v["agent_administrator"] == true);
            if !Principal::from_account(&account)?.active() || !(actor == owner || delegated) {
                bail!("Agent Operator required");
            }
            if user == owner {
                bail!("Owner appointment is protected");
            }
            if operator && relationship != Relationship::Member {
                bail!("Operator must be a member");
            }
            operator
        } else {
            previous
                .as_ref()
                .is_some_and(|v| v["agent_administrator"] == true)
                && relationship == Relationship::Member
        };
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Membership revision overflow"))?;
        let mut value = json!({"user_id":user,"agent_id":agent,"relationship":relationship,
            "agent_administrator":operator,"revision":revision,"origin":origin});
        value["capability_level_id"] = if operator || relationship == Relationship::Owner {
            json!("operator")
        } else if let Some(level) = level {
            json!(level)
        } else {
            previous
                .as_ref()
                .and_then(|v| v.get("capability_level_id"))
                .filter(|v| *v != "operator" && *v != "administrator")
                .cloned()
                .unwrap_or_else(|| {
                    json!(if relationship == Relationship::Member {
                        "member"
                    } else {
                        "visitor"
                    })
                })
        };
        sqlx::query("INSERT INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?) ON CONFLICT(user_id,agent_id) DO UPDATE SET value=excluded.value")
            .bind(user).bind(agent).bind(self.seal_agent(&value)?).execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET revision=revision+1 WHERE id=?")
            .bind(agent)
            .execute(&mut *tx)
            .await?;
        let receipt = json!({"agent_id":agent,"user_id":user,"actor_id":actor.map(|v|v.0),"revision":revision,"relationship":relationship,"operator":operator});
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(format!(
                "agent:{agent}:membership_audit:{}",
                uuid::Uuid::new_v4()
            ))
            .bind(self.seal_agent(&receipt)?)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(value)
    }
}

pub fn validate_membership_update(
    user_id: &str,
    agent_id: &str,
    relationship: Relationship,
) -> Result<()> {
    if !access::valid_id(user_id) || !access::valid_agent_id(agent_id) {
        bail!("Invalid membership identity");
    }
    if user_id == "admin" && agent_id == "shared_default" && relationship != Relationship::Owner {
        bail!("The original administrator must retain its agent");
    }
    Ok(())
}
pub fn validate_membership_actor(actor_id: &str) -> Result<()> {
    if !crate::valid_id(actor_id) {
        bail!("Invalid actor identity");
    }
    Ok(())
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn migrate_access_v1(&self) -> Result<()> {
        let mut tx = self.app_pool.begin().await?;
        let marker: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='storage:access_v1'")
                .fetch_optional(&mut *tx)
                .await?;
        if let Some(marker) = &marker {
            if self.unseal(marker)?["version"] != 1 {
                bail!("Unsupported access migration marker");
            }
        }
        // Fresh-install seeds are explicit; only Operator receives codebase_admin.
        let operator = Tier {
            id: "operator".into(),
            revision: 1,
            model_policy: None,
            ability_permissions: [
                ("codebase_admin".into(), access::Permission::Auto),
                ("web_access".into(), access::Permission::Auto),
            ]
            .into(),
            allowed_models: vec!["*".into()],
            tools: [("*".into(), access::Permission::Auto)].into(),
            entitlements: Default::default(),
            max_sessions: 10_000,
            max_active_runs: 200,
            chain: Vec::new(),
        };
        let basic = Tier {
            id: "basic".into(),
            revision: 1,
            model_policy: None,
            ability_permissions: [("web_access".into(), access::Permission::Auto)].into(),
            allowed_models: Vec::new(),
            tools: Default::default(),
            entitlements: Default::default(),
            max_sessions: 10,
            max_active_runs: 2,
            chain: Vec::new(),
        };
        for tier in [&operator, &basic] {
            tier.validate()?;
            sqlx::query(
                "INSERT OR IGNORE INTO entitlement_tiers(tier_id,revision,value) VALUES(?,?,?)",
            )
            .bind(&tier.id)
            .bind(tier.revision as i64)
            .bind(self.seal(&serde_json::to_value(tier)?)?)
            .execute(&mut *tx)
            .await?;
        }
        if self
            .accounts()
            .initialize_admin_assignment_in(
                &mut tx,
                AccountStatus::Active,
                "administrator",
                "operator",
            )
            .await?
        {
            let membership = json!({"user_id":"admin","agent_id":"shared_default",
                "relationship":Relationship::Owner,"revision":1});
            sqlx::query(
                "INSERT OR IGNORE INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)",
            )
            .bind("admin")
            .bind("shared_default")
            .bind(self.seal(&membership)?)
            .execute(&mut *tx)
            .await?;
        }
        if marker.is_none() {
            sqlx::query("INSERT INTO docs(key,value) VALUES('storage:access_v1',?)")
                .bind(self.seal(&json!({"version":1}))?)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn initialize_authorization_tables(&self) -> Result<()> {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS entitlement_tiers(tier_id TEXT PRIMARY KEY,revision INTEGER NOT NULL,value BLOB NOT NULL);
   CREATE TABLE IF NOT EXISTS agent_memberships(user_id TEXT NOT NULL REFERENCES user_accounts(user_id),agent_id TEXT NOT NULL,value BLOB NOT NULL,PRIMARY KEY(user_id,agent_id));")
   .execute(self.app_pool).await?;
        Ok(())
    }
    /// The host commits only after Accounts has inserted the original administrator.
    pub async fn bootstrap_admin_membership_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    ) -> Result<()> {
        let membership = json!({"user_id":"admin","agent_id":"shared_default","relationship":Relationship::Owner,"revision":1});
        sqlx::query("INSERT INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)")
            .bind("admin")
            .bind("shared_default")
            .bind(self.seal(&membership)?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
