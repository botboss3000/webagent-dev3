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
    login_attempts: webagent_accounts::LoginAttempts,
    control_dir: PathBuf,
    tool_socket: PathBuf,
    build_socket: PathBuf,
    promotion_socket: PathBuf,
    secure: bool,
    chat_ui_config_path: PathBuf,
    message_api_token_hash: Option<[u8; 32]>,
    browser_login_bridge: Option<browser_login::Bridge>,
}
pub(crate) use webagent_host_core::{bad, conflict, now, uuid, Api, ApiError};
use webagent_configuration::digest;
use webagent_host_core::key;
fn terminal(status: &str) -> bool {
    webagent_engine::runtime::failure::terminal(status)
}
fn status(run: &Value) -> &str {
    run["status"].as_str().unwrap_or("failed")
}
// CONTEXT-PUBLIC-SUMMARY: publish measured numeric totals only, never arbitrary
// persisted report fields, overlays, schema descriptions or transcript content.
fn public_context_summary(run: &Value) -> Value {
    let report = &run["context_assembly_report"];
    if report["version"] != 1 || report["scope"] != "logical_call_envelope" {
        return Value::Null;
    }
    let fields = [
        "envelope_bytes",
        "max_bytes",
        "history_json_bytes",
        "instruction_json_bytes",
        "schemas_json_bytes",
        "turn",
        "call",
    ];
    let mut summary = serde_json::Map::new();
    for field in fields {
        let Some(value) = report[field].as_u64() else {
            return Value::Null;
        };
        summary.insert(field.into(), json!(value));
    }
    summary.insert(
        "accounting".into(),
        json!("serialized_json_bytes_not_provider_tokens"),
    );
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
        (
            "Legacy run has no pinned role; start a new message after configuring Standard",
            "plan_missing",
        ),
        ("Invalid pinned model plan identity", "plan_identity"),
        ("Candidate missing", "candidate_missing"),
        ("Selection candidates exhausted", "candidate_missing"),
        ("Model entry removed", "candidate_missing"),
        ("Model connection removed", "candidate_missing"),
        (
            "Pinned model is no longer available to this caller",
            "model_grant",
        ),
        ("Model grant revoked", "model_grant"),
        ("Candidate grant revoked", "candidate_grant"),
        (
            "Pinned model candidate is no longer authorized",
            "candidate_grant",
        ),
        ("Model candidate is not authorized", "candidate_grant"),
        ("Platform provenance removed", "platform"),
        ("Platform provenance changed", "platform"),
        ("Platform provenance missing from plan", "platform"),
        ("Published model lacks role capability", "platform"),
        (
            "Selected model connection is unavailable",
            "candidate_grant",
        ),
        ("Agent model roster required", "candidate_grant"),
        ("Invalid agent model roster", "candidate_grant"),
        (
            "Invalid or duplicate role backup candidates",
            "candidate_grant",
        ),
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
        (
            "Role model is incompatible or not authorized",
            "candidate_grant",
        ),
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
    webagent_engine::output::public_run_model(run)
}
async fn public_runs(app: &App, views: Vec<Value>) -> Result<Vec<Value>> {
    let mut result = Vec::with_capacity(views.len());
    for view in views {
        // Paused views need the owning record's private component marker; old
        // views need its model fields. Read through Store and project only safe fields.
        let has_retrieval_marker = view["code_index_retrieval_attempted"].is_boolean()
            && view["code_index_retrieval_status"].as_str().is_some()
            && view["code_index_retrieval_stage"].as_str().is_some();
        if view["status"] == "paused" || view["model"].is_null() || !has_retrieval_marker {
            let run = app
                .run(
                    view["id"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Run id missing"))?,
                )
                .await?;
            result.push(public_run(&run));
        } else {
            result.push(public_run(&view));
        }
    }
    shared_projection::runs(app, &mut result).await?;
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
fn public_assembly_attempt(event: &Value) -> Value {
    let Some(attempt) = event["attempt_id"]
        .as_str()
        .filter(|s| uuid::Uuid::parse_str(s).is_ok())
    else {
        return Value::Null;
    };
    let source = &event["report"];
    let mut report = serde_json::Map::new();
    for field in [
        "max_bytes",
        "instructions_json_bytes",
        "base_content_bytes",
        "guidance_content_bytes",
        "schemas_json_bytes",
        "history_json_bytes",
        "framing_bytes",
        "history_budget_bytes",
        "summary_reserve_bytes",
        "trigger_tokens_estimated",
        "tail_tokens_estimated",
        "characters_per_token",
    ] {
        if let Some(n) = source[field].as_u64() {
            report.insert(field.into(), json!(n));
        }
    }
    let outcome = match source["outcome"].as_str() {
        Some("ready") => "ready",
        Some("compaction_required") => "compaction_required",
        Some("failed") => "failed",
        _ => "unknown",
    };
    let code = source["failure_code"]
        .as_str()
        .filter(|c| {
            matches!(
                *c,
                "instruction_limit"
                    | "invalid_contribution"
                    | "overhead_exceeds_budget"
                    | "summary_reserve_unavailable"
                    | "envelope_exceeds_budget"
                    | "invalid_measurements"
                    | "no_safe_boundary"
                    | "stale_plan"
                    | "summary_length"
                    | "summary_did_not_shrink"
                    | "summary_missing"
                    | "summary_source_limit"
                    | "system_summary_input_unavailable"
                    | "manual_no_safe_boundary"
                    | "assembly_internal"
            )
        })
        .unwrap_or_default();
    let stage = source["stage"]
        .as_str()
        .filter(|s| {
            matches!(
                *s,
                "assembly"
                    | "selection"
                    | "instructions"
                    | "budget"
                    | "compaction_plan"
                    | "summary_input"
                    | "summary_validation"
            )
        })
        .unwrap_or("unknown");
    report.insert("outcome".into(), json!(outcome));
    report.insert("failure_code".into(), json!(code));
    report.insert("stage".into(), json!(stage));
    let mut compaction = serde_json::Map::new();
    for field in [
        "candidate_boundaries",
        "pending_calls",
        "duplicate_calls",
        "unmatched_calls_or_results",
        "source_limit_rejections",
        "tail_byte_rejections",
        "tail_character_rejections",
    ] {
        if let Some(n) = source["compaction"][field].as_u64() {
            compaction.insert(field.into(), json!(n));
        }
    }
    report.insert("compaction".into(), Value::Object(compaction));
    json!({"attempt_id":attempt,"report":report})
}
fn run_diagnostics(run: &Value) -> Value {
    const CHECKPOINTS: [&str; 8] = [
        "admission",
        "attachment_preparation",
        "context_assembly",
        "model_dispatch",
        "model_output",
        "pre_tool",
        "tool_result",
        "publication",
    ];
    let checkpoints = CHECKPOINTS
        .iter()
        .filter_map(|name| {
            let receipt = run.pointer(&format!("/workflow/receipts/{name}"))?;
            let decision = receipt["result"]["decision"].as_str()?;
            if !matches!(decision, "pass" | "reject") {
                return None;
            }
            let reason_code = if decision == "reject" {
                match receipt["result"]["reason"].as_str() {
                    Some("Publication exceeds configured limit") => "publication_limit",
                    Some("Tool invocation denied")
                    | Some("Tool invocation denied by policy or invalid arguments") => {
                        "tool_denied"
                    }
                    _ => "rejected",
                }
            } else {
                ""
            };
            Some(
                json!({"checkpoint":name,"decision":decision,"reason_code":reason_code,
            "policy_revision":receipt["policy_revision"],"turn":receipt["turn"],
            "call_index":receipt["call_index"]}),
            )
        })
        .collect::<Vec<_>>();
    json!({"run_id":run["id"],"session_id":run["session_id"],"status":run["status"],
        "uncertain":run["uncertain"],"checkpoints":checkpoints,
        "tool_failure":tools::failure::project(&run["latest_tool_failure"]),
        "context_summary":public_context_summary(run),
        "latest_context_assembly":public_assembly_attempt(&run["latest_context_assembly"]),
        "last_successful_assembly":public_context_summary(run),
        "ability_context":abilities::public_context(&run["ability_context"])})
}
async fn run_diagnostics_api(
    State(app): State<App>,
    Path((sid, rid)): Path<(String, String)>,
) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let run = app
        .run(&rid)
        .await
        .map_err(|_| ApiError(StatusCode::NOT_FOUND, "Run not found".into()))?;
    if run["session_id"] != sid {
        return Err(ApiError(StatusCode::NOT_FOUND, "Run not found".into()));
    }
    let mut diagnostic = run_diagnostics(&run);
    diagnostic["reasoning_cleanup_logical_bytes_removed"] =
        json!(app.store.reasoning_cleanup_total(&sid).await?);
    Ok(Json(diagnostic))
}
// Session access is checked again against the durable run before projecting
// progress. The projection contains no model, tool, or user message content.
async fn run_progress_api(State(app): State<App>, Path((sid, rid)): Path<(String, String)>) -> Api {
    let _guard = app.gate.lock().await;
    if app.store.get(&key("session", &sid)).await?.is_none() {
        return Err(ApiError(StatusCode::NOT_FOUND, "Session not found".into()));
    }
    let run = app
        .run(&rid)
        .await
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
    context_assembly::pinned_instructions(
        provider::DEFAULT_INSTRUCTIONS,
        &store.list_agent_prompt_slots(agent_id).await?,
    )
}
/// Advance a pinned cursor only for a typed, safe pre-response failure.
fn model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    webagent_engine::runtime::failure::model_failure(run, error)
}
// Diagnostic ownership comes from the caller's Store, never model/run arguments.
fn trusted_model_failure(
    store: &Store,
    run: &mut Value,
    error: &anyhow::Error,
    system: bool,
) -> Result<()> {
    webagent_engine::runtime::failure::trusted_model_failure(
        &diagnostics::EngineModelDiagnostics,
        store,
        run,
        error,
        system,
    )
}
// Compaction has its own pinned System cursor. Failover must never rewrite the chat role.
fn system_dispatch_run(run: &Value) -> Result<Value> {
    webagent_engine::runtime::failure::system_dispatch_run(run)
}
fn system_model_failure(run: &mut Value, error: &anyhow::Error) -> Result<()> {
    webagent_engine::runtime::failure::system_model_failure(run, error)
}
// System requests use their own model ceiling, including instructions and JSON framing.
#[cfg(test)]
fn system_summary_input(
    context: &loop_runtime::Context,
    selection: &provider::ProviderSelection,
    prompt: String,
    max_bytes: usize,
) -> Result<Vec<Value>> {
    let input = vec![json!({"role":"user","content":prompt})];
    validate_system_summary_input(
        context,
        selection,
        &input,
        provider::DEFAULT_INSTRUCTIONS,
        max_bytes,
    )?;
    Ok(input)
}
/// Validate the complete summary envelope, including current instructions, against the System model and workflow byte ceilings.
fn validate_system_summary_input(
    context: &loop_runtime::Context,
    selection: &provider::ProviderSelection,
    input: &[Value],
    instructions: &str,
    max_bytes: usize,
) -> Result<()> {
    webagent_engine::runtime::failure::validate_system_summary_input(
        context,
        selection,
        input,
        instructions,
        max_bytes,
    )
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
            bounded_model_authorization_failure_stage(&anyhow::Error::new(
                sqlx::Error::RowNotFound
            )),
            "storage"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!("BYO model outside tier")),
            "model_grant"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!(
                "Role model is incompatible or not authorized"
            )),
            "candidate_grant"
        );
        assert_eq!(
            bounded_model_authorization_failure_stage(&anyhow::anyhow!("Agent owner conflict")),
            "agent_owner"
        );
        let hostile = anyhow::anyhow!("account secret: bearer-token and private model label");
        assert_eq!(
            bounded_model_authorization_failure_stage(&hostile),
            "unknown"
        );
    }

    #[test]
    fn public_code_index_outcome_is_an_allowlisted_content_free_projection() {
        let run = json!({"id":"run","session_id":"session","status":"running",
            "code_index_retrieval_attempted":true,"code_index_retrieval_status":"secret query and source excerpt",
            "code_index_retrieval_stage":"provider said: secret","code_index_query":"private query",
            "code_index_source":"private source","code_index_retrieval_error":"upstream body"});
        let public = public_run(&run);
        assert_eq!(
            public["code_index_retrieval"],
            json!({"status":"unavailable","stage":"unknown"})
        );
        let rendered = public.to_string();
        for secret in [
            "secret query",
            "provider said",
            "private query",
            "private source",
            "upstream body",
        ] {
            assert!(!rendered.contains(secret));
        }
    }
    #[test]
    fn public_code_index_outcome_exposes_only_valid_status_and_stage() {
        let run = json!({"code_index_retrieval_attempted":true,"code_index_retrieval_status":"attached_candidates",
            "code_index_retrieval_stage":"complete","code_index_query":"private"});
        assert_eq!(
            public_code_index_retrieval(&run),
            json!({"status":"attached_candidates","stage":"complete"})
        );
        assert_eq!(
            public_code_index_retrieval(&json!({})),
            json!({"status":"not_attempted","stage":"not_started"})
        );
    }
}

