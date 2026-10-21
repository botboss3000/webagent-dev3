//! Application-wide records for who people are, what their tier permits, and
//! which agents they may use. Conversations themselves belong to user storage.
use crate::{self as access, AccountStatus, Principal, Relationship, Role, Tier};
use crate::storage::{RecordCodec,Repository};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use sqlx::Row;

fn account_view(value: &Value) -> Result<Value> {
    let principal = Principal::from_account(value)?;
    Ok(json!({"user_id":principal.user_id,"username":value["username"],
        "status":principal.status,"role":principal.role,
        "tier_id":principal.tier_id,"access_revision":principal.access_revision}))
}
async fn audit(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>, store: &Repository<'_, impl RecordCodec>,
    kind: &str, payload: &Value) -> Result<()> {
    sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES('',?,?)")
        .bind(kind).bind(store.seal(payload)?).execute(&mut **tx).await?;
    Ok(())
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn access_audit(&self, limit: u32) -> Result<Vec<Value>> {
        if limit == 0 || limit > 200 { bail!("Invalid audit limit"); }
        let rows = sqlx::query("SELECT seq,kind,payload FROM events
            WHERE session_id='' AND kind IN ('account_created',
                'account_access_changed','tier_changed','agent_membership_changed',
                'profile_updated','password_changed',
                'agent_status_changed','agent_tool_permissions_changed',
                'agent_tool_callers_changed','agent_ability_permissions_changed')
            ORDER BY seq DESC LIMIT ?")
            .bind(limit as i64).fetch_all(self.app_pool).await?;
        rows.iter().map(|row| {
            Ok(json!({"seq":row.get::<i64,_>("seq"),"kind":row.get::<String,_>("kind"),
                "detail":self.unseal(&row.get::<Vec<u8>,_>("payload"))?}))
        }).collect()
    }
    pub async fn update_account_access(&self, id: &str, expected_revision: u64,
        status: AccountStatus, role: Role, tier_id: &str) -> Result<Value> {
        if !access::valid_id(id) || !access::valid_id(tier_id) || expected_revision == 0 {
            bail!("Invalid account update");
        }
        if id == "admin" && (status != AccountStatus::Active
            || role != Role::Administrator || tier_id != "operator") {
            bail!("The original administrator must remain active");
        }
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE user_accounts SET value=value WHERE user_id=?")
            .bind(id).execute(&mut *tx).await?;
        let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
            .bind(id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let mut account = self.unseal(&blob)?;
        if (account["role"] == "guest") != (role == Role::Guest) {
            bail!("Guest identity cannot be converted through access administration");
        }
        if Principal::from_account(&account)?.access_revision != expected_revision {
            bail!("Account revision changed");
        }
        if self.entitlement_tier(tier_id).await?.is_none() {bail!("Entitlement tier not found");}
        account["status"] = json!(status);
        account["role"] = json!(role);
        account["tier_id"] = json!(tier_id);
        account["access_revision"] = json!(expected_revision + 1);
        account["generation"] = json!(uuid::Uuid::new_v4().to_string());
        account["is_approved"] = json!(status == AccountStatus::Active);
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.seal(&account)?).bind(id).execute(&mut *tx).await?;
        let profile_blob: Vec<u8> = sqlx::query_scalar(
            "SELECT value FROM user_profiles WHERE user_id=?")
            .bind(id).fetch_one(&mut *tx).await?;
        let mut profile = self.unseal(&profile_blob)?;
        profile["is_admin"] = json!(status == AccountStatus::Active
            && role == Role::Administrator);
        sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
            .bind(self.seal(&profile)?).bind(id).execute(&mut *tx).await?;
        audit(&mut tx,self,"account_access_changed",&json!({"user_id":id,
            "status":status,"role":role,"tier_id":tier_id,
            "access_revision":expected_revision+1})).await?;
        tx.commit().await?;
        account_view(&account)
    }
    pub async fn entitlement_tier(&self, id: &str) -> Result<Option<Tier>> {
        if !access::valid_id(id) { bail!("Invalid tier ID"); }
        let policy=self.read_access_policy()?;
        policy["tiers"].get(id).map(|value| {
            let tier:Tier=serde_json::from_value(value.clone())?;
            tier.validate()?;
            Ok(tier)
        }).transpose()
    }
    pub async fn entitlement_tiers(&self) -> Result<Vec<Tier>> {
        let policy=self.read_access_policy()?;
        let tiers=policy["tiers"].as_object().ok_or_else(||anyhow!("Policy tiers missing"))?;
        tiers.values().map(|value| {let tier:Tier=serde_json::from_value(value.clone())?;tier.validate()?;Ok(tier)}).collect()
    }
    pub async fn save_entitlement_tier(&self, mut tier: Tier,
        expected_revision: u64) -> Result<Tier> {
        if tier.id == "operator" {
            let mut previous=self.entitlement_tier("operator").await?.ok_or_else(||anyhow!("Operator tier missing"))?;
            previous.ability_permissions=tier.ability_permissions.clone();previous.revision=tier.revision;
            if serde_json::to_value(&previous)?!=serde_json::to_value(&tier)? {
                bail!("Only operator ability grants may change");
            }
        }
        tier.revision=expected_revision.checked_add(1).ok_or_else(||anyhow!("Tier revision overflow"))?;
        tier.validate()?;
        {
            let _lock=self.policy_lock()?;
            let mut policy=self.read_access_policy()?;
            if policy["tiers"][&tier.id]["revision"].as_u64().unwrap_or(0)!=expected_revision {bail!("Tier revision changed");}
            policy["tiers"][&tier.id]=serde_json::to_value(&tier)?;
            policy["revision"]=json!(policy["revision"].as_u64().unwrap_or(0).checked_add(1).ok_or_else(||anyhow!("Policy revision overflow"))?);
            self.write_access_policy(&policy)?;
        }
        // File authority is published first; runtime tier revisions invalidate old grants.
        let mut tx=self.app_pool.begin().await?;
        let rows=sqlx::query("SELECT user_id,value FROM user_accounts").fetch_all(&mut *tx).await?;
        for row in rows {
            let mut account=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            if account["tier_id"]!=tier.id {continue;}
            let revision=Principal::from_account(&account)?.access_revision;
            account["access_revision"]=json!(revision+1);
            account["generation"]=json!(uuid::Uuid::new_v4().to_string());
            sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?").bind(self.seal(&account)?).bind(row.get::<String,_>("user_id")).execute(&mut *tx).await?;
        }
        audit(&mut tx,self,"tier_changed",&json!({"tier_id":tier.id,"revision":tier.revision})).await?;
        tx.commit().await?;
        Ok(tier)
    }
    pub async fn agent_membership(&self, user_id: &str, agent_id: &str) -> Result<Option<Value>> {
        if !access::valid_id(user_id) || !access::valid_agent_id(agent_id) {
            bail!("Invalid membership identity");
        }
        let row = sqlx::query("SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
            .bind(user_id).bind(agent_id).fetch_optional(self.app_pool).await?;
        row.map(|row| self.unseal(&row.get::<Vec<u8>,_>("value"))).transpose()
    }
    pub async fn agent_authority(&self, user_id: &str, agent_id: &str) -> Result<Value> {
        if !access::valid_id(user_id) || !access::valid_agent_id(agent_id) {
            bail!("Invalid membership identity");
        }
        let owner: String = sqlx::query_scalar("SELECT owner_id FROM agent_directory WHERE id=?")
            .bind(agent_id).fetch_optional(self.app_pool).await?
            .ok_or_else(||anyhow!("Agent not found"))?;
        let account = self.account_by_id(user_id).await?
            .ok_or_else(||anyhow!("Account not found"))?;
        let principal = Principal::from_account(&account)?;
        let membership = self.agent_membership(user_id,agent_id).await?;
        if membership.as_ref().is_some_and(|value| value["user_id"] != user_id
            || value["agent_id"] != agent_id || value["revision"].as_u64().is_none_or(|r|r == 0)) {
            bail!("Invalid membership record");
        }
        let relationship: Relationship = membership.as_ref()
            .map(|value|serde_json::from_value(value["relationship"].clone()))
            .transpose()?.unwrap_or(Relationship::Revoked);
        let administrator = membership.as_ref().is_some_and(|value|
            value["agent_administrator"] == true && value["relationship"] == "member");
        Ok(json!({"agent_id":agent_id,"owner_id":owner,"relationship":relationship,
            "revision":membership.as_ref().and_then(|v|v["revision"].as_u64()).unwrap_or(0),
            "agent_administrator":administrator,"installation_administrator":principal.administrator(),
            "can_manage":principal.active() && (owner == user_id || administrator || principal.administrator()),
            "can_use":principal.active() && relationship != Relationship::Revoked}))
    }
    pub async fn agent_memberships(&self, agent_id: &str) -> Result<Vec<Value>> {
        if !access::valid_agent_id(agent_id) { bail!("Invalid agent identity"); }
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM agent_directory WHERE id=?")
            .bind(agent_id).fetch_optional(self.app_pool).await?;
        if exists.is_none() { bail!("Agent not found"); }
        let rows = sqlx::query("SELECT user_id,value FROM agent_memberships WHERE agent_id=? ORDER BY user_id")
            .bind(agent_id).fetch_all(self.app_pool).await?;
        rows.iter().map(|row| {
            let value = self.unseal(&row.get::<Vec<u8>,_>("value"))?;
            Ok(json!({"user_id":row.get::<String,_>("user_id"),"agent_id":agent_id,
                "relationship":value["relationship"],"revision":value["revision"],
                "agent_administrator":value["agent_administrator"].as_bool().unwrap_or(false),
                "capability_level_id":crate::capabilities::membership_level(Some(&value),
                    serde_json::from_value(value["relationship"].clone())?)?}))
        }).collect()
    }
    pub async fn assign_agent_membership(&self, actor_id: &str, user_id: &str,
        agent_id: &str, relationship: Relationship, agent_administrator: bool,
        expected_revision: u64) -> Result<Value> {
        validate_membership_actor(actor_id)?;
        self.write_agent_membership(user_id,agent_id,relationship,expected_revision,
            Some((actor_id,agent_administrator))).await
    }
    pub async fn set_agent_membership(&self, user_id: &str, agent_id: &str,
        relationship: Relationship, expected_revision: u64) -> Result<Value> {
        // Trusted bootstrap/internal seam; HTTP roster writes use the actor-aware method.
        self.write_agent_membership(user_id,agent_id,relationship,expected_revision,None).await
    }
    async fn write_agent_membership(&self, user_id: &str, agent_id: &str,
        relationship: Relationship, expected_revision: u64,
        actor: Option<(&str,bool)>) -> Result<Value> {
        validate_membership_update(user_id,agent_id,relationship)?;
        let agent_view = self.for_agent(agent_id).await?;
        let agent: Option<String> = sqlx::query_scalar("SELECT status FROM agents WHERE id=?")
            .bind(agent_id).fetch_optional(agent_view.agent_pool).await?;
        if agent.is_none() { bail!("Agent not found"); }
        let owner: Option<String> = sqlx::query_scalar(
            "SELECT owner_id FROM agent_directory WHERE id=?")
            .bind(agent_id).fetch_optional(self.app_pool).await?;
        let owner = owner.ok_or_else(||anyhow!("Agent is not registered"))?;
        if (owner == user_id && relationship != Relationship::Owner)
            || (owner != user_id && relationship == Relationship::Owner) {
            bail!("Agent owner relationship cannot be changed here");
        }
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE user_accounts SET value=value WHERE user_id=?")
            .bind(user_id).execute(&mut *tx).await?;
        let account_blob: Vec<u8> = sqlx::query_scalar(
            "SELECT value FROM user_accounts WHERE user_id=?")
            .bind(user_id).fetch_optional(&mut *tx).await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let row: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
            .bind(user_id).bind(agent_id).fetch_optional(&mut *tx).await?;
        let current = row.as_ref().map(|row| self.unseal(row)).transpose()?;
        if current.as_ref().and_then(|value|value["revision"].as_u64()).unwrap_or(0)
            != expected_revision { bail!("Membership revision changed"); }
        let current_admin = current.as_ref().is_some_and(|v|v["agent_administrator"] == true);
        let administrator = if let Some((actor_id, requested_admin)) = actor {
            // This write transaction serializes authority reads with revocation.
            let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id=?")
                .bind(actor_id).fetch_optional(&mut *tx).await?
                .ok_or_else(||anyhow!("Actor account not found"))?;
            let principal = Principal::from_account(&self.unseal(&blob)?)?;
            let actor_membership: Option<Vec<u8>> = sqlx::query_scalar(
                "SELECT value FROM agent_memberships WHERE user_id=? AND agent_id=?")
                .bind(actor_id).bind(agent_id).fetch_optional(&mut *tx).await?;
            let actor_membership = actor_membership.as_ref().map(|blob|self.unseal(blob)).transpose()?;
            let delegated = actor_membership.as_ref().is_some_and(|v|
                v["agent_administrator"] == true && v["relationship"] == "member");
            if !principal.active() || !(actor_id == owner || delegated || principal.administrator()) {
                bail!("Agent administration required");
            }
            // Owners appoint administrators. Installation oversight can revoke
            // an administrator but never silently gains ownership or appoints one.
            if requested_admin && actor_id != owner && !current_admin {
                bail!("Only the agent owner can appoint administrators");
            }
            if actor_id != owner && !principal.administrator()
                && (current_admin || requested_admin || user_id == owner) {
                bail!("Agent administrators cannot change privileged memberships");
            }
            if requested_admin && relationship != Relationship::Member {
                bail!("Agent administrator must be a member");
            }
            requested_admin
        } else {
            current_admin && relationship == Relationship::Member
        };
        let revision = expected_revision.checked_add(1)
            .ok_or_else(|| anyhow!("Membership revision overflow"))?;
        let mut value = json!({"user_id":user_id,"agent_id":agent_id,
            "relationship":relationship,"agent_administrator":administrator,"revision":revision});
        // Relationship/admin edits do not silently replace an appointed capability policy.
        value["capability_level_id"]=json!(if let Some(level)=current.as_ref().and_then(|v|v.get("capability_level_id")) {
            level.as_str().ok_or_else(||anyhow!("Invalid membership level"))?.to_owned()
        } else {crate::capabilities::membership_level(Some(&value),relationship)?});
        sqlx::query("INSERT INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)
            ON CONFLICT(user_id,agent_id) DO UPDATE SET value=excluded.value")
            .bind(user_id).bind(agent_id).bind(self.seal(&value)?)
            .execute(&mut *tx).await?;
        let mut account = self.unseal(&account_blob)?;
        let old = Principal::from_account(&account)?.access_revision;
        account["access_revision"] = json!(old.checked_add(1)
            .ok_or_else(||anyhow!("Account revision overflow"))?);
        // INITIAL-OWNER-SESSION: new ownership grants no previously held access to revoke.
        // Keep the creator signed in so creation can proceed to model configuration.
        // Every later membership edit still invalidates sign-ins and access snapshots.
        let initial_owner = owner == user_id && current.is_none() && relationship == Relationship::Owner;
        if !initial_owner {
            account["generation"] = json!(uuid::Uuid::new_v4().to_string());
        }
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.seal(&account)?).bind(user_id).execute(&mut *tx).await?;
        audit(&mut tx,self,"agent_membership_changed",&json!({"user_id":user_id,
            "agent_id":agent_id,"relationship":relationship,"agent_administrator":administrator,
            "actor_id":actor.map(|(id,_)|id),"revision":revision})).await?;
        tx.commit().await?;
        Ok(value)
    }
}

pub fn validate_membership_update(user_id:&str,agent_id:&str,relationship:Relationship)->Result<()> {
        if !access::valid_id(user_id) || !access::valid_agent_id(agent_id) {
            bail!("Invalid membership identity");
        }
        if user_id == "admin" && agent_id == "shared_default"
            && relationship != Relationship::Owner {
            bail!("The original administrator must retain its agent");
        }
    Ok(())
}
pub fn validate_membership_actor(actor_id:&str)->Result<()> {
    if !crate::valid_id(actor_id) {bail!("Invalid actor identity");} Ok(())
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn migrate_access_v1(&self) -> Result<()> {
        let mut tx = self.app_pool.begin().await?;
        let marker: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM docs WHERE key='storage:access_v1'")
            .fetch_optional(&mut *tx).await?;
        if let Some(marker) = &marker {
            if self.unseal(marker)?["version"] != 1 {
                bail!("Unsupported access migration marker");
            }
        }
        let operator = Tier { id:"operator".into(), revision:1, model_policy:None,
            ability_permissions:access::legacy_ability_grants(),
            allowed_models:vec!["*".into()],
            tools:[("*".into(), access::Permission::Auto)].into(),
            max_sessions:10_000, max_active_runs:200 };
        let basic = Tier { id:"basic".into(), revision:1, model_policy:None,
            ability_permissions:access::legacy_ability_grants(),
            allowed_models:Vec::new(), tools:Default::default(),
            max_sessions:10, max_active_runs:2 };
        for tier in [&operator, &basic] {
            tier.validate()?;
            sqlx::query("INSERT OR IGNORE INTO entitlement_tiers(tier_id,revision,value) VALUES(?,?,?)")
                .bind(&tier.id).bind(tier.revision as i64)
                .bind(self.seal(&serde_json::to_value(tier)?)?)
                .execute(&mut *tx).await?;
        }
        let admin: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT value FROM user_accounts WHERE user_id='admin'")
            .fetch_optional(&mut *tx).await?;
        if let Some(admin) = admin {
            let mut account = self.unseal(&admin)?;
            let changed = account["status"].is_null() || account["role"].is_null()
                || account["tier_id"].is_null() || account["access_revision"].is_null();
            if changed {
                account["status"] = json!(AccountStatus::Active);
                account["role"] = json!(Role::Administrator);
                account["tier_id"] = json!("operator");
                account["access_revision"] = json!(1);
                sqlx::query("UPDATE user_accounts SET value=? WHERE user_id='admin'")
                    .bind(self.seal(&account)?).execute(&mut *tx).await?;
            }
            let membership = json!({"user_id":"admin","agent_id":"shared_default",
                "relationship":Relationship::Owner,"revision":1});
            sqlx::query("INSERT OR IGNORE INTO agent_memberships(user_id,agent_id,value) VALUES(?,?,?)")
                .bind("admin").bind("shared_default").bind(self.seal(&membership)?)
                .execute(&mut *tx).await?;
        }
        if marker.is_none() {
            sqlx::query("INSERT INTO docs(key,value) VALUES('storage:access_v1',?)")
                .bind(self.seal(&json!({"version":1}))?)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
