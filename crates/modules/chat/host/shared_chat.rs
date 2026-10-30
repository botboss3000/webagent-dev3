//! Shared conversation access keeps actor and storage-owner identities separate.
use webagent_accounts_host::AccountsStore;
use crate::{store::Store, App, key};
use tokio_util::sync::CancellationToken;
use std::time::Duration;
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
pub use webagent_chat::shared::{AccessKind, SessionAuthorityDenied, SharedChatPolicy};
/// Resolved conversation binding. It stays in the host because it implements the Engine `SessionBinding` port.
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
pub async fn resolve(app: &App, sid: &str, user: &str) -> Result<SessionAccess> {
    resolve_internal(app, sid, user, false, false).await
}
async fn resolve_internal(
    app: &App,
    sid: &str,
    user: &str,
    lifecycle: bool,
    deleted_receipt: bool,
) -> Result<SessionAccess> {resolve_store_internal(&app.store,sid,user,lifecycle,deleted_receipt).await}
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
// A deleted owner's tombstone is accessible solely for an idempotent delete receipt.
pub async fn scope_delete(app: App, sid: &str) -> Result<App> {
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

async fn resolve_store_internal(
    store: &Store,
    sid: &str,
    user: &str,
    lifecycle: bool,
    deleted_receipt: bool,
) -> Result<SessionAccess> {
    let account = store
        .account_by_id(user)
        .await?
        .ok_or_else(|| anyhow!("Account not found"))?;
    if !crate::access::Principal::from_account(&account)?.active() || account["is_approved"] != true
    {
        bail!("Account unavailable");
    }
    let mut directory = store.shared_directory(sid).await?;
    if directory.is_none() {
        let owner = store.for_user(user).await?;
        if let Some(session) = owner.get(&format!("session:{sid}")).await? {
            store
                .register_shared_session(
                    user,
                    sid,
                    session["agent_id"].as_str().unwrap_or("shared_default"),
                )
                .await?;
            directory = store.shared_directory(sid).await?;
        }
    }
    let directory = directory.ok_or_else(|| anyhow!("Session not found"))?;
    let owner = directory["owner_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid directory"))?;
    let agent = directory["agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Invalid directory"))?;
    let storage=store.for_user(owner).await?;
    let pinned=storage.get(&format!("session:{sid}")).await?.ok_or_else(||anyhow!("Session not found"))?;
    let kind = if owner == user {
        AccessKind::Owner
    } else if store.shared_is_participant(sid, user).await? {
        AccessKind::Participant
    } else if pinned["conversation_privacy"]=="operator_reviewable" && staff(&store, user, agent).await? {
        AccessKind::Observer
    } else {
        bail!("Session not found");
    };
    let live = live_access(&store, user, sid, agent).await?;
    if !live.principal.active()
        || !live.agent_enabled
        || (kind != AccessKind::Observer && !live.admitted())
    {
        return Err(SessionAuthorityDenied.into());
    }
    let storage = store.for_user(owner).await?;
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
pub(crate) async fn resolve_store(store:&Store,sid:&str,user:&str)->Result<SessionAccess>{resolve_store_internal(store,sid,user,false,false).await}

pub(crate) async fn guard_operation<T>(store:&Store,sid:&str,cancel:CancellationToken,operation:impl std::future::Future<Output=Result<T>>)->Result<T> {
        let initial=store.get(&key("session",sid)).await?
            .filter(|session|session["status"]=="active")
            .ok_or_else(||anyhow::Error::from(crate::store::session::ArchivedSession))?;
        let epoch=initial["execution_epoch"].as_u64().unwrap_or(0);
        tokio::pin!(operation);
        let mut poll=tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                result=&mut operation=>{
                    let available=store.get(&key("session",sid)).await
                        .is_ok_and(|session|session.is_some_and(|s|s["status"]=="active"
                            && s["execution_epoch"].as_u64().unwrap_or(0)==epoch));
                    if !available {cancel.cancel();return Err(crate::store::session::ArchivedSession.into());}
                    return result;
                },
                _=poll.tick()=>{
                    let available=store.get(&key("session",sid)).await
                        .is_ok_and(|session|session.is_some_and(|s|s["status"]=="active"
                            && s["execution_epoch"].as_u64().unwrap_or(0)==epoch));
                    if !available {
                        cancel.cancel();
                        // Poll cancellation cleanup before dropping a worker socket.
                        let _=tokio::time::timeout(Duration::from_secs(5),&mut operation).await;
                        return Err(crate::store::session::ArchivedSession.into());
                    }
                }
            }
        }
    }

impl webagent_engine::runtime::ports::SessionBinding for SessionAccess {
 fn can_write(&self)->bool{SessionAccess::can_write(self)}
 fn session_id(&self)->&str{&self.session_id}
}
#[derive(Clone)]
pub(crate) struct EngineChat;
impl webagent_engine::runtime::ports::ChatAuthority<Store> for EngineChat {
 type Binding=SessionAccess;
 fn resolve<'a>(&'a self,store:&'a Store,sid:&'a str,user:&'a str)->impl std::future::Future<Output=Result<Self::Binding>>+Send+'a{resolve_store(store,sid,user)}
 fn live_access<'a>(&'a self,store:&'a Store,user:&'a str,sid:&'a str,agent:&'a str)->impl std::future::Future<Output=Result<webagent_access::authority::LiveAccess>>+Send+'a{live_access(store,user,sid,agent)}
 fn operation<'a,T:Send+'a,F:std::future::Future<Output=Result<T>>+Send+'a>(&'a self,store:&'a Store,sid:&'a str,cancel:CancellationToken,operation:F)->impl std::future::Future<Output=Result<T>>+Send+'a{guard_operation(store,sid,cancel,operation)}
 fn agent_id<'a>(&'a self,store:&'a Store,sid:&'a str)->impl std::future::Future<Output=Result<String>>+Send+'a {async move {
  let session=store.get(&format!("session:{sid}")).await?.ok_or_else(||anyhow!("Session not found"))?;
  if session["status"]!="active" {return Err(crate::store::session::ArchivedSession.into());}
  Ok(session["agent_id"].as_str().unwrap_or("shared_default").to_owned())
 }}
 fn messaging_owns(name:&str)->bool {crate::session_messaging::owns(name)}
 fn messaging_policy_defaults(policy:Value)->Value {crate::session_messaging::policy_defaults(policy)}

 fn participation_allows_run<'a>(&'a self,store:&'a Store,run:&'a Value)->impl std::future::Future<Output=Result<bool>>+Send+'a {store.participation_allows_run(run)}
}