#[cfg(test)]
mod system_budget_tests {
    use super::*;
    // Different role profiles must not let a large Standard budget authorize a smaller System request.
    #[test]
    fn compaction_uses_system_profile_and_counts_escaped_envelope() {
        let mut context = loop_runtime::Config::default().context;
        let standard = provider::ProviderSelection {
            limits: None,
            provider: provider::ProviderId::OpenaiSubscription,
            model: "standard".into(),
        };
        let system = provider::ProviderSelection {
            limits: None,
            provider: provider::ProviderId::OpenaiSubscription,
            model: "system".into(),
        };
        context.model_profiles.insert(
            "openai_subscription/system".into(),
            loop_runtime::ModelBudget {
                context_tokens: 512,
                output_reserve_tokens: 64,
                bytes_per_token: 1,
            },
        );
        let prompt = "\"\\\n".repeat(200);
        assert!(system_summary_input(&context, &standard, prompt.clone(), 262144).is_ok());
        assert!(system_summary_input(&context, &system, prompt, 262144).is_err());
        assert!(system_summary_input(&context, &standard, "small".into(), 8).is_err());
    }
}
/// Seed installation tool permissions; live account, agent and session gates still constrain execution.
fn policy_default() -> Value {
    webagent_engine::runtime::policy::default_policy::<
        tool_execution::NativeActions,
        engine_runtime::policy::NativePolicy,
    >()
}
fn policy_decision<'a>(policy: &'a Value, name: &str) -> &'a str {
    webagent_engine::runtime::policy::policy_decision(policy, name)
}
fn live_tool_decision(
    app: &App,
    access: &access_policy::LiveAccess,
    name: &str,
    policy: &Value,
    visibility: &Value,
    state: &Value,
    loaded: &Value,
    read_only: bool,
) -> access::ToolDecision {
    app.engine()
        .live_tool_decision(access, name, policy, visibility, state, loaded, read_only)
}
fn filter_ability_entry(
    app: &App,
    access: &access_policy::LiveAccess,
    entry: &mut Value,
    policy: &Value,
    visibility: &Value,
    state: &Value,
    loaded: &Value,
    read_only: bool,
) -> bool {
    app.engine()
        .filter_ability_entry(access, entry, policy, visibility, state, loaded, read_only)
}
fn default_session_mode() -> Value {
    webagent_engine::runtime::policy::default_session_mode()
}
async fn agent_abilities(app: &App) -> Result<Value> {
    app.store.agent_abilities().await
}
async fn session_agent_id(app: &App, sid: &str) -> Result<String> {
    app.engine().session_agent_id(sid).await
}
async fn session_agent_abilities(app: &App, sid: &str) -> Result<Value> {
    app.engine().session_agent_abilities(sid).await
}
fn default_session_abilities() -> Value {
    webagent_engine::runtime::policy::default_session_abilities()
}
async fn session_ability_state(app: &App, sid: &str) -> Result<Value> {
    app.engine().session_ability_state(sid).await
}
async fn ability_controls(app: &App, sid: &str) -> Result<(Value, u64, Value, Value)> {
    app.engine().ability_controls(sid).await
}
fn invalidate_waiting_run(mut run: Value) -> Value {
    run["pending"]["state"] = json!("requested");
    run["pending"]["decision"] = Value::Null;
    run["status"] = json!("queued");
    run["updated_at"] = json!(now());
    run
}
async fn session_mode(app: &App, sid: &str) -> Result<Value> {
    app.engine().session_mode(sid).await
}
async fn effective_policy(app: &App, sid: &str) -> Result<(Value, u64)> {
    app.engine().effective_policy(sid).await
}
async fn effective_policy_for_mode(app: &App, mode: &Value) -> Result<(Value, u64)> {
    app.engine().effective_policy_for_mode(mode).await
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
    async fn session_operation<T>(
        &self,
        sid: &str,
        cancel: CancellationToken,
        operation: impl std::future::Future<Output = Result<T>>,
    ) -> Result<T> {
        crate::shared_chat::guard_operation(&self.store, sid, cancel, operation).await
    }
    async fn save(&self, run: &mut Value) -> Result<()> {
        // Terminal documents are immutable in storage. Preserve their last
        // timestamp so an idempotent save remains identical across seconds.
        if !matches!(
            run["status"].as_str(),
            Some("completed" | "cancelled" | "failed")
        ) || run["updated_at"].is_null()
        {
            run["updated_at"] = json!(now());
        }
        let public = public_run(run);
        self.store
            .save_run_observed(run, "run_status", &public)
            .await
    }
    async fn save_override(&self, run: &mut Value) -> Result<()> {
        run["updated_at"] = json!(now());
        let public = public_run(run);
        self.store
            .save_run_override_observed(run, "run_status", &public)
            .await
    }
    async fn selection(&self) -> Result<provider::ProviderSelection> {
        let value = self.store.agent_model().await?;
        provider::ProviderSelection::parse(&value, DEFAULT_AGENT_MODEL)
    }
    /// Rehydrate published platform metadata at admission, then freeze it for this run.
    async fn pin_model_plan(&self, plan: &mut model_selection::SelectionPlan) -> Result<()> {
        self.models_runtime()
            .pin_model_plan(
                plan,
                shared_chat::actor(self),
                self.chat_access.as_ref().map(|a| a.session_id.as_str()),
                &ModelSessionAuthority(&self.store),
            )
            .await
    }
    async fn run_model_authorized(&self, run: &Value) -> bool {
        match self.run_candidate(run).await {
            Ok(_) => true,
            Err(error) => {
                tracing::warn!(
                    run_id = run["id"].as_str().unwrap_or("unknown"),
                    stage = bounded_model_authorization_failure_stage(&error),
                    "Pinned model authorization failed"
                );
                false
            }
        }
    }
    async fn run_candidate(&self, run: &Value) -> Result<Option<model_selection::Candidate>> {
        if !crate::modules::binding_valid(self, run).await {
            return Err(anyhow!("Module admission revoked"));
        }
        self.models_runtime()
            .run_candidate(run, &ModelSessionAuthority(&self.store))
            .await
    }
    async fn tokens_for(
        &self,
        selection: &provider::ProviderSelection,
        run: &Value,
    ) -> Result<Value> {
        if !crate::modules::binding_valid(self, run).await {
            return Err(anyhow!("Module admission revoked"));
        }
        self.models_runtime()
            .tokens_for(selection, run, &ModelSessionAuthority(&self.store))
            .await
    }

    pub(crate) async fn tokens_at(&self, reference: &str) -> Result<Value> {
        self.models_runtime().tokens_at(reference).await
    }
}
// Exit fixed offline smoke before state initialization; otherwise initialize the selected role at the shared v3 root.
async fn run_process() -> Result<()> {
    if std::env::args().any(|argument| argument == "--boot-plan") {
        #[cfg(feature = "linked-context")]
        {
            // Stable host CLI calls the library directly, before state or credentials initialize.
            use std::io::Read;
            let mut bytes = Vec::new();
            std::io::stdin().take(65537).read_to_end(&mut bytes)?;
            if bytes.len() > 65536 {
                anyhow::bail!("Boot input bounds");
            }
            let input: webagent_boot_contracts::Request = serde_json::from_slice(&bytes)?;
            println!(
                "{}",
                serde_json::to_string(&webagent_boot_supervisor::plan(&input)?)?
            );
            return Ok(());
        }
        #[cfg(feature = "live-context")]
        anyhow::bail!("Linked boot planner unavailable in live core");
    }
    if std::env::args().any(|argument| argument == "--context-smoke") {
        println!("{}", crate::engine_components::context_smoke().await?);
        return Ok(());
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("webagent_v3=info".parse()?),
        )
        .init();
    if let Some(path) = std::env::args()
        .skip(1)
        .find_map(|arg| arg.strip_prefix("--validate-assets=").map(PathBuf::from))
    {
        let catalog = abilities::Catalog::load_snapshot(&path.join("plugins/abilities"))?;
        let _ = crate::live_assets::native_schema_snapshot_at(Some(&path))?;
        let engine = crate::engine_components::load(&path, "candidate")?;
        let prompts: Value =
            serde_json::from_slice(&crate::live_assets::prompt_defaults_at(Some(&path))?)?;
        crate::api::live_configuration::validate_source_prompts(&prompts)?;
        for id in [
            "app_ui",
            "chat",
            "context_compaction",
            "tool_output_retention",
            "access_policy",
            "message_visibility",
            "closing_status",
        ] {
            let file = if id == "access_policy" {
                webagent_access::policy_file::LIVE_POLICY.to_owned()
            } else {
                crate::api::live_configuration::live(id)
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow!("Unknown live configuration"))?
            };
            let bytes = std::fs::read(path.join(file))?;
            if id == "tool_output_retention" {
                crate::tool_output::Policy::parse(&bytes)
                    .map_err(|_| anyhow!("Candidate retention validation failed"))?;
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            crate::api::live_configuration::validate(id, &value)?;
        }
        // Composition is read-only: no model preparation, credential access or provider dispatch.
        let state_root =
            PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
        let effective = provider::Provider::instructions_for_snapshot("", &state_root, Some(&path))
            .map_err(|_| anyhow!("Effective prompt validation failed"))?;
        // This reader uses mounted state/current fallback, independently of the candidate source above.
        // Validation containers have no checkout: validate the candidate's own live file.
        let mounted_retention = crate::tool_output::Policy::read_in(&[path.clone()])
            .map_err(|_| anyhow!("Candidate retention validation failed"))?;
        println!(
            "{}",
            json!({"catalog_version":catalog.version(),"abilities":catalog.ids().len(),"engine_components":engine.entries.len(),
            "effective_prompt_digest":digest(&json!(effective)),
            "mounted_retention_digest":digest(&json!(mounted_retention))})
        );
        return Ok(());
    }
    let root = PathBuf::from(std::env::var("V3_STATE_ROOT").unwrap_or_else(|_| "/state".into()));
    let role = std::env::var("V3_PROCESS_ROLE").unwrap_or_else(|_| "api".into());
    if role == "health" {
        let url = format!("sqlite://{}?mode=ro", root.join("db/app.db").display());
        let pool = sqlx::SqlitePool::connect(&url).await?;
        let age:Option<i64>=sqlx::query_scalar(
            "SELECT CAST(strftime('%s','now') AS INTEGER)-updated_at FROM loop_heartbeat WHERE singleton=1 AND protocol_version=1")
            .fetch_optional(&pool).await?;
        pool.close().await;
        return if age.is_some_and(|age| age >= 0 && age <= 10) {
            Ok(())
        } else {
            Err(anyhow!("V3 loop heartbeat is stale"))
        };
    }
    if !matches!(role.as_str(), "api" | "loop") {
        return Err(anyhow!("Invalid v3 process role"));
    }
    std::fs::create_dir_all(&root)?;
    let lock = if role == "loop" {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(root.join("runtime.lock"))?;
        lock.try_lock_exclusive()
            .map_err(|_| anyhow!("Another v3 loop owns this state"))?;
        Some(lock)
    } else {
        None
    };
    // Live configuration files are saved through the broker, before any owner reads them.
    crate::live_files::install(PathBuf::from(
        std::env::var("V3_BUILD_SOCKET").unwrap_or_else(|_| "/run/v3/build/build.sock".into()),
    ));
    let store = Store::open(&root).await?;
    diagnostics::init(&root, &store, &role).await;
    optimization::init(&root, &role).await;
    mcp::bridge::init(&root, &role).await;
    store.migrate_agent_storage(&root).await?;
    let (runtime, runtime_path) = loop_runtime::Config::live()?;
    // The live policy file, reread at call boundaries; fail start-up only if it is invalid.
    let compaction_config_path = context_compaction::Config::configured_path();
    context_compaction::Config::configured()?;
    crate::engine_components::initialize(&root)?;
    store.configure_cache(
        runtime.cache.document_budget_bytes,
        runtime.cache.context_budget_bytes,
    );
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
    let bridge = Arc::new(claude_bridge::Queue(store.clone()));
    let app = App {
        runtime: Arc::new(RwLock::new(runtime)),
        runtime_path,
        compaction_config_path,
        browser_login_bridge: browser_login::Bridge::configure(
            &std::env::var("V3_BROWSER_LOGIN_TOKEN").unwrap_or_default(),
            &std::env::var("V3_BROWSER_LOGIN_CALLER_ID").unwrap_or_default(),
        )?,
        message_api_token_hash: message_api_token_hash(
            &std::env::var("V3_MESSAGE_API_TOKEN").unwrap_or_default(),
        )?,
        store,
        chat_actor: None,
        chat_access: None,
        abilities: Arc::new(catalog),
        gate: Arc::new(Mutex::new(())),
        provider: provider::Provider::new().with_bridge(bridge),
        refresh: Arc::new(Mutex::new(())),
        login: Arc::new(RwLock::new(None)),
        active: Arc::new(Mutex::new(HashMap::new())),
        slots: Arc::new(Semaphore::new(Semaphore::MAX_PERMITS)),
        ready: Arc::new(AtomicBool::new(false)),
        stop: CancellationToken::new(),
        login_attempts: webagent_accounts::LoginAttempts::default(),
        control_dir: PathBuf::from(
            std::env::var("V3_CONTROL_ROOT").unwrap_or_else(|_| "/run/v3/control".into()),
        ),
        tool_socket: PathBuf::from(
            std::env::var("V3_TOOL_SOCKET").unwrap_or_else(|_| "/run/v3/worker/tools.sock".into()),
        ),
        build_socket: PathBuf::from(
            std::env::var("V3_BUILD_SOCKET").unwrap_or_else(|_| "/run/v3/build/build.sock".into()),
        ),
        promotion_socket: PathBuf::from(
            std::env::var("V3_PROMOTE_SOCKET")
                .unwrap_or_else(|_| "/run/v3/promote/promote.sock".into()),
        ),
        secure: std::env::var("V3_SECURE_COOKIE").unwrap_or_else(|_| "true".into()) != "false",
        // Checkout-relative: read live (`live-files.json`) on every use.
        chat_ui_config_path: PathBuf::from(webagent_chat::configuration::LIVE_CHAT),
    };
    if role == "loop" {
        let progress = agent_loop::watchdog::Progress::new();
        let _watchdog = agent_loop::watchdog::Watchdog::start(progress.clone())?;
        restore_worker_control(&app).await?;
        recover(&app, true).await?;
        app.ready.store(true, Ordering::Release);
        let mut scheduler = tokio::spawn(futures_util::FutureExt::boxed(scheduler_with_progress(
            app.clone(),
            progress,
        )));
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
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
        for token in app.active.lock().await.values() {
            token.cancel();
        }
        // A stuck gate must not make SIGTERM wait forever.
        if tokio::time::timeout(Duration::from_secs(12), &mut scheduler)
            .await
            .is_err()
        {
            scheduler.abort();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
        while !app.active.lock().await.is_empty() && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let _ = marker(&app, true);
        app.stop.cancel();
        for token in app.active.lock().await.values() {
            token.cancel();
        }
        let _ = app.store.clear_loop_heartbeat().await;
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
    let notification_relay = tokio::spawn(futures_util::FutureExt::boxed(
        api::notifications::service(app.clone()),
    ));
    let administrator_watch = tokio::spawn(futures_util::FutureExt::boxed(
        administrator_services::watch(app.clone()),
    ));
    let shutdown = app.clone();
    axum::serve(
        listener,
        router(app.clone()).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("signal handler");
        tokio::select! {_=signal.recv()=>{},_=tokio::signal::ctrl_c()=>{}}
        shutdown.ready.store(false, Ordering::Release);
        shutdown.stop.cancel();
    })
    .await?;
    app.stop.cancel();
    let _ = administrator_watch.await;
    let _ = notification_relay.await;
    app.store.pool.close().await;
    app.store.agent_pool.close().await;
    app.store.app_pool.close().await;
    Ok(())
}

struct ModelSessionAuthority<'a>(&'a Store);
impl webagent_models::runtime::SessionAuthority for ModelSessionAuthority<'_> {
    fn live_access<'a>(
        &'a self,
        user: &'a str,
        session: &'a str,
        agent: &'a str,
    ) -> impl std::future::Future<Output = Result<access_policy::LiveAccess>> + Send + 'a {
        shared_chat::live_access(self.0, user, session, agent)
    }
}
impl App {
    fn models_runtime(&self) -> webagent_models::runtime::Runtime<'_, Store> {
        webagent_models::runtime::Runtime::new(
            self.store.models_repository(),
            self.provider.transport(),
            &self.gate,
            &self.refresh,
            &self.login,
            &self.stop,
        )
    }
}
// Root host state completes native adapters without teaching Engine authentication fields.
#[derive(Clone)]
struct EngineHostState {
    pub(crate) login_attempts: webagent_accounts::LoginAttempts,
    pub(crate) promotion_socket: PathBuf,
    pub(crate) secure: bool,
    pub(crate) chat_ui_config_path: PathBuf,
    pub(crate) message_api_token_hash: Option<[u8; 32]>,
    pub(crate) browser_login_bridge: Option<browser_login::Bridge>,
}
impl EngineHostState {
    pub(crate) fn new(app: &App) -> Self {
        Self {
            login_attempts: app.login_attempts.clone(),
            promotion_socket: app.promotion_socket.clone(),
            secure: app.secure,
            chat_ui_config_path: app.chat_ui_config_path.clone(),
            message_api_token_hash: app.message_api_token_hash,
            browser_login_bridge: app.browser_login_bridge.clone(),
        }
    }
}
impl EngineHostState {
    pub(crate) fn app(
        &self,
        view: webagent_engine::runtime::native::View<
            '_,
            Store,
            loop_runtime::NativeSchemas,
            abilities::Catalog,
            shared_chat::SessionAccess,
        >,
    ) -> App {
        App {
            store: view.store.clone(),
            abilities: view.catalog.clone(),
            chat_actor: view.actor.map(str::to_owned),
            chat_access: view.binding.cloned(),
            runtime: view.handles.runtime.clone(),
            runtime_path: view.handles.runtime_path.clone(),
            compaction_config_path: view.handles.compaction_config_path.clone(),
            gate: view.handles.gate.clone(),
            active: view.handles.active.clone(),
            slots: view.handles.slots.clone(),
            ready: view.handles.ready.clone(),
            stop: view.handles.stop.clone(),
            control_dir: view.handles.control_dir.clone(),
            provider: view.provider.clone(),
            refresh: view.refresh.clone(),
            login: view.login.clone(),
            tool_socket: view.tool_socket.to_owned(),
            build_socket: view.build_socket.to_owned(),
            login_attempts: self.login_attempts.clone(),
            promotion_socket: self.promotion_socket.clone(),
            secure: self.secure,
            chat_ui_config_path: self.chat_ui_config_path.clone(),
            message_api_token_hash: self.message_api_token_hash,
            browser_login_bridge: self.browser_login_bridge.clone(),
        }
    }
}

