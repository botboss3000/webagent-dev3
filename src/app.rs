// Shared application state and cross-boundary helpers used by API and loop.
const DEFAULT_AGENT_MODEL: &str = "gpt-6-luna";

#[derive(Clone)]
struct App {
    runtime: Arc<RwLock<loop_runtime::Config>>,
    runtime_path: PathBuf,
    compaction_config_path: PathBuf,
    store: Store,
    // Authentication actor remains distinct from canonical conversation storage.
    chat_actor: Option<String>,
    chat_access: Option<shared_chat::SessionAccess>,
    abilities: Arc<abilities::Catalog>,
    gate: Arc<Mutex<()>>,
    provider: provider::Provider,
    refresh: Arc<Mutex<()>>,
    login: Arc<RwLock<Option<Value>>>,
    active: Arc<Mutex<HashMap<String, CancellationToken>>>,
    slots: Arc<Semaphore>,
    ready: Arc<AtomicBool>,
    stop: CancellationToken,
    login_attempts: Arc<Mutex<Vec<u64>>>,
    control_dir: PathBuf,
    tool_socket: PathBuf,
    build_socket: PathBuf,
    promotion_socket: PathBuf,
    secure: bool,
    chat_ui_config_path: PathBuf,
    message_api_token_hash: Option<[u8; 32]>,
    browser_login_bridge: Option<browser_login::Bridge>,
}
#[derive(Debug)]
struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
impl From<anyhow::Error> for ApiError {
    fn from(error: anyhow::Error) -> Self {
        if error.downcast_ref::<crate::store::session::RecycledSession>().is_some() {
            return ApiError(StatusCode::NOT_FOUND,"Session not found".into());
        }
        tracing::error!("Request storage or execution operation failed");
        diagnostics::backend_error();
        Self(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Operation failed; check development logs".into(),
        )
    }
}
type Api = std::result::Result<Json<Value>, ApiError>;
fn bad(s: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, s.into())
}
fn conflict(s: &str) -> ApiError {
    ApiError(StatusCode::CONFLICT, s.into())
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
fn uuid() -> String {
    Uuid::new_v4().to_string()
}
use webagent_configuration::digest;
fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}:{id}")
}
fn terminal(status: &str) -> bool {
    matches!(status, "completed" | "cancelled" | "failed")
}
fn status(run: &Value) -> &str {
    run["status"].as_str().unwrap_or("failed")
}
// CONTEXT-PUBLIC-SUMMARY: publish measured numeric totals only, never arbitrary
// persisted report fields, overlays, schema descriptions or transcript content.
fn public_context_summary(run: &Value) -> Value {
    let report = &run["context_assembly_report"];
    if report["version"] != 1 || report["scope"] != "logical_call_envelope" { return Value::Null; }
    let fields = ["envelope_bytes", "max_bytes", "history_json_bytes",
        "instruction_json_bytes", "schemas_json_bytes", "turn", "call"];
    let mut summary=serde_json::Map::new();
    for field in fields {
        let Some(value)=report[field].as_u64() else {return Value::Null;};
        summary.insert(field.into(),json!(value));
    }
    summary.insert("accounting".into(),json!("serialized_json_bytes_not_provider_tokens"));
    Value::Object(summary)
}
fn bounded_model_authorization_failure_stage(error: &anyhow::Error) -> &'static str {
    if error.downcast_ref::<serde_json::Error>().is_some()
        || error.chain().any(|cause| cause.is::<serde_json::Error>())
    {
        return "plan_decode";
    }
    if error.downcast_ref::<sqlx::Error>().is_some()
        || error.chain().any(|cause| cause.is::<sqlx::Error>())
        || error.downcast_ref::<std::io::Error>().is_some()
        || error.chain().any(|cause| cause.is::<std::io::Error>())
    {
        return "storage";
    }
    const KNOWN: &[(&str, &str)] = &[
        ("Legacy run has no pinned role; start a new message after configuring Standard", "plan_missing"),
        ("Invalid pinned model plan identity", "plan_identity"),
        ("Candidate missing", "candidate_missing"),
        ("Selection candidates exhausted", "candidate_missing"),
        ("Model entry removed", "candidate_missing"),
        ("Model connection removed", "candidate_missing"),
        ("Pinned model is no longer available to this caller", "model_grant"),
        ("Model grant revoked", "model_grant"),
        ("Candidate grant revoked", "candidate_grant"),
        ("Pinned model candidate is no longer authorized", "candidate_grant"),
        ("Model candidate is not authorized", "candidate_grant"),
        ("Platform provenance removed", "platform"),
        ("Platform provenance changed", "platform"),
        ("Platform provenance missing from plan", "platform"),
        ("Published model lacks role capability", "platform"),
        ("Selected model connection is unavailable", "candidate_grant"),
        ("Agent model roster required", "candidate_grant"),
        ("Invalid agent model roster", "candidate_grant"),
        ("Invalid or duplicate role backup candidates", "candidate_grant"),
        ("Standard model required", "model_grant"),
        ("Premium model required", "model_grant"),
        ("Image In model required", "model_grant"),
        ("Image Out model required", "model_grant"),
        ("System model required", "model_grant"),
        ("Voice model required", "model_grant"),
        ("Decision model required", "model_grant"),
        ("BYO model outside tier", "model_grant"),
        ("Adopted platform transport changed", "platform"),
        ("Selected model is outside agent roster", "candidate_grant"),
        ("Role entry missing", "candidate_grant"),
        ("Model connection missing", "candidate_grant"),
        ("Role model is incompatible or not authorized", "candidate_grant"),
        ("Account not found", "account"),
        ("Account identity conflict", "account"),
        ("Invalid access identity", "account"),
        ("Guest access is not enabled", "account"),
        ("Entitlement tier not found", "tier"),
        ("Tier identity conflict", "tier"),
        ("Invalid entitlement tier", "tier"),
        ("Invalid entitlement grant", "tier"),
        ("Membership identity or revision conflict", "membership"),
        ("Membership capability level is missing", "membership"),
        ("Agent directory entry missing", "agent_owner"),
        ("Agent owner conflict", "agent_owner"),
        ("Invalid agent metadata", "agent_owner"),
    ];
    for cause in error.chain() {
        // Compare only exact internal messages; never return or log message text.
        let message = cause.to_string();
        if let Some((_, stage)) = KNOWN.iter().find(|(known, _)| *known == message.as_str()) {
            return stage;
        }
    }
    "unknown"
}

