//! Engine-owned retry/backup/recovery state using the validated Models failover contract.
use crate::runtime::{config, ports::*, Runtime};
use crate::{context_accounting, context_assembly, now, provider};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use webagent_models::backup_policy::BackupPolicy;
use webagent_models::model_selection::{self, SelectionPlan};
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
    use model_selection::{FailoverState, FailureKind, NextAction};
    let failure = error.downcast_ref::<provider::ProviderFailure>();
    let kind = failure.map(|e| e.kind).unwrap_or(FailureKind::Protocol);
    let plan: Option<SelectionPlan> = run
        .get("selection_plan")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?;
    // The agent's policy pinned at admission; runs without a plan follow the live app policy.
    let policy = plan
        .as_ref()
        .map_or_else(BackupPolicy::live, SelectionPlan::policy);
    let mut state: FailoverState = run
        .get("failover_state")
        .filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone()))
        .transpose()?
        .unwrap_or_default();
    state.response_started |= failure.is_some_and(|e| e.response_started);
    state.uncertain_tool_effect |= run["uncertain"] == true;
    let mut switched = None;
    let mut action = if let Some(plan) = &plan {
        let from = plan
            .candidates
            .get(state.candidate_index)
            .map(|c| c.upstream_model.clone());
        let action = state.failed(plan, kind, &policy);
        if action == NextAction::Backup {
            switched = Some(json!({"from_model":from,
                "to_model":plan.candidates[state.candidate_index].upstream_model,"reason":kind}));
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
        && state.retries < policy.same_model_retries
    {
        state.retries += 1;
        NextAction::Retry
    } else {
        NextAction::Terminal
    };
    // A provider whose own client is offline (a bridge) took nothing, so it resumes like a
    // temporary error once no alias is left.
    let resumable = (kind == FailureKind::Transient || failure.is_some_and(|e| e.offline))
        && !state.response_started
        && !state.uncertain_tool_effect;
    // Once the retries and aliases run out on a run that is safe to resume, resume it
    // automatically after `auto_resume_after_seconds`, at most `auto_resume_attempts` times per
    // run, with a fresh retry budget. It stays queued, so Stop cancels it like any queued retry.
    let resumes = run["auto_resumes"].as_u64().unwrap_or(0);
    let auto_resume = resumable
        && action == NextAction::Terminal
        && resumes < u64::from(policy.auto_resume_attempts);
    if auto_resume {
        state.retries = 0;
        // A fresh budget includes the per-call attempt limit (`workflow.max_attempts`), as for a
        // backup switch; otherwise the resumed dispatch pauses at once at the attempt limit.
        run["attempt"] = json!(0);
        run["auto_resumes"] = json!(resumes + 1);
        action = NextAction::Retry;
    }
    let wait = if auto_resume {
        policy.auto_resume_after_seconds
    } else {
        retry_delay(&policy, &state, failure.and_then(|f| f.retry_after))
    };
    let detail: String = error.to_string().chars().take(300).collect();
    // Kept for the operator's "model unreachable" row if a later limit stops this call.
    run["last_model_error"] = json!(detail);
    if action != NextAction::Terminal {
        let mut item = json!({"type":"retry_scheduled","attempt":state.retries,
            "of":policy.same_model_retries,"wait_seconds":wait,"error":detail,
            "backup":action == NextAction::Backup,"auto_resume":auto_resume});
        if let Some(switch) = switched.as_ref().and_then(Value::as_object) {
            item.as_object_mut().unwrap().extend(switch.clone());
        }
        retry_activity(run, item);
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
    // A switch to the next alias is silent: the activity group's retry row records it.
    run["error"] = match action {
        NextAction::Retry if auto_resume => json!(format!(
            "Provider still failing; resuming automatically in {wait} s: {error}"
        )),
        NextAction::Retry => json!(format!(
            "Transient provider failure; bounded retry: {error}"
        )),
        NextAction::Backup | NextAction::Terminal => Value::Null,
    };
    if action == NextAction::Terminal {
        model_unreachable(
            run,
            if resumable {
                "retries, backup models and automatic resumes used up"
            } else {
                "no safe retry or eligible backup model remains"
            },
            kind,
        );
    }
    Ok(())
}

/// The chat notice for a model that cannot be reached. It names no model, provider or
/// recovery detail; those go to the operator row below.
pub const MODEL_UNREACHABLE: &str = "The model is unreachable";

/// The provider rejected the request because it no longer fits the model's context window.
pub const CONTEXT_LIMIT: &str = "The conversation no longer fits the model's context window";
/// A context summary outgrew the summarizer's window before it could run.
pub const SUMMARY_CONTEXT_LIMIT: &str = "The context summary exceeded the model's context \
     window; lower the summary input limit or use the same-model summarizer";
/// The provider's usage limit for this account was reached.
pub const PROVIDER_QUOTA: &str =
    "The provider's usage limit for this account was reached; try again after it resets";
/// The provider rejected the stored credentials.
pub const PROVIDER_CREDENTIALS: &str = "The provider rejected the stored credentials";
/// The model is not available from its provider.
pub const MODEL_UNAVAILABLE: &str = "The model is not available from its provider";

/// The notice's next step, naming the agent administrator's contact email when one is set.
pub fn model_unreachable_guidance(contact: Option<&str>) -> String {
    match contact {
        Some(email) => {
            format!("Use a different session to resume, or contact the administrator at {email}.")
        }
        None => "Use a different session to resume, or contact the administrator.".into(),
    }
}

/// Chat text for a run stopped on a setup or internal error only an administrator can fix.
pub const NEEDS_ADMIN: &str = "This agent needs its administrator's attention";

/// Run errors a chat user can act on, or that name nothing internal, shown as written.
const USER_ERRORS: [&str; 22] = [
    MODEL_UNREACHABLE,
    CONTEXT_LIMIT,
    SUMMARY_CONTEXT_LIMIT,
    PROVIDER_QUOTA,
    PROVIDER_CREDENTIALS,
    MODEL_UNAVAILABLE,
    "Cancelled by user",
    "Stopped by global shutoff",
    "Approval withdrawn",
    "Parallel write outcome",
    "Background summary interrupted",
    "This run reached its limit",
    "The model used all",
    "Access changed",
    "Paused for app update",
    "Recovered after interruption",
    "Storage is full",
    "Tool outcome",
    "Build result awaits",
    "Read tool interrupted",
    "Provider request interrupted",
    "Context summary interrupted",
];

/// What every chat reader sees of a run error. Setup, policy and internal errors become
/// `NEEDS_ADMIN`; agent operators read the full text in the notice's `detail`. A retry keeps
/// its count and drops the cause.
pub fn public_error(error: &str) -> &str {
    if error.starts_with("Retrying (") {
        return error.find("): ").map_or(error, |end| &error[..=end]);
    }
    if error.starts_with("Transient provider failure") {
        return "The model did not answer; retrying";
    }
    if USER_ERRORS.iter().any(|known| error.starts_with(known)) {
        error
    } else {
        NEEDS_ADMIN
    }
}

/// Whether a run error sends the chat user to the agent administrator.
pub fn needs_admin(error: &str) -> bool {
    matches!(error, MODEL_UNREACHABLE | PROVIDER_CREDENTIALS) || public_error(error) == NEEDS_ADMIN
}

/// Whether a stopped call carries the operator-only model/provider detail row.
pub fn unreachable_class(error: &str) -> bool {
    matches!(
        error,
        MODEL_UNREACHABLE
            | CONTEXT_LIMIT
            | SUMMARY_CONTEXT_LIMIT
            | PROVIDER_QUOTA
            | PROVIDER_CREDENTIALS
            | MODEL_UNAVAILABLE
    )
}

/// The step that sends a chat user to the agent administrator, naming the contact email
/// (Identity → Profile) when one is set.
pub fn contact_administrator(contact: Option<&str>) -> String {
    match contact {
        Some(email) => format!("Contact the administrator at {email}."),
        None => "Contact the administrator.".into(),
    }
}

/// The agent administrator's contact email (Identity → Profile), if one is set.
pub async fn admin_contact(
    store: &impl crate::ports::AgentMetadata,
    agent: &str,
) -> Result<Option<String>> {
    use crate::ports::AgentMetadata;
    use webagent_access::storage::RecordCodec;
    let view = store.for_agent(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(view.agent_pool())
        .await?;
    let metadata = view.unseal(&blob)?;
    Ok(metadata["admin_contact_email"]
        .as_str()
        .filter(|email| !email.is_empty())
        .map(str::to_owned))
}

/// Stop a call whose model could not be reached: the chat shows only `MODEL_UNREACHABLE`,
/// and an activity row (run logs, administrators only) records the model, provider, what
/// recovery was tried and the last error.
pub fn model_unreachable(run: &mut Value, stopped_by: &str, kind: model_selection::FailureKind) {
    let plan: Option<SelectionPlan> = serde_json::from_value(run["selection_plan"].clone()).ok();
    let policy = plan
        .as_ref()
        .map_or_else(BackupPolicy::live, SelectionPlan::policy);
    let backups = plan
        .as_ref()
        .map_or(0, |plan| plan.candidates.len().saturating_sub(1));
    let state = &run["failover_state"];
    let model = run["provider_selection"]["model"]
        .as_str()
        .unwrap_or("unknown");
    let provider = run["provider_selection"]["provider"]
        .as_str()
        .unwrap_or("unknown");
    let detail = format!(
        "Model {model} on provider {provider}. Recovery tried: {retries} of {retry_limit} \
         same-model retries, backup models {used} of {backups}, automatic resumes {resumes} of \
         {resume_limit}. Stopped: {stopped_by}. Last error: {error}",
        retries = state["retries"].as_u64().unwrap_or(0),
        retry_limit = policy.same_model_retries,
        used = state["candidate_index"].as_u64().unwrap_or(0),
        resumes = run["auto_resumes"].as_u64().unwrap_or(0),
        resume_limit = policy.auto_resume_attempts,
        error = run["last_model_error"].as_str().unwrap_or("none recorded"),
    );
    let item = json!({"type":"model_unreachable","model":model,"provider":provider,
        "detail":&detail});
    // Name the actual class of failure the chat user can act on; the operator row keeps the
    // model, provider and recovery detail below.
    run["error"] = json!(match kind {
        model_selection::FailureKind::ContextLimit
            if run["phase"].as_str() == Some("compacting") =>
        {
            SUMMARY_CONTEXT_LIMIT
        }
        model_selection::FailureKind::ContextLimit => CONTEXT_LIMIT,
        model_selection::FailureKind::QuotaExhausted => PROVIDER_QUOTA,
        model_selection::FailureKind::InvalidCredentials => PROVIDER_CREDENTIALS,
        model_selection::FailureKind::ModelUnavailable => MODEL_UNAVAILABLE,
        _ => MODEL_UNREACHABLE,
    });
    // The chat notice shows this to agent operators only.
    run["unreachable_detail"] = json!(detail);
    retry_activity(run, item);
}
pub use webagent_models::backup_policy::response_start_seconds as response_start;

/// BACKUP STICK. A session stays on the alias that last answered for it, so that model's
/// provider cache keeps serving: `model_backup:{sid}` holds `{from, to}` (entry ids of the
/// plan's first model and that alias). The next major reorder, when no cache is left to lose,
/// tries the first model again.
pub fn stick_key(session: &str) -> String {
    format!("model_backup:{session}")
}

/// Start a run on the alias its session's stick names, when the stick fits the run's plan.
pub fn apply_stick(run: &mut Value, stick: &Value) -> bool {
    let Some(plan) = plan_of(run) else {
        return false;
    };
    let at = |entry: &Value| {
        plan.candidates
            .iter()
            .position(|c| entry.as_str() == Some(c.entry_id.as_str()))
    };
    match at(&stick["to"]) {
        Some(index)
            if index > 0
                && at(&stick["from"]) == Some(0)
                && run["failover_state"]["candidate_index"] == 0 =>
        {
            move_to(run, &plan, index, "backup_in_use");
            run["backup_stick"] = json!(index);
            true
        }
        _ => false,
    }
}

/// At a major reorder: move a run on an alias back to the plan's first model, once per run.
/// The caller clears the session's stick.
pub fn first_model_again(run: &mut Value) -> bool {
    let on_alias = run["failover_state"]["candidate_index"]
        .as_u64()
        .is_some_and(|index| index > 0);
    let Some(plan) = plan_of(run).filter(|_| on_alias && run["first_model_retried"] != true) else {
        return false;
    };
    move_to(run, &plan, 0, "first_model_again");
    run["first_model_retried"] = json!(true);
    run["backup_stick"] = Value::Null;
    true
}

/// After a reply: the stick to save when an alias answered and the session's stick is older.
pub fn stick_to_save(run: &mut Value) -> Option<Value> {
    let index = run["failover_state"]["candidate_index"]
        .as_u64()
        .filter(|index| *index > 0 && run["backup_stick"] != json!(index))?;
    let candidates = &run["selection_plan"]["candidates"];
    let stick =
        json!({"from":candidates[0]["entry_id"],"to":candidates[index as usize]["entry_id"]});
    run["backup_stick"] = json!(index);
    Some(stick)
}

fn plan_of(run: &Value) -> Option<SelectionPlan> {
    serde_json::from_value(run["selection_plan"].clone()).ok()
}

/// Point the run at candidate `index` with a fresh retry budget and an activity row.
fn move_to(run: &mut Value, plan: &SelectionPlan, index: usize, reason: &str) {
    let from = run["failover_state"]["candidate_index"]
        .as_u64()
        .and_then(|current| plan.candidates.get(current as usize))
        .map(|c| c.upstream_model.clone());
    let to = &plan.candidates[index];
    run["failover_state"]["candidate_index"] = json!(index);
    run["failover_state"]["retries"] = json!(0);
    run["provider_selection"] = provider::ProviderSelection {
        provider: to.transport.provider_id(),
        model: to.upstream_model.clone(),
    }
    .value();
    run["attempt"] = json!(0);
    retry_activity(
        run,
        json!({"type":"retry_scheduled","wait_seconds":0,"backup":true,"auto_resume":false,
            "from_model":from,"to_model":to.upstream_model,"reason":reason}),
    );
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

/// Seconds before the next attempt, from the run's backup policy. An alias starts after the
/// first delay.
fn retry_delay(
    policy: &BackupPolicy,
    state: &model_selection::FailoverState,
    retry_after: Option<u64>,
) -> u64 {
    let entropy = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_nanos()))
        .unwrap_or(0);
    policy.delay(state.retries, retry_after, entropy)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Chat users read the action they can take; setup and internal causes stay with operators.
    #[test]
    fn public_error_hides_internal_causes() {
        assert_eq!(
            public_error("Invalid workflow policy; administrator review required"),
            NEEDS_ADMIN
        );
        assert_eq!(
            public_error("Paused after 3 failed attempts: socket closed"),
            NEEDS_ADMIN
        );
        assert_eq!(public_error(MODEL_UNREACHABLE), MODEL_UNREACHABLE);
        assert_eq!(
            public_error("This run reached its limit of 40 steps"),
            "This run reached its limit of 40 steps"
        );
        assert_eq!(
            public_error("Retrying (1 of 3 attempts failed): worker socket closed"),
            "Retrying (1 of 3 attempts failed)"
        );
        assert_eq!(
            public_error("Transient provider failure; bounded retry: HTTP 503"),
            "The model did not answer; retrying"
        );
        assert_eq!(public_error("Cancelled by user"), "Cancelled by user");
        assert!(!needs_admin("Cancelled by user"));
        assert!(needs_admin(MODEL_UNREACHABLE));
        assert!(!needs_admin("Access changed; run stopped"));
        assert_eq!(
            contact_administrator(Some("ops@example.com")),
            "Contact the administrator at ops@example.com."
        );
        assert_eq!(contact_administrator(None), "Contact the administrator.");
    }
}
