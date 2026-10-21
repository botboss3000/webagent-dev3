//! Engine-owned retry/backup/recovery state using the validated Models failover contract.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use crate::{now,provider,context_accounting,context_assembly};
use crate::runtime::{Runtime,config,ports::*};
use webagent_models::model_selection;
pub fn system_dispatch_run(run: &Value) -> Result<Value> {
    let plan:model_selection::SelectionPlan=serde_json::from_value(run["system_selection_plan"].clone())
        .map_err(|_|anyhow!("System model required for context compaction; configure it before starting a new run"))?;
    if plan.role != model_selection::ModelRole::System {return Err(anyhow!("Invalid System role plan"));}
    let state:model_selection::FailoverState=serde_json::from_value(run["system_failover_state"].clone())?;
    let candidate=plan.candidates.get(state.candidate_index).ok_or_else(||anyhow!("System candidates exhausted"))?;
    Ok(json!({"id":run["id"],"session_id":run["session_id"],"requester_user_id":run["requester_user_id"],"agent_id":run["agent_id"],"selection_plan":plan,"failover_state":state,
        "uncertain":run["uncertain"],"provider_selection":provider::ProviderSelection { limits:candidate.limits.clone(),
            provider:candidate.transport.provider_id(),model:candidate.upstream_model.clone()}.value()}))
}
pub fn model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    use model_selection::{FailureKind, FailoverState, NextAction, SelectionPlan};
    let failure = error.downcast_ref::<provider::ProviderFailure>();
    let kind = failure.map(|e| e.kind).unwrap_or(FailureKind::Protocol);
    let mut state: FailoverState = run.get("failover_state").filter(|v| !v.is_null())
        .map(|v| serde_json::from_value(v.clone())).transpose()?.unwrap_or_default();
    state.response_started |= failure.is_some_and(|e| e.response_started);
    state.uncertain_tool_effect |= run["uncertain"] == true;
    let action = if let Some(value) = run.get("selection_plan").filter(|v| !v.is_null()) {
        let plan: SelectionPlan = serde_json::from_value(value.clone())?;
        let action = state.failed(&plan, kind);
        if action == NextAction::Backup {
            run["provider_selection"] = provider::ProviderSelection { limits:plan.candidates[state.candidate_index].limits.clone(),
                provider: plan.candidates[state.candidate_index].transport.provider_id(),
                model: plan.candidates[state.candidate_index].upstream_model.clone(),
            }.value();
            run["attempt"] = json!(0);
        }
        action
    } else if kind == FailureKind::Transient && !state.response_started
        && !state.uncertain_tool_effect && state.retries < 1 {
        state.retries += 1;
        NextAction::Retry
    } else { NextAction::Terminal };
    let resumable = kind == FailureKind::Transient && !state.response_started
        && !state.uncertain_tool_effect;
    // This encrypted marker permits an explicit new retry budget, never an
    // automatic reset. Partial responses and uncertain effects cannot set it.
    run["provider_recovery"] = if resumable && action == NextAction::Terminal {
        json!({"version":1,"safe_to_resume":true})
    } else { Value::Null };
    run["provider_retry_at"] = if action != NextAction::Terminal { json!(now()+2) } else { Value::Null };
    run["failover_state"] = serde_json::to_value(state)?;
    run["status"] = json!(match action { NextAction::Retry | NextAction::Backup => "queued", NextAction::Terminal if resumable => "stopped", NextAction::Terminal => "failed" });
    run["error"] = json!(match action {
        NextAction::Retry => format!("Transient provider failure; bounded retry: {error}"),
        NextAction::Backup => format!("Primary/backup provider failed; trying the next configured connection: {error}"),
        NextAction::Terminal if resumable => format!("Provider recovery budget exhausted; resume to try again: {error}"),
        NextAction::Terminal => format!("Provider request failed; no safe retry or eligible backup remains: {error}"),
    });
    Ok(())
}
pub fn system_model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    let mut system=system_dispatch_run(run)?;
    model_failure(&mut system,error)?;
    run["system_failover_state"]=system["failover_state"].clone();
    run["provider_recovery"]=system["provider_recovery"].clone();
    if run["provider_recovery"].is_object() { run["provider_recovery"]["system"]=json!(true); }
    run["provider_retry_at"]=system["provider_retry_at"].clone();
    run["status"]=system["status"].clone();
    run["error"]=system["error"].clone();
    Ok(())
}
pub fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "cancelled" | "failed")
}
pub fn validate_system_summary_input(context:&config::Context,selection:&provider::ProviderSelection,input:&[Value],instructions:&str,max_bytes:usize)->Result<()> {
    #[derive(serde::Serialize)]
    struct Envelope<'a>{instructions:&'a str,tools:[Value;0],input:&'a [Value]}
    let envelope=Envelope{instructions,tools:[],input};
    if context_accounting::json_size(&envelope)?>context.ceiling(selection)?.min(max_bytes){
        return Err(context_assembly::Failure::new("system_summary_input_unavailable","summary_input").into());
    }
    Ok(())
}
impl<S,C:config::SchemaCatalog,B,A,H:ChatAuthority<S>,M,X:ModelDiagnostics<S>> Runtime<S,C,B,A,H,M,X> {
pub fn trusted_model_failure(&self,run:&mut Value,error:&anyhow::Error,system:bool)->Result<()> {trusted_model_failure(&self.native,self.store.records(),run,error,system)}
}

pub fn trusted_model_failure<S,D:ModelDiagnostics<S>>(observer:&D,store:&S,run:&mut Value,error:&anyhow::Error,system:bool)->Result<()> {
    let evidence=if observer.provider_enabled(store) {
        let selected=if system {system_dispatch_run(run).ok().map(|r|r["provider_selection"].clone())}else{Some(run["provider_selection"].clone())};
        Some(json!({"id":run["id"],"session_id":run["session_id"],"provider_selection":selected,"candidate_index":if system {run["system_failover_state"]["candidate_index"].clone()}else{run["failover_state"]["candidate_index"].clone()}}))
    }else{None};
    if system {system_model_failure(run,error)?;}else{model_failure(run,error)?;}
    if let Some(evidence)=evidence {
        let next=if system {run["system_failover_state"]["candidate_index"].as_u64()}else{run["failover_state"]["candidate_index"].as_u64()};
        let action=if next.unwrap_or(0)>evidence["candidate_index"].as_u64().unwrap_or(0){"backup"}else if run["status"]=="queued"{"retry"}else if run["provider_recovery"]["safe_to_resume"]==true{"resume_required"}else{"terminal"};
        observer.failure(store,&evidence,error,action);
    }
    Ok(())
}
