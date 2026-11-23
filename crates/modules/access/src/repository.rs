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
        "tier_id":principal.tier_id,"tier_until":value["tier_until"],
        "granted_tier":value["granted_tier"],"paid_until":value["paid"]["until"],
        "catalog_models":value["catalog_models"],
        "access_revision":principal.access_revision}),
    )
}
/// The plain index of accounts above Free: ids, tier ids and times only.
const ACCOUNT_TIERS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS account_tiers(user_id TEXT PRIMARY KEY,tier_id TEXT NOT NULL,admin_tier TEXT NOT NULL,grant_until INTEGER,paid_until INTEGER) WITHOUT ROWID";
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(u64::MAX)
}

/// A paid grant as stored on the account (plus tier 3.8).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaidSource {
    pub tier: String,
    pub until: u64,
    pub reference: String,
}

/// An account's tier sources: the administrator's choice (with its end), the paid grant and
/// the catalog switch. The account's tier is derived from them by the one writer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierSources {
    pub status: AccountStatus,
    pub admin_tier: String,
    pub admin_until: Option<u64>,
    pub paid: Option<PaidSource>,
    pub catalog_models: Option<bool>,
}

impl TierSources {
    /// The sources of a stored account (its state view).
    pub fn of(account: &Value) -> Result<Self> {
        let principal = Principal::from_account(account)?;
        let paid = &account["paid"];
        Ok(Self {
            status: principal.status,
            admin_tier: account["granted_tier"]
                .as_str()
                .filter(|tier| access::valid_id(tier))
                .map_or(principal.tier_id, str::to_owned),
            admin_until: account["tier_until"].as_u64(),
            paid: match (
                paid["tier"].as_str(),
                paid["until"].as_u64(),
                paid["reference"].as_str(),
            ) {
                (Some(tier), Some(until), Some(reference)) => Some(PaidSource {
                    tier: tier.to_owned(),
                    until,
                    reference: reference.to_owned(),
                }),
                _ => None,
            },
            catalog_models: account["catalog_models"].as_bool(),
        })
    }
    /// Which sources hold the account above Free: `grant`, `paid`, `both` or `none` (for the
    /// billing service).
    pub fn source(&self, now: u64) -> &'static str {
        source_of(
            &self.admin_tier,
            self.paid.as_ref().is_some_and(|paid| paid.until > now),
        )
    }
}

/// `grant` when the administrator's choice is above Free, `paid` when a paid grant is active,
/// `both`, or `none`.
pub fn source_of(admin_tier: &str, paid_active: bool) -> &'static str {
    let grant = admin_tier != access::FREE_TIER && admin_tier != access::ANONYMOUS_TIER;
    match (grant, paid_active) {
        (true, true) => "both",
        (true, false) => "grant",
        (false, true) => "paid",
        (false, false) => "none",
    }
}

