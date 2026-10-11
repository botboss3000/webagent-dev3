//! One bounded tool-free System-model call. Recheck authority before admitting work.
use super::*;
pub(super) async fn judge(app: &App, record: &mut Value) -> Result<Verdict> {
    let agent = record["agent_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Missing agent"))?
        .to_owned();
    let pinned: config::IncomingMessage = serde_json::from_value(record["settings"].clone())?;
    let settings_revision = record["settings_revision"].as_u64().unwrap_or(0);
    let (current_settings, current_revision) = config::snapshot(&app.store, &agent).await?;
    if current_settings != pinned || current_revision != settings_revision || app.shutoff().await? {
        return Err(anyhow!("Manager settings or authority changed"));
    }
    let proxy = json!({"agent_id":agent,"selection_plan":record["selection_plan"],
        "failover_state":record["failover_state"]});
    let candidate = app
        .run_candidate(&proxy)
        .await?
        .ok_or_else(|| anyhow!("System model required"))?;
    // A role removal/reassignment withdraws the pending gate even if its connection survives.
    let live = app
        .store
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
    let tokens = app.tokens_for(&selection, &proxy).await?;
    let cancel = app.stop.child_token();
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    // Deltas are private and discarded; a judge cannot publish a working message.
    let drain = tokio::spawn(async move { while rx.recv().await.is_some() {} });
    let call_id = format!("manager:{}", record["id"].as_str().unwrap_or(""));
    let instructions = gates::incoming_message::instructions(&pinned.criteria);
    let call = async {
        let selection = &selection;
        let tokens = &tokens;
        let cancel = cancel.clone();
        let ready = crate::provider::CallDraft::new(
            selection,
            gates::incoming_message::input(&record["message"]),
            Vec::new(),
            &call_id,
            &instructions,
        )
        .validate()?
        .bind_credentials(tokens, &cancel)?;
        app.provider.dispatch(ready, tx, cancel).await
    };
    let result = tokio::time::timeout(Duration::from_secs(40), call).await;
    cancel.cancel();
    drain.abort();
    let response = result.map_err(|_| anyhow!("Manager review timed out"))??;
    usage::record(record, &response);
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
    // A late result never grants access after revocation or a criteria edit.
    let (current_settings, current_revision) = config::snapshot(&app.store, &agent).await?;
    if app.shutoff().await? || current_settings != pinned || current_revision != settings_revision {
        return Err(anyhow!("Manager settings or authority changed"));
    }
    app.run_candidate(&proxy).await?;
    let current = app
        .store
        .model_catalog()
        .await?
        .resolve_role(&agent, model_selection::ModelRole::System)?;
    if current.assignment_revision != live.assignment_revision
        || current.candidates != live.candidates
    {
        return Err(anyhow!("Manager model assignment changed"));
    }
    if verdict == Verdict::Acceptable {
        app.run_candidate(&record["run"]).await?;
    }
    Ok(verdict)
}
