//! Shared conversation access keeps actor and storage-owner identities separate.
use crate::{store::Store, App};
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SharedChatPolicy {
    pub shareable: bool,
    pub admin_view: bool,
    pub admin_join: bool,
    pub help: bool,
    pub initial_active: bool,
}
impl Default for SharedChatPolicy {
    fn default() -> Self {
        Self {
            shareable: false,
            admin_view: false,
            admin_join: false,
            help: false,
            initial_active: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    Owner,
    Participant,
    Observer,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionAccess {
    pub actor_id: String,
    pub owner_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub kind: AccessKind,
}
impl SessionAccess {
    pub fn can_write(&self) -> bool {
        self.kind != AccessKind::Observer
    }
    pub fn is_owner(&self) -> bool {
        self.kind == AccessKind::Owner
    }
}
pub fn actor(app: &App) -> &str {
    app.chat_actor
        .as_deref()
        .unwrap_or(app.store.current_user_id())
}
pub async fn policy(store: &Store, agent: &str) -> Result<SharedChatPolicy> {
    let scoped = store.for_agent(agent).await?;
    let raw: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(&scoped.agent_pool)
        .await?;
    let value = scoped.unseal(&raw)?;
    if value["shared_chat"].is_null() {
        Ok(SharedChatPolicy::default())
    } else {
        Ok(serde_json::from_value(value["shared_chat"].clone())?)
    }
}
pub async fn staff(store: &Store, user: &str, agent: &str) -> Result<bool> {
    Ok(store.agent_authority(user, agent).await?["can_manage"] == true)
}
#[derive(Debug)]
pub(crate) struct SessionAuthorityDenied;
impl std::fmt::Display for SessionAuthorityDenied {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {f.write_str("Conversation authority unavailable")}
}
impl std::error::Error for SessionAuthorityDenied {}
pub async fn resolve(app: &App, sid: &str, user: &str) -> Result<SessionAccess> {
    resolve_internal(app, sid, user, false, false).await
}
async fn resolve_internal(
    app: &App,
    sid: &str,
    user: &str,
    lifecycle: bool,
    deleted_receipt: bool,
) -> Result<SessionAccess> {
    let account = app
        .store
        .account_by_id(user)
        .await?
        .ok_or_else(|| anyhow!("Account not found"))?;
    if !crate::access::Principal::from_account(&account)?.active() || account["is_approved"] != true
    {
        bail!("Account unavailable");
    }
    let mut directory = app.store.shared_directory(sid).await?;
    if directory.is_none() {
        let owner = app.store.for_user(user).await?;
        if let Some(session) = owner.get(&format!("session:{sid}")).await? {
            app.store
                .register_shared_session(
                    user,
                    sid,
                    session["agent_id"].as_str().unwrap_or("shared_default"),
                )
                .await?;
            directory = app.store.shared_directory(sid).await?;
        }
    }
    let directory = directory.ok_or_else(|| anyhow!("Session not found"))?;
    let owner = directory["owner_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid directory"))?;
    let agent = directory["agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid directory"))?;
    let kind = if owner == user {
        AccessKind::Owner
    } else if app.store.shared_is_participant(sid, user).await? {
        AccessKind::Participant
    } else if policy(&app.store, agent).await?.admin_view && staff(&app.store, user, agent).await? {
        AccessKind::Observer
    } else {
        bail!("Session not found");
    };
    let live = live_access(&app.store, user, sid, agent).await?;
    if !live.principal.active()
        || !live.agent_enabled
        || (kind != AccessKind::Observer && !live.admitted())
    {
        return Err(SessionAuthorityDenied.into());
    }
    let storage = app.store.for_user(owner).await?;
    let session = storage
        .get(&format!("session:{sid}"))
        .await?
        .ok_or_else(|| anyhow!("Session not found"))?;
    if (session["status"] == "deleted" && !deleted_receipt) || (!lifecycle && session["status"] != "active") {
        bail!("Session unavailable");
    }
    if lifecycle && kind != AccessKind::Owner {
        bail!("Owner access required");
    }
    Ok(SessionAccess {
        actor_id: user.into(),
        owner_id: owner.into(),
        session_id: sid.into(),
        agent_id: agent.into(),
        kind,
    })
}
pub async fn scope(app: App, sid: &str) -> Result<App> {
    let actor = actor(&app).to_owned();
    let access = resolve(&app, sid, &actor).await?;
    let mut scoped = app.clone();
    scoped.store = app
        .store
        .for_user(&access.owner_id)
        .await?
        .for_agent(&access.agent_id)
        .await?;
    scoped.chat_actor = Some(actor);
    scoped.chat_access = Some(access);
    Ok(scoped)
}
pub fn observer_projection(value: &mut Value) {
    if let Some(map) = value.as_object_mut() {
        for field in [
            "checkpoint",
            "input",
            "diagnostics",
            "pending",
            "approval",
            "approvals",
            "tool_calls",
            "context",
            "recovery",
        ] {
            map.remove(field);
        }
    }
}

pub async fn live_access(
    store: &Store,
    user: &str,
    sid: &str,
    agent: &str,
) -> Result<crate::access_policy::LiveAccess> {
    let mut access = store.live_access(user, agent).await?;
    if access.relationship == crate::access::Relationship::Revoked
        && store.agent_membership(user, agent).await?.is_none()
        && store.shared_is_participant(sid, user).await?
    {
        let directory = store
            .shared_directory(sid)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        if directory["agent_id"] != agent {
            bail!("Session agent conflict");
        }
        access.relationship = crate::access::Relationship::Caller;
    }
    Ok(access)
}

pub async fn scope_lifecycle(app: App, sid: &str) -> Result<App> {
    scope_lifecycle_receipt(app,sid,false).await
}
// A deleted owner's tombstone is accessible solely for an idempotent purge receipt.
pub async fn scope_purge(app: App, sid: &str) -> Result<App> {
    scope_lifecycle_receipt(app,sid,true).await
}
async fn scope_lifecycle_receipt(app: App, sid: &str, deleted_receipt:bool) -> Result<App> {
    let actor = actor(&app).to_owned();
    let access = resolve_internal(&app, sid, &actor, true, deleted_receipt).await?;
    if !access.is_owner() {
        bail!("Owner access required");
    }
    let mut scoped = app.clone();
    scoped.store = app
        .store
        .for_user(&access.owner_id)
        .await?
        .for_agent(&access.agent_id)
        .await?;
    scoped.chat_actor = Some(actor);
    scoped.chat_access = Some(access);
    Ok(scoped)
}

// Shared conversations retain each incoming message as a separate requester-bound follow-up.
pub(crate) async fn uses_shared_queue(app:&App,sid:&str)->Result<bool>{
    if actor(app)!=app.store.current_user_id(){return Ok(true);}
    let session=app.store.get(&format!("session:{sid}")).await?.ok_or_else(||anyhow::anyhow!("Session not found"))?;
    let agent=session["agent_id"].as_str().unwrap_or("shared_default");
    Ok(policy(&app.store,agent).await?.shareable || app.store.shared_participants(sid).await?.iter().any(|member|member["user_id"]!=app.store.current_user_id()))
}
