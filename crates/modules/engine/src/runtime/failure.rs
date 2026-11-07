//! Engine-owned retry/backup/recovery state using the validated Models failover contract.
use crate::runtime::{config, ports::*, Runtime};
use crate::{context_accounting, context_assembly, now, provider};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use webagent_models::model_selection;
pub fn system_dispatch_run(run: &Value) -> Result<Value> {
    let plan: model_selection::SelectionPlan =
        serde_json::from_value(run["system_selection_plan"].clone()).map_err(|_| {
            anyhow!(
            "System model required for context compaction; configure it before starting a new run"
        )
        })?;
    if plan.role != model_selection::ModelRole::System {
        return Err(anyhow!("Invalid System role plan"));
    }
    let state: model_selection::FailoverState =
        serde_json::from_value(run["system_failover_state"].clone())?;
    let candidate = plan
        .candidates
        .get(state.candidate_index)
        .ok_or_else(|| anyhow!("System candidates exhausted"))?;
    Ok(
        json!({"id":run["id"],"session_id":run["session_id"],"requester_user_id":run["requester_user_id"],"agent_id":run["agent_id"],"selection_plan":plan,"failover_state":state,
        "uncertain":run["uncertain"],"auto_resumes":run["auto_resumes"],"retry_rows":run["retry_rows"],
        "_activity_items":run["_activity_items"],"provider_selection":provider::ProviderSelection {
            provider:candidate.transport.provider_id(),model:candidate.upstream_model.clone()}.value()}),
    )
}
pub fn model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    use model_selection::{FailoverState, FailureKind, NextAction, SelectionPlan};
    let failure = error.downcast_ref::<provider::ProviderFailure>();
    let kind = failure.map(|e| e.kind).unwrap_or(FailureKind::Protocol);
    let retry = webagent_models::settings::Settings::live().retry;
    let mut state: FailoverState = run
        .get("failover_state")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?
        .unwrap_or_default();
    state.response_started |= failure.is_some_and(|e| e.response_started);
    state.uncertain_tool_effect |= run["uncertain"] == true;
    let mut action = if let Some(value) = run.get("selection_plan").filter(|v| !v.is_null()) {
        let plan: SelectionPlan = serde_json::from_value(value.clone())?;
        let action = state.failed_with(&plan, kind, retry.same_model_retries);
        if action == NextAction::Backup {
            run["provider_selection"] = provider::ProviderSelection {
                provider: plan.candidates[state.candidate_index]
                    .transport
                    .provider_id(),
                model: plan.candidates[state.candidate_index]
                    .upstream_model
                    .clone(),
            }
            .value();
            run["attempt"] = json!(0);
        }
        action
    } else if kind == FailureKind::Transient
        && !state.response_started
        && !state.uncertain_tool_effect
        && state.retries < retry.same_model_retries
    {
        state.retries += 1;
        NextAction::Retry
    } else {
        NextAction::Terminal
    };
    let resumable =
        kind == FailureKind::Transient && !state.response_started && !state.uncertain_tool_effect;
    // Once the retries run out on a run that is safe to resume, resume it automatically after
    // `retry.auto_resume_after_seconds`, at most `auto_resume_attempts` times per run, with a fresh
    // retry budget. It stays queued, so Stop cancels it like any queued retry.
    let resumes = run["auto_resumes"].as_u64().unwrap_or(0);
    let auto_resume = resumable
        && action == NextAction::Terminal
        && resumes < u64::from(retry.auto_resume_attempts);
    if auto_resume {
        state.retries = 0;
        // A fresh budget includes the per-call attempt limit (`workflow.max_attempts`), as for a
        // backup switch; otherwise the resumed dispatch pauses at once with "Run limit reached".
        run["attempt"] = json!(0);
        run["auto_resumes"] = json!(resumes + 1);
        action = NextAction::Retry;
    }
    let wait = if auto_resume {
        retry.auto_resume_after_seconds
    } else {
        retry_delay(&retry, &state, failure.and_then(|f| f.retry_after))
    };
    if action != NextAction::Terminal {
        let detail: String = error.to_string().chars().take(300).collect();
        retry_activity(
            run,
            json!({"type":"retry_scheduled","attempt":state.retries,"of":retry.same_model_retries,
                "wait_seconds":wait,"error":detail,"backup":action == NextAction::Backup,
                "auto_resume":auto_resume}),
        );
    }
    // This encrypted marker permits an explicit new retry budget, never an
    // automatic reset. Partial responses and uncertain effects cannot set it.
    run["provider_recovery"] = if resumable && action == NextAction::Terminal {
        json!({"version":1,"safe_to_resume":true})
    } else {
        Value::Null
    };
    run["provider_retry_at"] = if action != NextAction::Terminal {
        json!(now() + wait)
    } else {
        Value::Null
    };
    run["failover_state"] = serde_json::to_value(state)?;
    run["status"] = json!(match action {
        NextAction::Retry | NextAction::Backup => "queued",
        NextAction::Terminal if resumable => "stopped",
        NextAction::Terminal => "failed",
    });
    run["error"] = json!(match action {
        NextAction::Retry if auto_resume => {
            format!("Provider still failing; resuming automatically in {wait} s: {error}")
        }
        NextAction::Retry => format!("Transient provider failure; bounded retry: {error}"),
        NextAction::Backup => format!(
            "Primary/backup provider failed; trying the next configured connection: {error}"
        ),
        NextAction::Terminal if resumable =>
            format!("Provider recovery budget exhausted; resume to try again: {error}"),
        NextAction::Terminal =>
            format!("Provider request failed; no safe retry or eligible backup remains: {error}"),
    });
    Ok(())
}
/// Record an activity-only row for a scheduled retry; context assembly skips `retry:` steps.
fn retry_activity(run: &mut Value, item: Value) {
    let count = run["retry_rows"].as_u64().unwrap_or(0) + 1;
    run["retry_rows"] = json!(count);
    let entry = json!({"step":format!("retry:{count}"),"item":item});
    match run["_activity_items"].as_array_mut() {
        Some(items) => items.push(entry),
        None => run["_activity_items"] = json!([entry]),
    }
}