#[cfg(test)]
mod engine_host_tests {
    use super::*;

    #[tokio::test]
    async fn current_native_view_overrides_captured_host_defaults() {
        let (_captured_root, captured) = crate::runtime_tests::fixture().await;
        let (_current_root, current) = crate::runtime_tests::fixture().await;
        let host = EngineHostState::new(&captured);
        let handles = webagent_engine::runtime::Handles {
            runtime: current.runtime.clone(),
            runtime_path: current.runtime_path.clone(),
            compaction_config_path: current.compaction_config_path.clone(),
            control_dir: current.control_dir.clone(),
            gate: current.gate.clone(),
            active: current.active.clone(),
            slots: current.slots.clone(),
            ready: current.ready.clone(),
            stop: current.stop.clone(),
        };
        let reconstructed = host.app(webagent_engine::runtime::native::View {
            store: &current.store,
            catalog: &current.abilities,
            actor: Some("current-actor"),
            binding: None,
            handles: &handles,
            provider: &current.provider,
            refresh: &current.refresh,
            login: &current.login,
            tool_socket: &current.tool_socket,
            build_socket: &current.build_socket,
        });
        assert_eq!(reconstructed.store.root, current.store.root);
        assert_ne!(reconstructed.store.root, captured.store.root);
        assert_eq!(reconstructed.chat_actor.as_deref(), Some("current-actor"));
        assert!(reconstructed.chat_access.is_none());
        assert!(Arc::ptr_eq(&reconstructed.abilities, &current.abilities));
        assert!(Arc::ptr_eq(&reconstructed.runtime, &current.runtime));
        assert!(reconstructed
            .login_attempts
            .shares_state_with(&captured.login_attempts));
    }
}