// Publish model identity at the pinned failover cursor, never credentials.
fn public_run_model(run: &Value) -> Value {
    if run["model"].is_object(){return json!({"entry_id":run["model"]["entry_id"],"model":run["model"]["model"]});}
    let index=run["failover_state"]["candidate_index"].as_u64().unwrap_or(0) as usize;
    let candidate=run["selection_plan"]["candidates"].as_array().and_then(|items|items.get(index));
    match candidate {
        Some(value)=>json!({"entry_id":value["entry_id"],"model":value["upstream_model"]}),
        None=>json!({"entry_id":null,"model":run["provider_selection"]["model"]}),
    }
}
async fn public_runs(app:&App,views:Vec<Value>)->Result<Vec<Value>>{
    let mut result=Vec::with_capacity(views.len());
    for view in views{
        // Paused views need the owning record's private component marker; old
        // views need its model fields. Read through Store and project only safe fields.
        let has_retrieval_marker = view["code_index_retrieval_attempted"].is_boolean()
            && view["code_index_retrieval_status"].as_str().is_some()
            && view["code_index_retrieval_stage"].as_str().is_some();
        if view["status"]=="paused" || view["model"].is_null() || !has_retrieval_marker{
            let run=app.run(view["id"].as_str().ok_or_else(||anyhow!("Run id missing"))?).await?;
            result.push(public_run(&run));
        }else{result.push(public_run(&view));}
    }
    shared_projection::runs(app,&mut result).await?;
    Ok(result)
}
fn public_code_index_retrieval(run: &Value) -> Value {
    let attempted = run["code_index_retrieval_attempted"] == true;
    let status = match run["code_index_retrieval_status"].as_str() {
        Some("not_attempted") if !attempted => "not_attempted",
        Some("attached") => "attached",
        Some("attached_candidates") => "attached_candidates",
        Some("no_matches" | "disabled_or_no_matches") => "no_matches",
        Some("access_changed") => "access_changed",
        Some("timeout") => "timeout",
        Some("unavailable") => "unavailable",
        None if !attempted => "not_attempted",
        _ => "unavailable",
    };
    let stage = if !attempted {
        "not_started"
    } else {
        match run["code_index_retrieval_stage"].as_str() {
            Some("preflight") => "preflight",
            Some("lookup") => "lookup",
            Some("pre_jev") => "pre_jev",
            Some("jev_parse") => "jev_parse",
            Some("snippet") => "snippet",
            Some("authorization") => "authorization",
            Some("complete") => "complete",
            Some("unknown") => "unknown",
            _ => "unknown",
        }
    };
    json!({"status":status,"stage":stage})
}
// Project existing public run fields and fixed host component-pause metadata without private context.
fn public_run(run: &Value) -> Value {
    json!({"id":run["id"],"session_id":run["session_id"],"status":run["status"],"error":run["error"],
        "component_pause":if run["status"]=="paused"&&run["component_pause"]["version"]==1&&run["component_pause"]["component"].as_str().is_some_and(crate::engine_components::working_component) {json!({"version":1,"component":run["component_pause"]["component"],"reason":"component_unavailable"})}else{Value::Null},
        "code_index_retrieval":public_code_index_retrieval(run),
        "pending":run["pending"],"uncertain":run["uncertain"],"turns":run["turns"],
        "tool_failure":tools::failure::project(&run["latest_tool_failure"]),
        "attachments":run["attachments"].as_array().cloned().unwrap_or_default(),
        "model":public_run_model(run),
        "usage":usage::public(&run["usage"]),
        "compactions":run["compactions"].as_u64().unwrap_or(0),
        "context_controls":{"trigger_tokens":run["context_controls"]["trigger_tokens"].as_u64(),"tail_tokens":run["context_controls"]["tail_tokens"].as_u64(),"characters_per_token":run["context_controls"]["characters_per_token"].as_u64()},
        "mode":run["dispatch_mode"],
        "chat_model_revision":run["chat_model_revision"].as_u64(),
        "context_summary":public_context_summary(run)})
}
// Explicit allowlists prevent arbitrary persisted fields becoming diagnostic text.
fn public_assembly_attempt(event:&Value)->Value {
    let Some(attempt)=event["attempt_id"].as_str().filter(|s|uuid::Uuid::parse_str(s).is_ok()) else{return Value::Null;};
    let source=&event["report"];let mut report=serde_json::Map::new();
    for field in ["max_bytes","instructions_json_bytes","base_content_bytes","guidance_content_bytes","schemas_json_bytes",
        "history_json_bytes","framing_bytes","history_budget_bytes","summary_reserve_bytes","trigger_tokens_estimated",
        "tail_tokens_estimated","characters_per_token"] {
        if let Some(n)=source[field].as_u64(){report.insert(field.into(),json!(n));}
    }
    let outcome=match source["outcome"].as_str(){Some("ready")=>"ready",Some("compaction_required")=>"compaction_required",Some("failed")=>"failed",_=>"unknown"};
    let code=source["failure_code"].as_str().filter(|c|matches!(*c,
        "instruction_limit"|"invalid_contribution"|"overhead_exceeds_budget"|"summary_reserve_unavailable"|"envelope_exceeds_budget"|
        "invalid_measurements"|"no_safe_boundary"|"stale_plan"|"summary_length"|"summary_did_not_shrink"|"summary_missing"|
        "summary_source_limit"|"system_summary_input_unavailable"|"manual_no_safe_boundary"|"assembly_internal")).unwrap_or_default();
    let stage=source["stage"].as_str().filter(|s|matches!(*s,"assembly"|"selection"|"instructions"|"budget"|"compaction_plan"|"summary_input"|"summary_validation")).unwrap_or("unknown");
    report.insert("outcome".into(),json!(outcome));report.insert("failure_code".into(),json!(code));report.insert("stage".into(),json!(stage));
    let mut compaction=serde_json::Map::new();
    for field in ["candidate_boundaries","pending_calls","duplicate_calls","unmatched_calls_or_results","source_limit_rejections","tail_byte_rejections","tail_character_rejections"] {
        if let Some(n)=source["compaction"][field].as_u64(){compaction.insert(field.into(),json!(n));}
    }
    report.insert("compaction".into(),Value::Object(compaction));
    json!({"attempt_id":attempt,"report":report})
}
fn run_diagnostics(run: &Value) -> Value {
    const CHECKPOINTS: [&str; 8] = ["admission", "attachment_preparation", "context_assembly",
        "model_dispatch", "model_output", "pre_tool", "tool_result", "publication"];
    let checkpoints = CHECKPOINTS.iter().filter_map(|name| {
        let receipt = run.pointer(&format!("/workflow/receipts/{name}"))?;
        let decision = receipt["result"]["decision"].as_str()?;
        if !matches!(decision, "pass" | "reject") { return None; }
        let reason_code = if decision == "reject" {
            match receipt["result"]["reason"].as_str() {
                Some("Publication exceeds configured limit") => "publication_limit",
                Some("Tool invocation denied") |
                Some("Tool invocation denied by policy or invalid arguments") => "tool_denied",
                _ => "rejected",
            }
        } else { "" };
        Some(json!({"checkpoint":name,"decision":decision,"reason_code":reason_code,
            "policy_revision":receipt["policy_revision"],"turn":receipt["turn"],
            "call_index":receipt["call_index"]}))
    }).collect::<Vec<_>>();
    json!({"run_id":run["id"],"session_id":run["session_id"],"status":run["status"],
        "uncertain":run["uncertain"],"checkpoints":checkpoints,
        "tool_failure":tools::failure::project(&run["latest_tool_failure"]),
        "context_summary":public_context_summary(run),
        "latest_context_assembly":public_assembly_attempt(&run["latest_context_assembly"]),
        "last_successful_assembly":public_context_summary(run),
        "ability_context":abilities::AbilityContext::public(&run["ability_context"])})
}
async fn run_diagnostics_api(
    State(app): State<App>,
    Path((sid, rid)): Path<(String, String)>,
) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let run = app.run(&rid).await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    if run["session_id"] != sid {
        return Err(ApiError(StatusCode::NOT_FOUND, "Run not found".into()));
    }
    let mut diagnostic=run_diagnostics(&run);
    diagnostic["reasoning_cleanup_logical_bytes_removed"]=json!(app.store.reasoning_cleanup_total(&sid).await?);
    Ok(Json(diagnostic))
}
// Session access is checked again against the durable run before projecting
// progress. The projection contains no model, tool, or user message content.
async fn run_progress_api(
    State(app): State<App>,
    Path((sid, rid)): Path<(String, String)>,
) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let run = app.run(&rid).await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    if run["session_id"] != sid {
        return Err(ApiError(StatusCode::NOT_FOUND, "Run not found".into()));
    }
    Ok(Json(task_progress::project(&run)))
}
fn required<'a>(body: &'a Value, name: &str, max: usize) -> std::result::Result<&'a str, ApiError> {
    let s = body[name]
        .as_str()
        .ok_or_else(|| bad(&format!("{name} is required")))?;
    if s.is_empty() || s.len() > max {
        return Err(bad(&format!("{name} has invalid length")));
    }
    Ok(s)
}
async fn assembled_instructions(store: &Store) -> Result<String> {
    assembled_agent_instructions(store, "shared_default").await
}
async fn assembled_agent_instructions(store: &Store, agent_id: &str) -> Result<String> {
    context_assembly::pinned_instructions(provider::DEFAULT_INSTRUCTIONS,
        &store.list_agent_prompt_slots(agent_id).await?)
}
/// Advance a pinned cursor only for a typed, safe pre-response failure.
fn model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
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
// Diagnostic ownership comes from the caller's Store, never model/run arguments.
fn trusted_model_failure(store:&Store,run:&mut Value,error:&anyhow::Error,system:bool)->Result<()> {
    let evidence=if diagnostics::provider_enabled(store) {
        let selected=if system {system_dispatch_run(run).ok().map(|r|r["provider_selection"].clone())}else{Some(run["provider_selection"].clone())};
        Some(json!({"id":run["id"],"session_id":run["session_id"],"provider_selection":selected,"candidate_index":if system {run["system_failover_state"]["candidate_index"].clone()}else{run["failover_state"]["candidate_index"].clone()}}))
    }else{None};
    if system {system_model_failure(run,error)?;}else{model_failure(run,error)?;}
    if let Some(evidence)=evidence {
        let next=if system {run["system_failover_state"]["candidate_index"].as_u64()}else{run["failover_state"]["candidate_index"].as_u64()};
        let action=if next.unwrap_or(0)>evidence["candidate_index"].as_u64().unwrap_or(0){"backup"}else if run["status"]=="queued"{"retry"}else if run["provider_recovery"]["safe_to_resume"]==true{"resume_required"}else{"terminal"};
        diagnostics::failure(store,&evidence,error,action);
    }
    Ok(())
}
// Compaction has its own pinned System cursor. Failover must never rewrite the chat role.
fn system_dispatch_run(run: &Value) -> Result<Value> {
    let plan:model_selection::SelectionPlan=serde_json::from_value(run["system_selection_plan"].clone())
        .map_err(|_|anyhow!("System model required for context compaction; configure it before starting a new run"))?;
    if plan.role != model_selection::ModelRole::System {return Err(anyhow!("Invalid System role plan"));}
    let state:model_selection::FailoverState=serde_json::from_value(run["system_failover_state"].clone())?;
    let candidate=plan.candidates.get(state.candidate_index).ok_or_else(||anyhow!("System candidates exhausted"))?;
    Ok(json!({"id":run["id"],"session_id":run["session_id"],"requester_user_id":run["requester_user_id"],"agent_id":run["agent_id"],"selection_plan":plan,"failover_state":state,
        "uncertain":run["uncertain"],"provider_selection":provider::ProviderSelection { limits:candidate.limits.clone(),
            provider:candidate.transport.provider_id(),model:candidate.upstream_model.clone()}.value()}))
}
fn system_model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
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
// System requests use their own model ceiling, including instructions and JSON framing.
#[cfg(test)]
fn system_summary_input(context: &loop_runtime::Context, selection: &provider::ProviderSelection,
    prompt: String, max_bytes: usize) -> Result<Vec<Value>> {
    let input=vec![json!({"role":"user","content":prompt})];
    validate_system_summary_input(context,selection,&input,provider::DEFAULT_INSTRUCTIONS,max_bytes)?;
    Ok(input)
}
/// Validate the complete summary envelope, including current instructions, against the System model and workflow byte ceilings.
fn validate_system_summary_input(context:&loop_runtime::Context,selection:&provider::ProviderSelection,input:&[Value],instructions:&str,max_bytes:usize)->Result<()> {
    #[derive(serde::Serialize)]
    struct Envelope<'a>{instructions:&'a str,tools:[Value;0],input:&'a [Value]}
    let envelope=Envelope{instructions,tools:[],input};
    if context_accounting::json_size(&envelope)?>context.ceiling(selection)?.min(max_bytes){
        return Err(context_assembly::Failure::new("system_summary_input_unavailable","summary_input").into());
    }
    Ok(())
}