/// One row of the plain `account_tiers` index: an account above Free and where it comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TierRow {
    pub user_id: String,
    pub tier_id: String,
    pub admin_tier: String,
    pub grant_until: Option<u64>,
    pub paid_until: Option<u64>,
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
    /// Set an account's status and tier with no end date (see [`Self::update_account_tier`]).
    pub async fn update_account_access(
        &self,
        id: &str,
        expected_revision: u64,
        status: AccountStatus,
        tier_id: &str,
    ) -> Result<Value> {
        self.update_account_tier(id, expected_revision, status, tier_id, None)
            .await
    }
    /// Set an account's status and tier, with an optional end date (Unix seconds) after which
    /// it returns to Free. Only a tier ranked above Free and below Administrator may end, and
    /// only in the future. This is the administrator's choice; an active paid grant still
    /// raises the account above it (plus tier 3.8). Writes through the one account writer, so
    /// `auth_epoch` rises; the caller stores the `account_due` row that brings the end, before
    /// calling this.
    pub async fn update_account_tier(
        &self,
        id: &str,
        expected_revision: u64,
        status: AccountStatus,
        tier_id: &str,
        tier_until: Option<u64>,
    ) -> Result<Value> {
        if !access::valid_id(id) || !access::valid_id(tier_id) || expected_revision == 0 {
            bail!("Invalid account update");
        }
        if let Some(until) = tier_until {
            crate::account_ladder::tier_end_allowed(
                &self.read_access_policy()?,
                tier_id,
                until,
                unix_now(),
            )?;
        }
        if id == "admin"
            && (status != AccountStatus::Active || tier_id != access::ADMINISTRATOR_TIER)
        {
            bail!("The original administrator must remain active");
        }
        if self.entitlement_tier(tier_id).await?.is_none() {
            bail!("Entitlement tier not found");
        }
        let (account, _) = self
            .write_tier_sources(id, Some(expected_revision), |sources| {
                sources.status = status;
                sources.admin_tier = tier_id.to_owned();
                sources.admin_until = tier_until;
                Ok(())
            })
            .await?;
        Ok(account)
    }
    /// Apply a paid grant (billing service): `tier` until `until`, once per `reference`. Only a
    /// tier between Free and Administrator; never creates an account or touches a role but
    /// through the tier. Returns the account view and the tier sources as now stored.
    pub async fn apply_paid_grant(
        &self,
        id: &str,
        tier: &str,
        until: u64,
        reference: &str,
    ) -> Result<(Value, TierSources)> {
        crate::account_ladder::grantable(&self.read_access_policy()?, tier)?;
        if until <= unix_now() || until > crate::account_ladder::LATEST_TIER_END {
            bail!("A grant's end must be in the future");
        }
        self.write_tier_sources(id, None, |sources| {
            sources.paid = Some(PaidSource {
                tier: tier.to_owned(),
                until,
                reference: reference.to_owned(),
            });
            Ok(())
        })
        .await
    }
    /// End the paid grant now if it is the one applied with `grant_reference`; `None` when
    /// there was nothing to end (already ended or replaced).
    pub async fn end_paid_grant(
        &self,
        id: &str,
        grant_reference: &str,
    ) -> Result<Option<(Value, TierSources)>> {
        let state = self
            .accounts()
            .account_state(id)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        if state["paid"]["reference"].as_str() != Some(grant_reference) {
            return Ok(None);
        }
        self.write_tier_sources(id, None, |sources| {
            if sources.paid.as_ref().map(|paid| paid.reference.as_str()) != Some(grant_reference) {
                bail!("Paid grant changed");
            }
            sources.paid = None;
            Ok(())
        })
        .await
        .map(Some)
    }
    /// End what has ended by `now`: the administrator's grant (back to Free) and the paid
    /// grant. Returns the new state, or `None` when nothing had ended.
    pub async fn end_passed_grants(
        &self,
        id: &str,
        now: u64,
    ) -> Result<Option<(Value, TierSources)>> {
        let Some(state) = self.accounts().account_state(id).await? else {
            return Ok(None);
        };
        let sources = TierSources::of(&state)?;
        let admin_ended = sources.admin_until.is_some_and(|until| until <= now);
        let paid_ended = sources.paid.as_ref().is_some_and(|paid| paid.until <= now);
        if !admin_ended && !paid_ended {
            return Ok(None);
        }
        self.write_tier_sources(id, None, |sources| {
            if sources.admin_until.is_some_and(|until| until <= now) {
                sources.admin_tier = access::FREE_TIER.to_owned();
                sources.admin_until = None;
            }
            if sources.paid.as_ref().is_some_and(|paid| paid.until <= now) {
                sources.paid = None;
            }
            Ok(())
        })
        .await
        .map(Some)
    }
    /// Set the owner's "catalog models allowed" value (`None`: the tier's default). Never on
    /// an administrator, who is not limited.
    pub async fn set_catalog_models(
        &self,
        id: &str,
        allowed: Option<bool>,
    ) -> Result<(Value, TierSources)> {
        self.write_tier_sources(id, None, |sources| {
            if sources.admin_tier == access::ADMINISTRATOR_TIER {
                bail!("Administrators are not limited");
            }
            sources.catalog_models = allowed;
            Ok(())
        })
        .await
    }
    /// The one path that changes an account's tier sources: read them in the writer
    /// transaction, apply `change`, derive the tier (the higher of the administrator's choice
    /// and an active paid grant) and its role, and write through the account writer (its
    /// `auth_epoch` and access revision rise). Keeps the plain `account_tiers` index (ids,
    /// tier ids and times only) of accounts above Free in the same transaction.
    async fn write_tier_sources(
        &self,
        id: &str,
        expected_revision: Option<u64>,
        change: impl FnOnce(&mut TierSources) -> Result<()>,
    ) -> Result<(Value, TierSources)> {
        if !access::valid_id(id) {
            bail!("Invalid account update");
        }
        let policy = self.read_access_policy()?;
        let now = unix_now();
        let mut tx = self.app_pool.begin().await?;
        self.accounts().lock_account_in(&mut tx, id).await?;
        let account = self
            .accounts()
            .account_state_in(&mut tx, id)
            .await?
            .ok_or_else(|| anyhow!("Account not found"))?;
        let revision = Principal::from_account(&account)?.access_revision;
        if expected_revision.is_some_and(|expected| expected != revision) {
            bail!("Account revision changed");
        }
        let mut sources = TierSources::of(&account)?;
        change(&mut sources)?;
        let tier_id = match &sources.paid {
            Some(paid) if paid.until > now => {
                crate::account_ladder::higher(&policy, &sources.admin_tier, &paid.tier)?
            }
            _ => sources.admin_tier.as_str(),
        }
        .to_owned();
        let role = Role::for_tier(&tier_id);
        // Only visitors who are not signed in hold the anonymous tier, and they hold no other.
        if (account["role"] == "guest") != (role == Role::Guest) {
            bail!("The anonymous tier belongs only to visitors who are not signed in");
        }
        if policy["tiers"].get(&tier_id).is_none() {
            bail!("Entitlement tier not found");
        }
        let status = sources.status;
        let role_value = serde_json::to_value(role)?;
        let granted = (tier_id != sources.admin_tier).then_some(sources.admin_tier.as_str());
        let written = self
            .accounts()
            .apply_access_in(
                &mut tx,
                id,
                webagent_accounts::AccessMutation {
                    expected_revision: revision,
                    status,
                    role: role_value.as_str().ok_or_else(|| anyhow!("Invalid role"))?,
                    tier_id: &tier_id,
                    tier_until: sources.admin_until,
                    sources: Some(webagent_accounts::TierSources {
                        granted_tier: granted,
                        paid: sources
                            .paid
                            .as_ref()
                            .map(|paid| webagent_accounts::PaidGrant {
                                tier: &paid.tier,
                                until: paid.until,
                                reference: &paid.reference,
                            }),
                        catalog_models: sources.catalog_models,
                    }),
                    profile_is_admin: status == AccountStatus::Active
                        && role == Role::Administrator,
                },
            )
            .await?;
        sqlx::query(ACCOUNT_TIERS_SCHEMA).execute(&mut *tx).await?;
        if tier_id == access::FREE_TIER || role == Role::Guest {
            sqlx::query("DELETE FROM account_tiers WHERE user_id=?")
                .bind(id)
                .execute(&mut *tx)
                .await?;
        } else {
            sqlx::query("INSERT INTO account_tiers(user_id,tier_id,admin_tier,grant_until,paid_until) VALUES(?,?,?,?,?) ON CONFLICT(user_id) DO UPDATE SET tier_id=excluded.tier_id,admin_tier=excluded.admin_tier,grant_until=excluded.grant_until,paid_until=excluded.paid_until")
                .bind(id)
                .bind(&tier_id)
                .bind(&sources.admin_tier)
                .bind(sources.admin_until.and_then(|until| i64::try_from(until).ok()))
                .bind(sources.paid.as_ref().and_then(|paid| i64::try_from(paid.until).ok()))
                .execute(&mut *tx)
                .await?;
        }
        audit(
            &mut tx,
            self,
            "account_access_changed",
            &json!({"user_id":id,
            "status":status,"role":role,"tier_id":tier_id,"tier_until":sources.admin_until,
            "granted_tier":granted,"paid_until":sources.paid.as_ref().map(|paid| paid.until),
            "catalog_models":sources.catalog_models,"access_revision":revision+1}),
        )
        .await?;
        tx.commit().await?;
        Ok((account_view(&written)?, TierSources::of(&written)?))
    }
    /// Accounts above Free from the plain `account_tiers` index (no account record is
    /// decrypted), for the billing service. An account changed before this index existed
    /// appears at its next tier change.
    pub async fn accounts_above_free(&self) -> Result<Vec<TierRow>> {
        sqlx::query(ACCOUNT_TIERS_SCHEMA)
            .execute(self.app_pool)
            .await?;
        let rows: Vec<(String, String, String, Option<i64>, Option<i64>)> = sqlx::query_as(
            "SELECT user_id,tier_id,admin_tier,grant_until,paid_until FROM account_tiers ORDER BY user_id",
        )
        .fetch_all(self.app_pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(user_id, tier_id, admin_tier, grant, paid)| TierRow {
                user_id,
                tier_id,
                admin_tier,
                grant_until: grant.and_then(|v| u64::try_from(v).ok()),
                paid_until: paid.and_then(|v| u64::try_from(v).ok()),
            })
            .collect())
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
                    .position(|id| id == crate::FREE_TIER)
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
        let mut members = Vec::new();
        for row in &rows {
            let mut value = self.unseal_agent(&row.get::<Vec<u8>, _>("value"))?;
            // A user who removed the agent is no longer one of its members.
            if value["origin"] == "left" {
                continue;
            }
            value["agent_operator"] =
                json!(value["agent_administrator"] == true || value["relationship"] == "owner");
            value["capability_level_id"] = json!(crate::capabilities::membership_level(
                Some(&value),
                serde_json::from_value(value["relationship"].clone())?
            )?);
            members.push(value);
        }
        Ok(members)
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
    /// A non-owner removes the agent from their own list: access is revoked and the
    /// hidden row keeps app-login or visitor access from granting it again.
    pub async fn leave_agent(&self, user: &str, agent: &str) -> Result<Value> {
        let current = self
            .agent_membership(user, agent)
            .await?
            .filter(|v| v["origin"] != "left");
        let expected = current.and_then(|v| v["revision"].as_u64()).unwrap_or(0);
        self.write_agent_membership(
            user,
            agent,
            Relationship::Revoked,
            expected,
            None,
            None,
            "left",
        )
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
        // A user who left is listed nowhere, so re-adding them starts a new membership.
        let previous = blob
            .as_ref()
            .map(|b| self.unseal_agent(b))
            .transpose()?
            .filter(|v| v["origin"] != "left");
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
            // Only the owner appoints, demotes or removes operators; operators manage the rest.
            let was_operator = previous
                .as_ref()
                .is_some_and(|v| v["agent_administrator"] == true);
            if (operator || was_operator) && actor != owner {
                bail!("Only the agent owner can appoint or remove Operators");
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
        // A test account manages only agents it owns itself (which are test agents).
        if operator && user != owner && webagent_accounts::is_test_account(&target) {
            bail!("A test account cannot manage another account's agent");
        }
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
        if self
            .accounts()
            .initialize_admin_assignment_in(
                &mut tx,
                AccountStatus::Active,
                "administrator",
                access::ADMINISTRATOR_TIER,
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
    /// One-time move to the administrator/free/anonymous tiers: renames the former
    /// `operator` and `basic` tier IDs, puts guests on anonymous and derives every role
    /// from its tier. Idempotent; an account already consistent is left untouched.
    pub async fn migrate_tiers_v2(&self) -> Result<()> {
        let mut tx = self.app_pool.begin().await?;
        let ids: Vec<String> = sqlx::query_scalar("SELECT user_id FROM user_accounts")
            .fetch_all(&mut *tx)
            .await?;
        for id in ids {
            let Some(account) = self.accounts().account_state_in(&mut tx, &id).await? else {
                continue;
            };
            let Some(tier) = account["tier_id"].as_str() else {
                continue;
            };
            let tier = if account["role"] == "guest" {
                access::ANONYMOUS_TIER
            } else {
                match tier {
                    "operator" => access::ADMINISTRATOR_TIER,
                    "basic" => access::FREE_TIER,
                    other => other,
                }
            };
            let role = serde_json::to_value(Role::for_tier(tier))?;
            if account["tier_id"] == tier && account["role"] == role {
                continue;
            }
            let status = webagent_accounts::account_status(&account)?;
            let revision = account["access_revision"].as_u64().unwrap_or(1);
            self.accounts()
                .apply_access_in(
                    &mut tx,
                    &id,
                    webagent_accounts::AccessMutation {
                        expected_revision: revision,
                        status,
                        role: role.as_str().unwrap_or("user"),
                        tier_id: tier,
                        tier_until: account["tier_until"].as_u64(),
                        sources: None,
                        profile_is_admin: status == AccountStatus::Active
                            && tier == access::ADMINISTRATOR_TIER,
                    },
                )
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

impl<C: RecordCodec> Repository<'_, C> {
    pub async fn initialize_authorization_tables(&self) -> Result<()> {
        // Tiers live in the access policy file; the former seed table is gone.
        sqlx::raw_sql("DROP TABLE IF EXISTS entitlement_tiers;
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
