//! Manager reviews and scheduling compose concrete Models services with exact owner effects.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::{future::Future,path::{Path,PathBuf},sync::{Arc,atomic::{AtomicBool,Ordering}},time::{Duration,SystemTime,UNIX_EPOCH}};
use tokio::sync::{Mutex,RwLock};
use tokio_util::sync::CancellationToken;
use sqlx::SqlitePool;
use webagent_models::{model_selection,provider,storage::{RuntimeStorage,Repository as ModelRepository},runtime::{Runtime as Models,SessionAuthority},transport::{Provider,Projection,CallDraft}};
use crate::{Verdict,incoming_message,verdict,settings,storage::{RecordEffects,Repository}};
// rustc #100013: retain the same statically dispatched future behind its promised Send contract.
pub(crate) fn send_future<F:Future+Send>(future:F)->impl Future<Output=F::Output>+Send {future}
pub(crate) fn now()->u64 {SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()}
/// Actual Chat-scoped resources, returned by the existing owner after resolving the trusted requester.
pub struct SessionScope<C,A> {
 pub storage:C,pub user_pool:SqlitePool,pub agent_pool:SqlitePool,
 pub owner_id:String,pub agent_id:String,pub actor:String,pub session:String,pub can_write:bool,pub authority:A,
}
/// Existing Chat/Modules owner operations only; no Manager or Models decisions live here.
pub trait ReviewAuthority<C:RuntimeStorage+RecordEffects>:SessionAuthority+Clone+Send+Sync+'static {
 fn session_scope<'a>(&'a self,sid:&'a str,requester:&'a str)->impl Future<Output=Result<SessionScope<C,Self>>>+Send+'a;
 fn modules_binding<'a>(&'a self,run:&'a Value)->impl Future<Output=Result<()>>+Send+'a;
 fn session_operation<'a,T:Send+'a>(&'a self,sid:&'a str,cancel:CancellationToken,operation:impl Future<Output=Result<T>>+Send+'a)->impl Future<Output=Result<T>>+Send+'a;
}
pub trait Engine:Projection+Clone+Send+Sync+'static {
 fn snapshot(&self)->Result<(String,u64)>;
 fn current(&self,snapshot:&(String,u64))->Result<bool>;
}
/// Trusted composition clones existing handles; construction grants no authority and creates no resources.
#[derive(Clone)]
pub struct Runtime<C:RuntimeStorage+RecordEffects,A:ReviewAuthority<C>,P:Engine> {
 pub root:PathBuf,pub user_pool:SqlitePool,pub app_pool:SqlitePool,pub agent_pool:SqlitePool,
 pub owner:String,pub agent:String,pub actor:String,pub session:Option<String>,pub storage:C,pub authority:A,pub engine:P,
 pub provider:Provider,pub gate:Arc<Mutex<()>>,pub refresh:Arc<Mutex<()>>,pub login:Arc<RwLock<Option<Value>>>,pub stop:CancellationToken,pub ready:Arc<AtomicBool>,
}
impl<C:RuntimeStorage+RecordEffects,A:ReviewAuthority<C>,P:Engine> Runtime<C,A,P> {
 pub fn repository(&self)->Repository<'_,C> {Repository::new(&self.root,&self.user_pool,&self.owner,&self.storage)}
 pub(crate) fn model_repository(&self)->ModelRepository<'_,C> {ModelRepository::new(&self.root,&self.app_pool,&self.agent_pool,&self.agent,&self.owner,&self.storage)}
 fn models(&self)->Models<'_,C> {Models::new(self.model_repository(),&self.provider,&self.gate,&self.refresh,&self.login,&self.stop)}
 pub(crate) async fn shutoff(&self)->Result<bool> {Ok(self.storage.read_document("control").await?.unwrap_or(json!({"shutoff":true}))["shutoff"].as_bool().unwrap_or(true))}
 pub(crate) async fn run_candidate(&self,run:&Value)->Result<Option<model_selection::Candidate>> {self.authority.modules_binding(run).await?;{let models=self.models();send_future(models.run_candidate(run,&self.authority)).await}}
 async fn tokens_for(&self,selection:&provider::ProviderSelection,run:&Value)->Result<Value> {self.authority.modules_binding(run).await?;{let models=self.models();send_future(models.tokens_for(selection,run,&self.authority)).await}}
 fn requester<'a>(&'a self,record:&'a Value)->&'a str {record["run"]["requester_user_id"].as_str().unwrap_or(&self.owner)}
 pub(crate) async fn review_scope(&self,record:&Value)->Result<Self> {
  let sid=record["session_id"].as_str().ok_or_else(||anyhow!("Review session missing"))?;
  if record["run"]["session_id"].as_str()!=Some(sid)||record["run"]["agent_id"]!=record["agent_id"] {return Err(anyhow!("Review identity mismatch"));}
  let access=self.authority.session_scope(sid,self.requester(record)).await?;
  if !access.can_write||access.agent_id!=record["agent_id"].as_str().unwrap_or("")||access.owner_id!=self.owner {return Err(anyhow!("Review authority changed"));}
  Ok(Self {root:self.root.clone(),user_pool:access.user_pool,app_pool:self.app_pool.clone(),agent_pool:access.agent_pool,
   owner:access.owner_id,agent:access.agent_id,actor:access.actor,session:Some(access.session),storage:access.storage,authority:access.authority,
   engine:self.engine.clone(),provider:self.provider.clone(),gate:self.gate.clone(),refresh:self.refresh.clone(),login:self.login.clone(),
   stop:self.stop.clone(),ready:self.ready.clone()})
 }
 pub(crate) async fn participation_authorized(&self,record:&Value)->Result<()> {
  if !self.storage.participation_allows_run(&record["run"]).await? {return Err(anyhow!("Agent participation changed"));}
  if !self.storage.requester_claim_ready(&record["run"]).await? {return Err(anyhow!("Requester run quota changed"));}
  self.review_scope(record).await?;
  Ok(())
 }
 pub fn system_proxy(&self,record:&Value)->Value {json!({"agent_id":record["agent_id"],"session_id":record["session_id"],
  "requester_user_id":record["run"]["requester_user_id"].as_str().unwrap_or(&self.actor),"participation_generation":record["run"]["participation_generation"],"selection_plan":record["selection_plan"],"failover_state":record["failover_state"]})}
 pub async fn prepare(&self,agent:&str)->Result<Option<Value>> {
  let(settings,settings_revision)=settings::snapshot(&self.repository(),agent).await?;
  if !settings.enabled {return Ok(None);}
  let mut plan=self.model_repository().model_catalog().await?.resolve_role(agent,model_selection::ModelRole::System)?;
  self.models().pin_model_plan(&mut plan,&self.actor,self.session.as_deref(),&self.authority).await?;
  Ok(Some(json!({"settings":settings,"settings_revision":settings_revision,"selection_plan":plan,"failover_state":model_selection::FailoverState::default()})))
 }
pub fn judge<'a>(&'a self, record: &'a mut Value) -> impl Future<Output=Result<Verdict>>+Send+'a+use<'a,C,A,P> {
    async move {
    self.participation_authorized(record).await?;
    let agent = record["agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing agent"))?
        .to_owned();
    let pinned: crate::configuration::IncomingMessage = serde_json::from_value(record["settings"].clone())?;
    let settings_revision = record["settings_revision"].as_u64().unwrap_or(0);
    let (current_settings, current_revision) = settings::snapshot(&self.repository(), &agent).await?;
    if current_settings != pinned || current_revision != settings_revision || self.shutoff().await? {
        return Err(anyhow!("Manager settings or authority changed"));
    }
    let proxy = self.system_proxy(record);
    let candidate = self
        .run_candidate(&proxy)
        .await?
        .ok_or_else(|| anyhow!("System model required"))?;
    // A role removal/reassignment withdraws the pending gate even if its connection survives.
    let live = self
        .model_repository()
        .model_catalog()
        .await?
        .resolve_role(&agent, model_selection::ModelRole::System)?;
    let pinned_plan: model_selection::SelectionPlan =
        serde_json::from_value(record["selection_plan"].clone())?;
    if live.assignment_revision != pinned_plan.assignment_revision
        || live.candidates.first().map(|c| &c.entry_id) != Some(&candidate.entry_id)
    {
        return Err(anyhow!("Manager model assignment changed"));
    }
    let selection = provider::ProviderSelection { limits:candidate.limits.clone(),
        provider: candidate.transport.provider_id(),
        model: candidate.upstream_model.clone(),
    };
    let tokens = self.tokens_for(&selection, &proxy).await?;
    let cancel = self.stop.child_token();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    // Deltas are private and discarded; a judge cannot publish a working message.
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let call_id = format!("manager:{}", record["id"].as_str().unwrap_or(""));
    let instructions = incoming_message::instructions(&pinned.criteria);
    let result = {
    let call_record = &*record;
    let call_pinned = &pinned;
    let call_agent = &agent;
    let call_live = &live;
    let call_candidate = &candidate;
    let call_proxy = &proxy;
    let call_cancel = &cancel;
    let call = async move {
        let record = call_record;
        let pinned = call_pinned;
        let agent = call_agent;
        let live = call_live;
        let candidate = call_candidate;
        let proxy = call_proxy;
        let selection = &selection;
        let tokens = &tokens;
        let cancel = call_cancel.clone();
        // Complete pure projection before the final admission fence and network poll.
        let engine_snapshot = self.engine.snapshot()?;
        let prepared = CallDraft::new(
            selection,
            incoming_message::input(&record["message"]),
            Vec::new(),
            &call_id,
            &instructions,
        ).prepare(&self.engine, &cancel).await?;
        let dispatch_guard = self.gate.lock().await;
        self.participation_authorized(record).await?;
        let current_candidate = self.run_candidate(&proxy).await?
            .ok_or_else(|| anyhow!("System model required"))?;
        let current_selection = provider::ProviderSelection {
            limits: current_candidate.limits.clone(),
            provider: current_candidate.transport.provider_id(),
            model: current_candidate.upstream_model.clone(),
        };
        let (current_settings, current_revision) = settings::snapshot(&self.repository(), &agent).await?;
        let current_role = self.model_repository().model_catalog().await?
            .resolve_role(&agent, model_selection::ModelRole::System)?;
        if self.shutoff().await? || current_settings != *pinned
            || current_revision != settings_revision || current_selection != *selection
            || current_candidate.entry_id != candidate.entry_id
            || current_role.assignment_revision != live.assignment_revision
            || current_role.candidates != live.candidates {
            return Err(anyhow!("Manager settings or authority changed"));
        }
        if cancel.is_cancelled()
                        || !self.engine.current(&engine_snapshot)?
                    {
                        return Err(anyhow!("Provider component or authority changed"));
                    }
        // Poll the actual transport once while authority writers remain fenced.
                    let mut completion = Box::pin(async {
                        let ready = prepared.bind_credentials(tokens, &cancel)?;
                        self.provider.dispatch(ready, tx, cancel).await
                    });
                    let mut immediate = None;
                    futures_util::future::poll_fn(|cx| {
                        if let std::task::Poll::Ready(result) =
                            std::future::Future::poll(completion.as_mut(), cx)
                        {
                            immediate = Some(result);
                        }
                        std::task::Poll::Ready(())
                    }).await;
                    drop(dispatch_guard);
                    match immediate {
                        Some(result) => result,
                        None => completion.await,
                    }
    };
    let call = send_future(call);
    tokio::time::timeout(Duration::from_secs(40),
        send_future(self.authority.session_operation(record["session_id"].as_str().unwrap_or(""),cancel.clone(),call))).await
    };
    cancel.cancel();
    drain.abort();
    let response = result.map_err(|_| anyhow!("Manager review timed out"))??;
    self.storage.record_usage(record, &response);
    let items = response["output"]
        .as_array()
        .ok_or_else(|| anyhow!("Manager output missing"))?;
    if items.iter().any(|v| v["type"] == "function_call") {
        return Err(anyhow!("Manager attempted a tool call"));
    }
    let text = items
        .iter()
        .filter(|v| v["type"] == "message")
        .flat_map(|v| v["content"].as_array().into_iter().flatten())
        .filter_map(|v| v["text"].as_str())
        .collect::<Vec<_>>()
        .join("");
    let verdict = verdict::parse(&text)?;
    self.participation_authorized(record).await?;
    // A late result never grants access after revocation or a criteria edit.
    let (current_settings, current_revision) = settings::snapshot(&self.repository(), &agent).await?;
    if self.shutoff().await? || current_settings != pinned || current_revision != settings_revision {
        return Err(anyhow!("Manager settings or authority changed"));
    }
    self.run_candidate(&proxy).await?;
    let current = self
        .model_repository()
        .model_catalog()
        .await?
        .resolve_role(&agent, model_selection::ModelRole::System)?;
    if current.assignment_revision != live.assignment_revision
        || current.candidates != live.candidates
    {
        return Err(anyhow!("Manager model assignment changed"));
    }
    if verdict == Verdict::Acceptable {
        self.run_candidate(&record["run"]).await?;
    }
    Ok(verdict)
    }
}

 pub(crate) async fn pending(&self)->Result<Vec<Value>> {self.storage.list_documents("manager_pending:").await}

}
