//! Engine operations over one trusted existing Storage scope and foreign same-Tx owner effects.
use super::ports::*;
use crate::ports::*;
use anyhow::Result;
use serde_json::Value;
#[derive(Clone)]
pub struct Scope<S> {
    records: S,
}
impl<S> Scope<S> {
    pub fn new(records: S) -> Self {
        Self { records }
    }
    pub fn records(&self) -> &S {
        &self.records
    }
}
impl<S> std::ops::Deref for Scope<S> {
    type Target = S;
    fn deref(&self) -> &S {
        &self.records
    }
}
impl<
        S: ScopedStorage
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
    > Scope<S>
{
    pub fn repository(&self) -> crate::repository::Repository<'_, S, S, S, S, S> {
        crate::repository::Repository::new(
            AgentMetadata::root(&self.records),
            ScopedStorage::user_pool(&self.records),
            ScopedStorage::application_pool(&self.records),
            ScopedStorage::agent_id(&self.records),
            &self.records,
            &self.records,
            &self.records,
            &self.records,
            &self.records,
        )
    }
    pub async fn for_user(&self, user: &str) -> Result<Self> {
        Ok(Self::new(
            ScopedStorage::for_user(&self.records, user).await?,
        ))
    }
    pub async fn for_agent(&self, agent: &str) -> Result<Self> {
        Ok(Self::new(
            AgentMetadata::for_agent(&self.records, agent).await?,
        ))
    }
    pub async fn active_runs(&self) -> Result<Vec<Value>> {
        self.repository().active_runs().await
    }
    pub async fn recent_runs(&self, sid: Option<&str>) -> Result<Vec<Value>> {
        self.repository().recent_runs(sid).await
    }
    pub async fn newest_run_id(&self, sid: &str) -> Result<Option<String>> {
        self.repository().newest_run_id(sid).await
    }
    pub async fn last_other_run_at(&self, sid: &str, rid: &str) -> Result<Option<i64>> {
        self.repository().last_other_run_at(sid, rid).await
    }
    pub async fn save_run(&self, run: &mut Value, kind: &str, payload: &Value) -> Result<()> {
        self.repository().save_run(run, kind, payload).await
    }
    pub async fn save_run_override(
        &self,
        run: &mut Value,
        kind: &str,
        payload: &Value,
    ) -> Result<()> {
        self.repository()
            .save_run_override(run, kind, payload)
            .await
    }
    pub async fn claim_run(&self, run: &mut Value, owner: &str, generation: &str) -> Result<bool> {
        self.repository().claim_run(run, owner, generation).await
    }
    pub async fn claim_run_within(
        &self,
        run: &mut Value,
        owner: &str,
        generation: &str,
        capacity: Capacity,
    ) -> Result<bool> {
        self.repository()
            .claim_run_within(run, owner, generation, capacity)
            .await
    }
    pub async fn scheduler_runs(&self) -> Result<Vec<(String, String)>> {
        self.repository().scheduler_runs().await
    }
    pub async fn run_lease(&self, rid: &str) -> Result<crate::repository::RunLease> {
        self.repository().run_lease(rid).await
    }
    pub async fn renew_run_lease(
        &self,
        rid: &str,
        generation: &str,
        owner: &str,
        epoch: i64,
    ) -> Result<bool> {
        self.repository()
            .renew_run_lease(rid, generation, owner, epoch)
            .await
    }
    pub async fn beat_loop_member(
        &self,
        member: crate::partition::Member,
        owner: &str,
        state: crate::partition::MemberState,
    ) -> Result<bool> {
        self.repository()
            .beat_loop_member(member, owner, state)
            .await
    }
    pub async fn live_loop_owners(&self, except: Option<u32>) -> Result<Vec<String>> {
        self.repository().live_loop_owners(except).await
    }
    pub async fn release_run_place(&self, rid: &str) -> Result<()> {
        self.repository().release_run_place(rid).await
    }
    pub async fn release_places_of_gone_loops(&self, live: &[String]) -> Result<()> {
        self.repository().release_places_of_gone_loops(live).await
    }
    pub async fn acknowledge_loop_commands(&self) -> Result<usize> {
        self.repository().acknowledge_loop_commands().await
    }

    pub async fn save_run_observed(
        &self,
        run: &mut Value,
        kind: &str,
        payload: &Value,
    ) -> Result<()> {
        let before = run["run_revision"].clone();
        self.repository().save_run(run, kind, payload).await?;
        if run["run_revision"] != before {
            self.records.committed(run, kind, false);
        }
        Ok(())
    }
    pub async fn save_run_override_observed(
        &self,
        run: &mut Value,
        kind: &str,
        payload: &Value,
    ) -> Result<()> {
        let before = run["run_revision"].clone();
        self.repository()
            .save_run_override(run, kind, payload)
            .await?;
        if run["run_revision"] != before {
            self.records.committed(run, kind, true);
        }
        Ok(())
    }
    pub async fn context_assets(
        &self,
        sid: &str,
        agent: &str,
        requester: &str,
    ) -> Result<webagent_assets_contracts::State> {
        self.repository()
            .context_assets(sid, agent, requester)
            .await
    }
}

impl<
        S: ScopedStorage
            + ChatEffects
            + AdmissionEffects
            + NotificationEffects
            + UsageEffects
            + RunObservation
            + RunPresentation,
    > Scope<S>
{
    pub async fn background_get(&self, id: &str) -> Result<Option<Value>> {
        self.repository().background_get(id).await
    }
    pub async fn background_delivered(&self, task: &Value, owner: &str) -> Result<()> {
        self.repository().background_delivered(task, owner).await
    }
    pub async fn background_scan(&self, sid: Option<&str>) -> Result<Vec<Value>> {
        self.repository().background_scan(sid).await
    }
    pub async fn background_cas(&self, value: &Value, expected: Option<u64>) -> Result<bool> {
        self.repository().background_cas(value, expected).await
    }
}