/// Seconds before the next attempt, from the live retry settings (`models-runtime.json`).
/// A backup starts after the first delay.
fn retry_delay(
    retry: &webagent_models::settings::Retry,
    state: &model_selection::FailoverState,
    retry_after: Option<u64>,
) -> u64 {
    let entropy = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    retry.delay(state.retries, retry_after, entropy)
}

pub fn system_model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    let mut system = system_dispatch_run(run)?;
    model_failure(&mut system, error)?;
    run["system_failover_state"] = system["failover_state"].clone();
    for field in ["auto_resumes", "retry_rows", "_activity_items"] {
        run[field] = system[field].clone();
    }
    run["provider_recovery"] = system["provider_recovery"].clone();
    if run["provider_recovery"].is_object() {
        run["provider_recovery"]["system"] = json!(true);
    }
    run["provider_retry_at"] = system["provider_retry_at"].clone();
    run["status"] = system["status"].clone();
    run["error"] = system["error"].clone();
    Ok(())
}
pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "cancelled" | "failed")
}
pub fn validate_system_summary_input(
    context: &config::Context,
    selection: &provider::ProviderSelection,
    input: &[Value],
    instructions: &str,
    max_bytes: usize,
) -> Result<()> {
    #[derive(serde::Serialize)]
    struct Envelope<'a> {
        instructions: &'a str,
        tools: [Value; 0],
        input: &'a [Value],
    }
    let envelope = Envelope {
        instructions,
        tools: [],
        input,
    };
    if context_accounting::json_size(&envelope)? > context.ceiling(selection)?.min(max_bytes) {
        return Err(context_assembly::Failure::new(
            "system_summary_input_unavailable",
            "summary_input",
        )
        .into());
    }
    Ok(())
}
impl<S, C: config::SchemaCatalog, B, A, H: ChatAuthority<S>, M, X: ModelDiagnostics<S>>
    Runtime<S, C, B, A, H, M, X>
{
    pub fn trusted_model_failure(
        &self,
        run: &mut Value,
        error: &anyhow::Error,
        system: bool,
    ) -> Result<()> {
        trusted_model_failure(&self.native, self.store.records(), run, error, system)
    }
}

pub fn trusted_model_failure<S, D: ModelDiagnostics<S>>(
    observer: &D,
    store: &S,
    run: &mut Value,
    error: &anyhow::Error,
    system: bool,
) -> Result<()> {
    let evidence = if observer.provider_enabled(store) {
        let selected = if system {
            system_dispatch_run(run)
                .ok()
                .map(|r| r["provider_selection"].clone())
        } else {
            Some(run["provider_selection"].clone())
        };
        Some(
            json!({"id":run["id"],"session_id":run["session_id"],"provider_selection":selected,"candidate_index":if system {run["system_failover_state"]["candidate_index"].clone()}else{run["failover_state"]["candidate_index"].clone()}}),
        )
    } else {
        None
    };
    if system {
        system_model_failure(run, error)?;
    } else {
        model_failure(run, error)?;
    }
    if let Some(evidence) = evidence {
        let next = if system {
            run["system_failover_state"]["candidate_index"].as_u64()
        } else {
            run["failover_state"]["candidate_index"].as_u64()
        };
        let action = if next.unwrap_or(0) > evidence["candidate_index"].as_u64().unwrap_or(0) {
            "backup"
        } else if run["status"] == "queued" {
            "retry"
        } else if run["provider_recovery"]["safe_to_resume"] == true {
            "resume_required"
        } else {
            "terminal"
        };
        observer.failure(store, &evidence, error, action);
    }
    Ok(())
}
