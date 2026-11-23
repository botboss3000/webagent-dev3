//! A separate bounded review queue inside the V3 loop process.
use crate::runtime::*;
use crate::storage::RecordEffects;
use crate::{settings, Verdict};
use anyhow::{anyhow, Result};
use serde_json::Value;
use std::{future::Future, sync::atomic::Ordering, time::Duration};
use webagent_models::{model_selection, storage::RuntimeStorage};
pub fn process<C: RuntimeStorage + RecordEffects, A: ReviewAuthority<C>, P: Engine>(
    app: Runtime<C, A, P>,
    id: String,
) -> impl Future<Output = Result<()>> + Send + 'static {
    // A review embeds provider and authority futures; keep callers off that inline stack.
    Box::pin(async move {
        let Some(mut record) = app.repository().claim_manager_review(&id).await? else {
            return Ok(());
        };
        let app = match app.review_scope(&record).await {
            Ok(scoped) => scoped,
            Err(_) => {
                app.repository()
                    .finish_manager_review(&record, "review_error", None)
                    .await?;
                return Ok(());
            }
        };
        // Include credential refresh and authority reads in the operation deadline.
        let result =
            tokio::time::timeout(Duration::from_secs(45), Box::pin(app.judge(&mut record)))
                .await
                .unwrap_or_else(|_| Err(anyhow!("Manager operation timed out")));
        let _guard = app.gate.lock().await;
        let result = match result {
            Ok(Verdict::Acceptable) => {
                let agent = record["agent_id"].as_str().unwrap_or("");
                let pinned: crate::configuration::IncomingMessage =
                    serde_json::from_value(record["settings"].clone())?;
                let plan: model_selection::SelectionPlan =
                    serde_json::from_value(record["selection_plan"].clone())?;
                let live = app
                    .model_repository()
                    .model_catalog()
                    .await?
                    .resolve_role(agent, model_selection::ModelRole::System);
                let (current_settings, current_revision) =
                    settings::snapshot(&app.repository(), agent).await?;
                if app.participation_authorized(&record).await.is_err()
                    || app.shutoff().await?
                    || current_settings != pinned
                    || current_revision != record["settings_revision"].as_u64().unwrap_or(0)
                    || live.is_err()
                    || live.is_ok_and(|current| current.roster_revision != plan.roster_revision)
                    || app.run_candidate(&record["run"]).await.is_err()
                    || app.run_candidate(&app.system_proxy(&record)).await.is_err()
                {
                    Err(anyhow!("Manager admission authority changed"))
                } else {
                    Ok(Verdict::Acceptable)
                }
            }
            other => other,
        };
        let (status, verdict) = match result {
            Ok(Verdict::Acceptable) => ("acceptable", Some(Verdict::Acceptable)),
            Ok(Verdict::Unacceptable) => ("unacceptable", Some(Verdict::Unacceptable)),
            Err(_) => ("review_error", None),
        };
        app.repository()
            .finish_manager_review(&record, status, verdict)
            .await?;
        Ok(())
    })
}

pub async fn scheduler<
    C: RuntimeStorage + RecordEffects,
    A: ReviewAuthority<C>,
    P: Engine,
    U: OwnerSources<C, A, P>,
>(
    app: Runtime<C, A, P>,
    owners: U,
) {
    let mut jobs = tokio::task::JoinSet::new();
    let mut hints = owners.hints();
    loop {
        while let Some(result) = jobs.try_join_next() {
            if !matches!(result, Ok(Ok(()))) {
                tracing::warn!("Manager review deferred");
            }
        }
        if app.stop.is_cancelled() || !app.ready.load(Ordering::Acquire) {
            break;
        }
        if jobs.len() < 2 {
            // C8: only the accounts written since this scheduler last finished them, or not
            // looked at for `FALLBACK` (a review's claim deadline is time-based).
            if let Ok(users) = owners.user_ids().await {
                for (account, mark) in users {
                    if jobs.len() >= 2 {
                        break;
                    }
                    let Some(user) = owners.user(&account).await else {
                        continue;
                    };
                    // Finished: every pending review was read and either started or left
                    // for its deadline; one cut off by the job limit stays due.
                    let mut finished = true;
                    match user.pending().await {
                        Err(_) => finished = false,
                        Ok(pending) => {
                            for pointer in pending {
                                if jobs.len() >= 2 {
                                    finished = false;
                                    break;
                                }
                                if let Some(id) = pointer["id"].as_str() {
                                    let record = match user
                                        .storage
                                        .read_document(&format!("manager_review:{id}"))
                                        .await
                                    {
                                        Ok(Some(record)) => record,
                                        Ok(None) => continue,
                                        Err(_) => {
                                            finished = false;
                                            continue;
                                        }
                                    };
                                    if record["status"] == "pending_review" {
                                        jobs.spawn(process(user.clone(), id.to_owned()));
                                    } else if record["status"] == "reviewing"
                                        && record["deadline"].as_u64().unwrap_or(0) <= now()
                                    {
                                        // An interrupted provider call may have incurred usage. Never replay it.
                                        let _ = user
                                            .repository()
                                            .finish_manager_review(&record, "review_error", None)
                                            .await;
                                    }
                                }
                            }
                        }
                    }
                    if finished {
                        owners.looked(&account, mark);
                    }
                }
            }
        }
        // Message admission queues a review and then sends a wake hint; the fallback covers
        // claim deadlines and lost hints. A finished review frees a slot for the next one.
        tokio::select! {
            _ = app.stop.cancelled() => break,
            Some(result) = jobs.join_next(), if !jobs.is_empty() => {
                if !matches!(result, Ok(Ok(()))) {
                    tracing::warn!("Manager review deferred");
                }
            }
            _ = hints.changed() => {}
            _ = tokio::time::sleep(FALLBACK) => {}
        }
    }
    jobs.abort_all();
}

/// Longest wait between review scans without a wake hint, and the longest an unchanged
/// account is left unread.
pub const FALLBACK: Duration = Duration::from_secs(5);

pub trait OwnerSources<C: RuntimeStorage + RecordEffects, A: ReviewAuthority<C>, P: Engine>:
    Sync
{
    /// What an account's contents were when it was listed, handed back to
    /// [`OwnerSources::looked`].
    type Mark: Send;
    /// The accounts to look at, each with its mark taken before it is read: those written
    /// since [`OwnerSources::looked`] last recorded them, and any recorded [`FALLBACK`] ago or
    /// more. Each is opened with [`OwnerSources::user`] in turn and let go before the next,
    /// so idle account databases can be closed meanwhile.
    fn user_ids(&self) -> impl Future<Output = Result<Vec<(String, Self::Mark)>>> + Send + '_;
    /// The scheduler finished the account as of `mark`: nothing was left for later.
    fn looked(&self, id: &str, mark: Self::Mark);
    /// One account's runtime; `None` when its storage cannot be opened (skipped).
    fn user<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Option<Runtime<C, A, P>>> + Send + 'a;
    /// The process's wake hints (a sequence that moves on each hint; no account or content).
    fn hints(&self) -> tokio::sync::watch::Receiver<u64>;
}