#[cfg(test)]
mod code_index_run_projection_tests {
    use super::*;
    #[test]
    fn model_authorization_failure_stage_is_bounded_and_content_free() {
        let malformed: serde_json::Error = serde_json::from_str::<Value>("{").unwrap_err();
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::Error::new(malformed)),
            "plan_decode"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::Error::new(sqlx::Error::RowNotFound)),
            "storage"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!("BYO model outside tier")),
            "model_grant"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!("Role model is incompatible or not authorized")),
            "candidate_grant"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!("Agent owner conflict")),
            "agent_owner"
        );
        let hostile = anyhow::anyhow!("account secret: bearer-token and private model label");
        assert_eq!(bounded_model_authorization_failure_stage(&hostile), "unknown");
    }

    #[test]
    fn public_code_index_outcome_is_an_allowlisted_content_free_projection() {
        let run=json!({"id":"run","session_id":"session","status":"running",
            "code_index_retrieval_attempted":true,"code_index_retrieval_status":"secret query and source excerpt",
            "code_index_retrieval_stage":"provider said: secret","code_index_query":"private query",
            "code_index_source":"private source","code_index_retrieval_error":"upstream body"});
        let public=public_run(&run);
        assert_eq!(public["code_index_retrieval"],json!({"status":"unavailable","stage":"unknown"}));
        let rendered=public.to_string();
        for secret in ["secret query","provider said","private query","private source","upstream body"] {
            assert!(!rendered.contains(secret));
        }
    }
    #[test]
    fn public_code_index_outcome_exposes_only_valid_status_and_stage() {
        let run=json!({"code_index_retrieval_attempted":true,"code_index_retrieval_status":"attached_candidates",
            "code_index_retrieval_stage":"complete","code_index_query":"private"});
        assert_eq!(public_code_index_retrieval(&run),json!({"status":"attached_candidates","stage":"complete"}));
        assert_eq!(public_code_index_retrieval(&json!({})),json!({"status":"not_attempted","stage":"not_started"}));
    }
}

