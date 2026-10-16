//! One bounded tool-free System-model call. Recheck authority before admitting work.
use super::*;
// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Manager verdict caller prepares pure projection then rechecks participation, role and settings before network admission. Config: config/prompts.json -> manager_incoming_message; enable/revision are agent metadata.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled"]},{"path":"plugins/engine/provider-projection/component.json","pointers":["/interface_version","/implementation_version","/limits"]},{"path":"config/prompts.json","pointers":["/manager_incoming_message"]}],"opportunities":[]}}
/// Obtain one tool-free verdict from the pinned System role without publishing model deltas.
/// Projection precedes the dispatch gate; current participation, settings, assignment and component revisions fence its first network poll.
pub(super) async fn judge(app: &App, record: &mut Value) -> Result<Verdict> {
    participation_authorized(app, record).await?;
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
    let proxy = system_proxy(app, record);
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
        // Complete pure projection before the final admission fence and network poll.
        let engine_revision = crate::engine_components::catalog()?.revision.clone();
                    let engine_controls = crate::engine_components::controls(&app.store.root)?.revision;
        let prepared = crate::provider::CallDraft::new(
            selection,
            gates::incoming_message::input(&record["message"]),
            Vec::new(),
            &call_id,
            &instructions,
        ).prepare(&app.store.root, &cancel).await?;
        let dispatch_guard = app.gate.lock().await;
        participation_authorized(app, record).await?;
        let current_candidate = app.run_candidate(&proxy).await?
            .ok_or_else(|| anyhow!("System model required"))?;
        let current_selection = provider::ProviderSelection {
            limits: current_candidate.limits.clone(),
            provider: current_candidate.transport.provider_id(),
            model: current_candidate.upstream_model.clone(),
        };
        let (current_settings, current_revision) = config::snapshot(&app.store, &agent).await?;
        let current_role = app.store.model_catalog().await?
            .resolve_role(&agent, model_selection::ModelRole::System)?;
        if app.shutoff().await? || current_settings != pinned
            || current_revision != settings_revision || current_selection != *selection
            || current_candidate.entry_id != candidate.entry_id
            || current_role.assignment_revision != live.assignment_revision
            || current_role.candidates != live.candidates {
            return Err(anyhow!("Manager settings or authority changed"));
        }
        if cancel.is_cancelled()
                        || crate::engine_components::catalog()?.revision != engine_revision
                        || crate::engine_components::controls(&app.store.root)?.revision != engine_controls
                        || !crate::engine_components::component_available(
                            &app.store.root, crate::engine_components::PROJECTION)
                    {
                        return Err(anyhow!("Provider component or authority changed"));
                    }
        // Poll the actual transport once while authority writers remain fenced.
                    let mut completion = Box::pin(async {
                        let ready = prepared.bind_credentials(tokens, &cancel)?;
                        app.provider.dispatch(ready, tx, cancel).await
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
    let result = tokio::time::timeout(Duration::from_secs(40),
        app.session_operation(record["session_id"].as_str().unwrap_or(""),cancel.clone(),call)).await;
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
    participation_authorized(app, record).await?;
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
