//! A separate bounded review queue inside the V3 loop process.
use super::*;
pub(super) async fn process(app: App, id: String) -> Result<()> {
    let Some(mut record) = app.store.claim_manager_review(&id).await? else {
        return Ok(());
    };
    let app = match review_scope(&app, &record).await {
        Ok(scoped) => scoped,
        Err(_) => {
            app.store.finish_manager_review(&record, "review_error", None).await?;
            return Ok(());
        }
    };
    // Include credential refresh and authority reads in the operation deadline.
    let result = tokio::time::timeout(Duration::from_secs(45), review::judge(&app, &mut record))
        .await
        .unwrap_or_else(|_| Err(anyhow!("Manager operation timed out")));
    let _guard = app.gate.lock().await;
    let result = match result {
        Ok(Verdict::Acceptable) => {
            let agent = record["agent_id"].as_str().unwrap_or("");
            let pinned: config::IncomingMessage =
                serde_json::from_value(record["settings"].clone())?;
            let plan: model_selection::SelectionPlan =
                serde_json::from_value(record["selection_plan"].clone())?;
            let live = app
                .store
                .model_catalog()
                .await?
                .resolve_role(agent, model_selection::ModelRole::System);
            let (current_settings, current_revision) = config::snapshot(&app.store, agent).await?;
            if participation_authorized(&app, &record).await.is_err()
                || app.shutoff().await?
                || current_settings != pinned
                || current_revision != record["settings_revision"].as_u64().unwrap_or(0)
                || live.is_err()
                || live.is_ok_and(|current| current.assignment_revision != plan.assignment_revision)
                || app.run_candidate(&record["run"]).await.is_err()
                || app
                    .run_candidate(
                        &system_proxy(&app, &record),
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
    app.store
        .finish_manager_review(&record, status, verdict)
        .await?;
    Ok(())
}
pub(crate) async fn scheduler(app: App) {
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
            if let Ok(users) = crate::user_apps(&app).await {
                for user in users {
                    if jobs.len() >= 2 {
                        break;
                    }
                    if let Ok(pending) = user.store.list("manager_pending:").await {
                        for pointer in pending {
                            if jobs.len() >= 2 {
                                break;
                            }
                            if let Some(id) = pointer["id"].as_str() {
                                let Some(record) = user
                                    .store
                                    .get(&format!("manager_review:{id}"))
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
                                        .store
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