#[cfg(test)]
mod system_budget_tests {
    use super::*;
    // Different role profiles must not let a large Standard budget authorize a smaller System request.
    #[test]
    fn compaction_uses_system_profile_and_counts_escaped_envelope() {
        let mut context=loop_runtime::Config::default().context;
        let standard=provider::ProviderSelection { limits:None,provider:provider::ProviderId::OpenaiSubscription,model:"standard".into()};
        let system=provider::ProviderSelection { limits:None,provider:provider::ProviderId::OpenaiSubscription,model:"system".into()};
        context.model_profiles.insert("openai_subscription/system".into(),
            loop_runtime::ModelBudget {context_tokens:512,output_reserve_tokens:64,bytes_per_token:1});
        let prompt="\"\\\n".repeat(200);
        assert!(system_summary_input(&context,&standard,prompt.clone(),262144).is_ok());
        assert!(system_summary_input(&context,&system,prompt,262144).is_err());
        assert!(system_summary_input(&context,&standard,"small".into(),8).is_err());
    }
}
/// Seed installation tool permissions; live account, agent and session gates still constrain execution.
fn policy_default() -> Value {
    let mut policy=json!({"version":1,"rules":{"list_abilities":"allow","load_ability":"allow","list_skills":"allow","load_skill":"allow","context_assets":"allow","get_time":"allow","get_date":"allow","calculate":"allow","read_attachment":"allow","search_this_session":"allow","read_own_prompt":"allow","web_search":"allow","get_weather":"allow","maps_geocode":"allow","file_list":"allow","file_read":"allow","file_search":"allow","state_inspect":"allow","prompt_slot_read":"allow","prompt_slot_write":"require_approval","file_write":"require_approval","file_edit":"require_approval","workspace_seed":"require_approval","source_review":"allow","source_patch":"require_approval","promotion_propose":"allow","shell":"require_approval","python":"require_approval"}});
    for name in ["task_status","task_wait","task_cancel"] {policy["rules"][name]=json!("allow");}
    for schema in tools::administrator::schemas() {
        let name=schema["name"].as_str().unwrap();
        policy["rules"][name]=json!(if tools::is_read(name){"allow"}else{"require_approval"});
    }
    for schema in tools::core_mode::schemas().into_iter()
        .chain(tools::core_search::schemas()).chain(tools::core_compaction::schemas())
        .chain(tools::core_recall::schemas()).chain(tools::core_self_edit::schemas()).chain(crate::session_messaging::schemas())
        .chain(crate::main_chat::schemas()).chain(tools::core_task::schemas()).chain(tools::core_tool_search::schemas()).chain(tools::core_decision::schemas()).chain(tools::core_directory_decision::schemas()).chain(modules::schemas()) {
        let name=schema["name"].as_str().unwrap();
        policy["rules"][name]=json!(if tools::is_read(name){"allow"}else{"require_approval"});
    }
    for schema in mcp::all_schemas() {
        let name=schema["name"].as_str().unwrap();
        policy["rules"][name]=json!(if tools::is_read(name){"allow"}else{"require_approval"});
    }
    for name in ["message_other_session","start_session"] {policy["rules"][name]=json!("allow");}
    #[cfg(test)] { policy["rules"]["test_owner_purchase"]=json!("require_approval"); }
    policy
}
fn policy_decision<'a>(policy: &'a Value, name: &str) -> &'a str {
    policy["rules"][name].as_str().unwrap_or("deny")
}
fn live_tool_decision(app:&App, access:&access_policy::LiveAccess, name:&str,
    policy:&Value, visibility:&Value, state:&Value, loaded:&Value,
    read_only:bool) -> access::ToolDecision {
    let installation=access::Permission::from_policy(policy_decision(policy,name))
        .unwrap_or(access::Permission::Deny);
    let (available,visible)=app.abilities.tool_gate(name,visibility,state,loaded);
    let ability_ids=app.abilities.tool_abilities(name,visibility,state,loaded);
    let caution=if !tools::is_read(name) && ability_ids.iter().any(|id| {
        let key=match id.as_str(){"terminal_control"=>"terminal_caution","codebase_admin"=>"destructive_caution",_=>return false};
        visibility["settings"][id][key]=="cautious"
    }) {access::Permission::Ask}else{access::Permission::Auto};
    access.tool_decision_for_abilities(name,&ability_ids,installation.meet(caution),
        available,visible,read_only && !crate::session_messaging::owns(name),tools::is_read(name))
}
fn filter_ability_entry(app:&App, access:&access_policy::LiveAccess, entry:&mut Value,
    policy:&Value, visibility:&Value, state:&Value, loaded:&Value,
    read_only:bool) -> bool {
    let id=entry["id"].as_str().unwrap_or("");
    if access.ability_permission(id)==access::Permission::Deny {return false;}
    let Some(tools)=entry["tools"].as_array_mut() else {return false;};
    tools.retain(|tool| {
        let name=tool["name"].as_str().unwrap_or("");
        let installation=access::Permission::from_policy(policy_decision(policy,name))
            .unwrap_or(access::Permission::Deny);
        let (available,_)=app.abilities.tool_gate(name,visibility,state,loaded);
        let ability_ids=app.abilities.tool_abilities(name,visibility,state,loaded);
        access.tool_decision_for_abilities(name,&ability_ids,installation,
            available,true,read_only && !crate::session_messaging::owns(name),tools::is_read(name))
            .permission!=access::Permission::Deny
    });
    !tools.is_empty()
}
fn default_session_mode() -> Value {
    json!({"mode":"write","revision":0})
}
async fn agent_abilities(app: &App) -> Result<Value> {
    app.store.agent_abilities().await
}
async fn session_agent_id(app: &App, sid: &str) -> Result<String> {
    let session = app.store.get(&key("session", sid)).await?
        .ok_or_else(|| anyhow!("Session not found"))?;
    if session["status"] != "active" { return Err(crate::store::session::RecycledSession.into()); }
    Ok(session["agent_id"].as_str().unwrap_or("shared_default").to_owned())
}
async fn session_agent_abilities(app: &App, sid: &str) -> Result<Value> {
    let agent_id=session_agent_id(app,sid).await?;
    app.store.for_agent(&agent_id).await?.agent_abilities_for(&agent_id).await
}
fn default_session_abilities() -> Value {
    json!({"revision":0,"active":[],"suppressed":[]})
}
async fn session_ability_state(app: &App, sid: &str) -> Result<Value> {
    let value = app
        .store
        .get(&key("session_ability_state", sid))
        .await?
        .unwrap_or_else(default_session_abilities);
    if value["revision"].as_u64().is_none()
        || !value["active"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
        || !value["suppressed"]
            .as_array()
            .is_some_and(|a| a.iter().all(Value::is_string))
    {
        return Err(anyhow!("Invalid session ability state"));
    }
    Ok(value)
}
async fn ability_controls(app: &App, sid: &str) -> Result<(Value, u64, Value, Value)> {
    let (policy, mode_revision) = effective_policy(app, sid).await?;
    let visibility = session_agent_abilities(app, sid).await?;
    let state = session_ability_state(app, sid).await?;
    Ok((policy, mode_revision, visibility, state))
}
fn invalidate_waiting_run(mut run: Value) -> Value {
    run["pending"]["state"] = json!("requested");
    run["pending"]["decision"] = Value::Null;
    run["status"] = json!("queued");
    run["updated_at"] = json!(now());
    run
}
async fn session_mode(app: &App, sid: &str) -> Result<Value> {
    let mut value = app.store.get(&key("session_mode", sid)).await?.unwrap_or_else(default_session_mode);
    let agent=session_agent_id(app,sid).await?;
    let config=execution_modes::config(&app.store,&agent).await?;
    if let Some(id)=value["execution_mode"].as_str() {
        // Removed custom modes fail closed to Ask; never inherit write authority.
        let entry=execution_modes::resolve(&config,id).unwrap_or_else(|_|execution_modes::defaults()[0].clone());
        // A configuration edit cannot elevate an already read-only conversation.
        // Explicit mode selection must cross the existing approval boundary.
        if value["mode"]!="read_only" || entry["permission_policy"]=="read_only" {
            value["mode"]=entry["permission_policy"].clone();
        }
        value["execution_mode"]=entry["id"].clone();
        value["definition"]=entry;
    }
    if !matches!(value["mode"].as_str(), Some("write" | "read_only")) || value["revision"].as_u64().is_none() {
        return Err(anyhow!("Invalid session capability mode"));
    }
    Ok(value)
}
async fn effective_policy(app: &App, sid: &str) -> Result<(Value, u64)> {
    effective_policy_for_mode(app,&session_mode(app,sid).await?).await
}
async fn effective_policy_for_mode(app:&App,mode:&Value)->Result<(Value,u64)> {
    let mut policy = app
        .store
        .get("policy")
        .await?
        .unwrap_or_else(policy_default);
    // New Base discovery is available by default; an explicit saved denial wins.
    for name in ["search_tools","list_tools","load_tool"] {
        if policy["rules"].get(name).is_none() {policy["rules"][name]=json!("allow");}
    }
    for name in ["task_status","task_wait","task_cancel"] {
        if policy["rules"].get(name).is_none() {policy["rules"][name]=json!("allow");}
    }
    policy=crate::session_messaging::policy_defaults(policy);
    if mode["mode"] == "read_only" {
        for schema in tools::schemas() {
            let name = schema["name"].as_str().unwrap_or("");
            if !tools::is_read(name) && !crate::session_messaging::owns(name) {
                policy["rules"][name] = json!("deny");
            }
        }
    }
    Ok((policy, mode["revision"].as_u64().unwrap()))
}
fn marker(app: &App, shutoff: bool) -> Result<()> {
    let lock = File::open(app.control_dir.join("dispatch.lock"))?;
    lock.lock_exclusive()?;
    let result = (|| {
        let mut f = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(app.control_dir.join("shutoff"))?;
        f.write_all(if shutoff { b"1" } else { b"0" })?;
        f.sync_all()?;
        Ok(())
    })();
    FileExt::unlock(&lock)?;
    result
}
/// Restore persisted global control after launcher initialization, including API-only replacement.
/// Never infer enablement from container health: a saved shutoff remains disabled.
async fn restore_worker_control(app: &App) -> Result<()> {
    marker(app, app.shutoff().await?)
}
impl App {
    async fn run(&self, id: &str) -> Result<Value> {
        self.store
            .get(&key("run", id))
            .await?
            .ok_or_else(|| anyhow!("Run not found"))
    }
    async fn shutoff(&self) -> Result<bool> {
        Ok(self
            .store
            .get("control")
            .await?
            .unwrap_or(json!({"shutoff":true}))["shutoff"]
            .as_bool()
            .unwrap_or(true))
    }
    /// Deletion is durable across API/loop processes. The epoch also catches a
    /// recycle/restore occurring between polls; old work can never resume.
    async fn session_operation<T>(&self, sid: &str, cancel: CancellationToken,
        operation: impl std::future::Future<Output=Result<T>>) -> Result<T> {
        let initial=self.store.get(&key("session",sid)).await?
            .filter(|session|session["status"]=="active")
            .ok_or_else(||anyhow::Error::from(crate::store::session::RecycledSession))?;
        let epoch=initial["execution_epoch"].as_u64().unwrap_or(0);
        tokio::pin!(operation);
        let mut poll=tokio::time::interval(Duration::from_millis(100));
        loop {
            tokio::select! {
                result=&mut operation=>{
                    let available=self.store.get(&key("session",sid)).await
                        .is_ok_and(|session|session.is_some_and(|s|s["status"]=="active"
                            && s["execution_epoch"].as_u64().unwrap_or(0)==epoch));
                    if !available {cancel.cancel();return Err(crate::store::session::RecycledSession.into());}
                    return result;
                },
                _=poll.tick()=>{
                    let available=self.store.get(&key("session",sid)).await
                        .is_ok_and(|session|session.is_some_and(|s|s["status"]=="active"
                            && s["execution_epoch"].as_u64().unwrap_or(0)==epoch));
                    if !available {
                        cancel.cancel();
                        // Poll cancellation cleanup before dropping a worker socket.
                        let _=tokio::time::timeout(Duration::from_secs(5),&mut operation).await;
                        return Err(crate::store::session::RecycledSession.into());
                    }
                }
            }
        }
    }
    async fn save(&self, run: &mut Value) -> Result<()> {
        // Terminal documents are immutable in storage. Preserve their last
        // timestamp so an idempotent save remains identical across seconds.
        if !matches!(run["status"].as_str(), Some("completed" | "cancelled" | "failed"))
            || run["updated_at"].is_null() {
            run["updated_at"] = json!(now());
        }
        let public=public_run(run);
        self.store
            .save_run_observed(run, "run_status", &public)
            .await
    }
    async fn save_override(&self, run:&mut Value) -> Result<()> {
        run["updated_at"]=json!(now());
        let public=public_run(run);
        self.store.save_run_override_observed(run,"run_status",&public).await
    }
    async fn selection(&self) -> Result<provider::ProviderSelection> {
        let value = self.store.agent_model().await?;
        provider::ProviderSelection::parse(&value, DEFAULT_AGENT_MODEL)
    }
    /// Rehydrate published platform metadata at admission, then freeze it for this run.
    async fn pin_model_plan(&self, plan:&mut model_selection::SelectionPlan)->Result<()> {
        for candidate in &mut plan.candidates {
            if let Some(previous)=&candidate.platform {
                let source=account_catalog::Source{origin:account_catalog::Origin::Platform,
                    entry_id:account_catalog::platform_id(&previous.roster_id,&previous.entry.id)};
                let pin=self.store.pin_platform_source(shared_chat::actor(self),&source).await?;
                if pin.entry.transport!=candidate.transport {return Err(anyhow!("Adopted platform transport changed"));}
                let capability=match plan.role {model_selection::ModelRole::ImageIn=>"image_in",model_selection::ModelRole::ImageOut=>"image_out",model_selection::ModelRole::Voice=>"voice",model_selection::ModelRole::Decision=>"decision",_=>"text"};
                if !pin.entry.capabilities.contains(capability) {return Err(anyhow!("Published model lacks role capability"));}
                candidate.upstream_model=pin.entry.upstream_model.clone();candidate.platform=Some(pin);
            } else if !match &self.chat_access {
                Some(access)=>shared_chat::live_access(&self.store,shared_chat::actor(self),&access.session_id,&plan.agent_id).await?,
                None=>self.store.live_access(shared_chat::actor(self),&plan.agent_id).await?,
            }.model_allowed(&candidate.upstream_model) {
                return Err(anyhow!("BYO model outside tier"));
            }
        }
        Ok(())
    }
    async fn run_model_authorized(&self,run:&Value)->bool {
        match self.run_candidate(run).await {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    run_id=run["id"].as_str().unwrap_or("unknown"),
                    stage=bounded_model_authorization_failure_stage(&error),
                    "Pinned model authorization failed"
                );
                false
            }
        }
    }
    async fn run_candidate(&self, run: &Value) -> Result<Option<model_selection::Candidate>> {
        if !crate::modules::binding_valid(self,run).await {return Err(anyhow!("Module admission revoked"));}
        // Recovery cannot invent a role for an old run that never pinned one.
        let Some(value) = run.get("selection_plan").filter(|v| !v.is_null()) else {
            return Err(anyhow!("Legacy run has no pinned role; start a new message after configuring Standard"));
        };
        let plan: model_selection::SelectionPlan = serde_json::from_value(value.clone())?;
        let state: model_selection::FailoverState = serde_json::from_value(run["failover_state"].clone())?;
        if !matches!(plan.version,1|2) || plan.agent_id != run["agent_id"].as_str().unwrap_or("") {
            return Err(anyhow!("Invalid pinned model plan identity"));
        }
        let catalog = self.store.model_catalog().await?;
        // Membership, account ceiling and roster revocation remain live even for recovered plans.
        let candidate = plan.candidates.get(state.candidate_index).ok_or_else(||anyhow!("Candidate missing"))?;
        let requester=run["requester_user_id"].as_str().unwrap_or(self.store.current_user_id());
        let access=shared_chat::live_access(&self.store,requester,run["session_id"].as_str().unwrap_or(""),&plan.agent_id).await?;
        if !access.admitted() || (candidate.platform.is_none() && !access.model_allowed(&candidate.upstream_model)) {
            return Err(anyhow!("Model grant revoked"));
        }
        if catalog.agent_models.get(&plan.agent_id).is_none_or(|m|!m.entry_ids.contains(&candidate.entry_id)) {
            return Err(anyhow!("Candidate grant revoked"));
        }
        let candidate=plan.authorize_candidate(state.candidate_index, &catalog.entries, &catalog.connections)?.clone();
        if let Some(pin)=&candidate.platform {
            let live=catalog.platform_sources.get(&candidate.entry_id).ok_or_else(||anyhow!("Platform provenance removed"))?;
            if live.roster_id!=pin.roster_id || live.entry.id!=pin.entry.id {return Err(anyhow!("Platform provenance changed"));}
            self.store.platform_dispatch_tokens(requester,&plan.agent_id,pin).await?;
        } else if catalog.platform_sources.contains_key(&candidate.entry_id) {return Err(anyhow!("Platform provenance missing from plan"));}
        Ok(Some(candidate))
    }
    async fn tokens_for(&self, selection: &provider::ProviderSelection, run: &Value) -> Result<Value> {
        let candidate = self.run_candidate(run).await?;
        if let Some(candidate) = &candidate {
            if selection.model != candidate.upstream_model || selection.provider != candidate.transport.provider_id() {
                return Err(anyhow!("Pinned model selection mismatch"));
            }
        }
        if let Some(pin)=candidate.as_ref().and_then(|candidate|candidate.platform.as_ref()) {
            return self.store.platform_dispatch_tokens(run["requester_user_id"].as_str().unwrap_or(self.store.current_user_id()),run["agent_id"].as_str().unwrap_or(""),pin).await;
        }
        let reference = candidate.as_ref().map(|c| c.credential_ref.as_str()).unwrap_or("provider_tokens");
        if selection.provider == provider::ProviderId::OpenaiSubscription {
            self.tokens_at(reference).await.map_err(|_| provider::typed_failure(model_selection::FailureKind::InvalidCredentials,
                false, "Assigned provider authorization unavailable; reconnect this connection"))
        } else {
            if candidate.is_none() { return Err(anyhow!("API providers require an explicit connection")); }
            let tokens = self.store.get(reference).await?.ok_or_else(|| provider::typed_failure(model_selection::FailureKind::InvalidCredentials, false, "Save an API key for this connection"))?;
            if tokens["api_key"].as_str().is_none_or(|s| s.is_empty()) { return Err(provider::typed_failure(model_selection::FailureKind::InvalidCredentials, false, "Provider API key unavailable")); }
            Ok(tokens)
        }
    }

    pub(crate) async fn tokens_at(&self, reference: &str) -> Result<Value> {
        let _lock = self.refresh.lock().await;
        let mut tokens = self
            .store
            .get(reference)
            .await?
            .ok_or_else(|| anyhow!("Connect your ChatGPT subscription first"))?;
        if tokens["expires_at"].as_f64().unwrap_or(0.) < now() as f64 + 60. {
            tokens = self.provider.refresh(&tokens).await?;
            self.store.put(reference, &tokens).await?;
        }
        Ok(tokens)
    }
}
// Exit fixed offline smoke before state initialization; otherwise initialize the selected role at the shared v3 root.
async fn run_process() -> Result<()> {
    if std::env::args().any(|argument|argument=="--boot-plan") {
        #[cfg(feature="linked-context")]
        {
            // Stable host CLI calls the library directly, before state or credentials initialize.
            use std::io::Read;
            let mut bytes=Vec::new();std::io::stdin().take(65537).read_to_end(&mut bytes)?;
            if bytes.len()>65536 {anyhow::bail!("Boot input bounds");}
            let input:webagent_boot_contracts::Request=serde_json::from_slice(&bytes)?;
            println!("{}",serde_json::to_string(&webagent_boot_supervisor::plan(&input)?)?);
            return Ok(());
        }
        #[cfg(feature="live-context")]
        anyhow::bail!("Linked boot planner unavailable in live core");
    }
    if std::env::args().any(|argument|argument=="--context-smoke") {println!("{}",crate::engine_components::context_smoke().await?);return Ok(());}
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("webagent_v3=info".parse()?),
        )
        .init();
    if let Some(path)=std::env::args().skip(1).find_map(|arg|arg.strip_prefix("--validate-assets=").map(PathBuf::from)) {
        let catalog=abilities::Catalog::load_snapshot(&path.join("plugins/abilities"))?;
        let engine=crate::engine_components::load(&path,"candidate")?;
        let prompts:Value=serde_json::from_slice(&std::fs::read(path.join("config/prompts.json"))?)?;
        crate::api::live_configuration::validate_prompts(&prompts)?;
        for (file,id)in [("app-settings.json","app_settings"),("chat.json","chat"),("context-compaction.json","context_compaction"),("tool-output-retention.json","tool_output_retention"),("access-policy.json","access_policy")] {
            let value:Value=serde_json::from_slice(&std::fs::read(path.join("config").join(file))?)?;
            crate::api::live_configuration::validate(id,&value)?;
        }
        println!("{}",json!({"catalog_version":catalog.version(),"abilities":catalog.ids().len(),"engine_components":engine.entries.len()}));
        return Ok(());
    }
    let root = PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
    let role=std::env::var("V3_PROCESS_ROLE").unwrap_or_else(|_|"api".into());
    if role=="health" {
        let url=format!("sqlite://{}?mode=ro",root.join("db/app.db").display());
        let pool=sqlx::SqlitePool::connect(&url).await?;
        let age:Option<i64>=sqlx::query_scalar(
            "SELECT CAST(strftime('%s','now') AS INTEGER)-updated_at FROM loop_heartbeat WHERE singleton=1 AND protocol_version=1")
            .fetch_optional(&pool).await?;
        pool.close().await;
        return if age.is_some_and(|age|age>=0 && age<=10) {Ok(())}
            else {Err(anyhow!("V3 loop heartbeat is stale"))};
    }
    if !matches!(role.as_str(),"api"|"loop") {
        return Err(anyhow!("Invalid v3 process role"));
    }
    std::fs::create_dir_all(&root)?;
    let lock=if role=="loop" {
        let lock=OpenOptions::new().create(true).truncate(false).write(true)
            .open(root.join("runtime.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_|anyhow!("Another v3 loop owns this state"))?;
        Some(lock)
    } else {None};
    // Seed mutable prompt configuration once; both roles reload it per turn.
    std::fs::create_dir_all(root.join("config"))?;
    match OpenOptions::new().write(true).create_new(true).open(root.join("config/prompts.json")) {
        Ok(mut file) => file.write_all(include_bytes!("../config/prompts.json"))?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(error.into()),
    }
    if crate::session_naming::config::initialize(&root).is_err() {
        tracing::warn!("Naming prompt initialization unavailable; preserving existing prompts");
    }
    // Mutable retention defaults reload at model-call boundaries.
    match OpenOptions::new().write(true).create_new(true).open(root.join("config/tool-output-retention.json")) {
        Ok(mut file) => {file.write_all(include_bytes!("../config/tool-output-retention.json"))?;file.sync_all()?;},
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{},
        Err(error)=>return Err(error.into()),
    }
    let store = Store::open(&root).await?;
    diagnostics::init(&root, &store, &role).await;
    optimization::init(&root, &role).await;
    mcp::bridge::init(&root, &role).await;
    store.migrate_agent_storage(&root).await?;
    let (runtime, runtime_path) = loop_runtime::Config::initialize(&root, &store).await?;
    // One data-root authority, reread at call boundaries. Legacy JSON is only
    // a first-start migration source; existing invalid data is never overwritten.
    let compaction_config_path = context_compaction::Config::initialize(&root)?;
    crate::engine_components::initialize(&root)?;
    store.configure_cache(runtime.cache.document_budget_bytes, runtime.cache.context_budget_bytes);
    // The temporary development bootstrap credential had no generation. Retire it
    // once so this installation follows the same first-run setup flow as v2.
    if store
        .get("admin")
        .await?
        .is_some_and(|v| v["generation"].is_null())
    {
        store.delete("admin").await?;
    }
    for (k, v) in [
        ("policy", policy_default()),
        ("settings", json!({"model":DEFAULT_AGENT_MODEL})),
        ("control", json!({"shutoff":false})),
    ] {
        if store.get(k).await?.is_none() {
            store.put(k, &v).await?;
        }
    }
    store.backfill_shared_directory().await?;
    migrate_default_agent_model(&store).await?;
    migrate_provider_selection(&store).await?;
    // Catalog roles become authoritative once; later clears never restore legacy defaults.
    model_selection_api::migrate_legacy_default(&store).await?;
    migrate_tool_policy_defaults(&store).await?;
    let abilities_root = std::env::var("V3_ABILITIES_ROOT")
        .unwrap_or_else(|_| "/opt/v3/plugins/abilities".to_owned());
    let catalog = abilities::Catalog::load(&PathBuf::from(abilities_root))?;
    store.ensure_agent_connections(&catalog.ids()).await?;
    live_assets::initialize_prompt_baseline(&root)?;
    let app = App {
        runtime: Arc::new(RwLock::new(runtime)), runtime_path, compaction_config_path,
        browser_login_bridge: browser_login::Bridge::configure(
            &std::env::var("V3_BROWSER_LOGIN_TOKEN").unwrap_or_default(),
            &std::env::var("V3_BROWSER_LOGIN_CALLER_ID").unwrap_or_default(),
        )?,
        message_api_token_hash: message_api_token_hash(&std::env::var("V3_MESSAGE_API_TOKEN").unwrap_or_default())?,
        store,
        chat_actor: None,
        chat_access: None,
        abilities: Arc::new(catalog),
        gate: Arc::new(Mutex::new(())),
        provider: provider::Provider::new(),
        refresh: Arc::new(Mutex::new(())),
        login: Arc::new(RwLock::new(None)),
        active: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
        ready: Arc::new(AtomicBool::new(false)),
        stop: CancellationToken::new(),
        login_attempts: Arc::new(Mutex::new(Vec::new())),
        control_dir: PathBuf::from(
            std::env::var("V3_CONTROL_ROOT").unwrap_or_else(|_| "/run/v3/control".into()),
        ),
        tool_socket: PathBuf::from(
            std::env::var("V3_TOOL_SOCKET").unwrap_or_else(|_| "/run/v3/worker/tools.sock".into()),
        ),
        build_socket: PathBuf::from(std::env::var("V3_BUILD_SOCKET").unwrap_or_else(|_|"/run/v3/build/build.sock".into())),
        promotion_socket: PathBuf::from(
            std::env::var("V3_PROMOTE_SOCKET")
                .unwrap_or_else(|_| "/run/v3/promote/promote.sock".into()),
        ),
        secure: std::env::var("V3_SECURE_COOKIE").unwrap_or_else(|_| "true".into()) != "false",
        chat_ui_config_path: PathBuf::from(
            std::env::var("V3_CHAT_UI_CONFIG_PATH")
                .unwrap_or_else(|_| "/state/config/chat.json".into()),
        ),
    };
    if role=="loop" {
        let progress=agent_loop::watchdog::Progress::new();
        let _watchdog=agent_loop::watchdog::Watchdog::start(progress.clone())?;
        restore_worker_control(&app).await?;
        recover(&app, true).await?;
        app.ready.store(true, Ordering::Release);
        let mut scheduler=tokio::spawn(scheduler_with_progress(app.clone(),progress));
        let mut signal=tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            _=signal.recv()=>{},
            _=tokio::signal::ctrl_c()=>{},
            result=&mut scheduler=>{
                // A dispatcher panic/early return must terminate the supervised
                // process rather than leave healthy lease renewers running alone.
                return Err(anyhow!("Loop dispatcher exited unexpectedly: {}", if result.is_err() {"task failure"} else {"early return"}));
            }
        }
        app.ready.store(false, Ordering::Release);
        app.stop.cancel();
        for token in app.active.lock().await.values() {token.cancel();}
        // A stuck gate must not make SIGTERM wait forever.
        if tokio::time::timeout(Duration::from_secs(12),&mut scheduler).await.is_err() {
            scheduler.abort();
        }
        let deadline=tokio::time::Instant::now()+Duration::from_secs(12);
        while !app.active.lock().await.is_empty() && tokio::time::Instant::now()<deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _=marker(&app,true);
        app.stop.cancel();
        for token in app.active.lock().await.values() {token.cancel();}
        let _=app.store.clear_loop_heartbeat().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
        app.store.pool.close().await;
        app.store.agent_pool.close().await;
        app.store.app_pool.close().await;
        drop(lock);
        return Ok(());
    }
    recover_promotions(&app).await?;
    restore_worker_control(&app).await?;
    app.ready.store(true, Ordering::Release);
    let listener = tokio::net::TcpListener::bind(
        std::env::var("V3_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into()),
    )
    .await?;
    tracing::info!("WebAgent v3 API ready");
    let notification_relay=tokio::spawn(api::notifications::service(app.clone()));
    let administrator_watch=tokio::spawn(administrator_services::watch(app.clone()));
    let shutdown = app.clone();
    axum::serve(listener, router(app.clone()))
        .with_graceful_shutdown(async move {
            let mut signal =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("signal handler");
            tokio::select! {_=signal.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
            shutdown.ready.store(false, Ordering::Release);
            shutdown.stop.cancel();
        })
        .await?;
    app.stop.cancel();
    let _=administrator_watch.await;
    let _=notification_relay.await;
    app.store.pool.close().await;
    app.store.agent_pool.close().await;
    app.store.app_pool.close().await;
    Ok(())
}
