//! A separate bounded review queue inside the V3 loop process.
use crate::runtime::*;
use crate::{Verdict,settings};
use anyhow::{anyhow,Result};
use serde_json::Value;
use webagent_models::{storage::RuntimeStorage,model_selection};
use crate::storage::RecordEffects;
use std::{future::Future,sync::atomic::Ordering,time::Duration};
pub fn process<C:RuntimeStorage+RecordEffects,A:ReviewAuthority<C>,P:Engine>(app: Runtime<C,A,P>, id: String) -> impl Future<Output=Result<()>>+Send+'static {
    // A review embeds provider and authority futures; keep callers off that inline stack.
    Box::pin(async move {
    let Some(mut record) = app.repository().claim_manager_review(&id).await? else {
        return Ok(());
    };
    let app = match app.review_scope(&record).await {
        Ok(scoped) => scoped,
        Err(_) => {
            app.repository().finish_manager_review(&record, "review_error", None).await?;
            return Ok(());
        }
    };
    // Include credential refresh and authority reads in the operation deadline.
    let result = tokio::time::timeout(Duration::from_secs(45), Box::pin(app.judge(&mut record)))
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
            let (current_settings, current_revision) = settings::snapshot(&app.repository(), agent).await?;
            if app.participation_authorized(&record).await.is_err()
                || app.shutoff().await?
                || current_settings != pinned
                || current_revision != record["settings_revision"].as_u64().unwrap_or(0)
                || live.is_err()
                || live.is_ok_and(|current| current.assignment_revision != plan.assignment_revision)
                || app.run_candidate(&record["run"]).await.is_err()
                || app
                    .run_candidate(
                        &app.system_proxy(&record),
                    )
                    .await
                    .is_err()
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

pub async fn scheduler<C:RuntimeStorage+RecordEffects,A:ReviewAuthority<C>,P:Engine,U:OwnerSources<C,A,P>>(app: Runtime<C,A,P>,owners:U) {
    let mut jobs = tokio::task::JoinSet::new();
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
            if let Ok(users) = owners.users().await {
                for user in users {
                    if jobs.len() >= 2 {
                        break;
                    }
                    if let Ok(pending) = user.pending().await {
                        for pointer in pending {
                            if jobs.len() >= 2 {
                                break;
                            }
                            if let Some(id) = pointer["id"].as_str() {
                                let Some(record) = user
                                    .storage
                                    .read_document(&format!("manager_review:{id}"))
                                    .await
                                    .ok()
                                    .flatten()
                                else {
                                    continue;
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
            }
        }
        tokio::select! {_=app.stop.cancelled()=>break,
        _=tokio::time::sleep(Duration::from_millis(250))=>{}}
    }
    jobs.abort_all();
}

pub trait OwnerSources<C:RuntimeStorage+RecordEffects,A:ReviewAuthority<C>,P:Engine>:Sync {fn users(&self)->impl Future<Output=Result<Vec<Runtime<C,A,P>>>>+Send+'_;}
