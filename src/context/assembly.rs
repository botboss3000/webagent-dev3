//! Core compatibility seam: resolve provider metadata, then borrow inputs into the library.
#[cfg(any(feature="linked-context",test))] use anyhow::Result;
#[cfg(test)] use serde_json::{json,Value};
#[cfg(test)] use crate::{context_compaction,provider};
#[cfg(any(feature="linked-context",test))] use crate::context_input::Request;
#[cfg(test)] use crate::context_input::CallIdentity;
pub use webagent_context_contracts::context_assembly::{Failure,failure_code,compaction_error,Envelope,Prepared,pinned_instructions};
/// Resolve only reporting metadata; credentials and provider execution stay in the core.
pub fn model_metadata(selection:&crate::provider::ProviderSelection)->webagent_context_contracts::context_input::ModelMetadata {
 let value=selection.value();
 webagent_context_contracts::context_input::ModelMetadata {provider:value["provider"].as_str().unwrap_or("").into(),model:selection.model.clone(),
 transport:match selection.provider {crate::provider::ProviderId::OpenaiSubscription=>"responses",_=>"chat_completions"}.into()}
}
#[cfg(test)]
/// Preserve the existing test/diagnostics convenience path with fresh disposable accounting.
pub fn prepare<'a>(request:Request<'a>)->Result<Prepared<'a>> {prepare_incremental(request,&mut crate::context_accounting::ContextAccounting::default())}
#[cfg(any(feature="linked-context",test))]
/// Bridge the core provider selection to the linked algorithm without copying private history.
pub fn prepare_incremental<'a>(request:Request<'a>,state:&mut crate::context_accounting::ContextAccounting)->Result<Prepared<'a>> {
 let model=model_metadata(request.selection);
 webagent_context_assembly::context_assembly::prepare_incremental(webagent_context_contracts::context_input::Request {
 history:request.history,base:request.base,guidance:request.guidance,schemas:request.schemas,contributions:request.contributions,
 identity:request.identity,model,max_bytes:request.max_bytes,compaction:&request.compaction.0},state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_input::{Contribution, Lifetime};
    fn selection() -> provider::ProviderSelection {
        provider::ProviderSelection::parse(&json!({"provider":"deepseek","model":"deepseek-chat"}), "unused").unwrap()
    }
    fn test_config(reserve: usize) -> context_compaction::Config {
        let mut c = context_compaction::Config::configured().unwrap();
        c.summary_reserve_bytes=reserve; c
    }
    #[test]
    fn preserves_projection_and_reports_without_content() {
        let history = vec![json!({"role":"user","content":"private transcript"}),
            json!({"type":"function_call","call_id":"c","name":"read","arguments":"private arguments"}),
            json!({"type":"function_call_output","call_id":"c","output":"private output"})];
        let c = Contribution { id: "mode".into(), source: "native_mode".into(),
            lifetime: Lifetime::Call, content: "private overlay".into(), turn: 1, call: 2 };
        let contributions = [c.clone(), c.clone(), Contribution { call: 1, id: "old".into(), ..c }];
        let schemas = [json!({"name":"load_ability","parameters":{"description":"private schema"}})];
        let selection = selection();
        let config = test_config(100);
        let Prepared::Ready(envelope) = prepare(Request { history: &history, base: "private base",
            guidance: " private skill", schemas: &schemas, contributions: &contributions,
            identity: CallIdentity { turn: 1, call: 2 }, selection: &selection,
            max_bytes: 4096, compaction: &config }).unwrap() else { panic!("unexpected compaction") };
        assert_eq!(envelope.instructions, "private base private skill\nprivate overlay");
        assert_eq!(envelope.input, history);
        assert_eq!(envelope.schemas, schemas);
        let report = serde_json::to_value(&envelope.report).unwrap();
        assert!(!report.to_string().contains("private"));
        assert_eq!(report["transport"], "chat_completions");
        assert_eq!(report["overlays"][1]["disposition"], "duplicate");
        assert_eq!(report["overlays"][2]["disposition"], "expired");
        assert_eq!(envelope.report.envelope_bytes, envelope.report.instruction_json_bytes +
            envelope.report.schemas_json_bytes + envelope.report.history_json_bytes + envelope.report.framing_bytes);
        assert_eq!(envelope.report.history.argument_content_bytes, "private arguments".len());
        assert_eq!(envelope.report.history.result_content_bytes, "private output".len());
        assert!(envelope.report.schemas[0].bootstrap);
    }
    #[test]
    fn compacts_complete_turns_and_accounts_for_overlays() {
        let history = vec![json!({"role":"user","content":"x".repeat(2000)}),
            json!({"role":"assistant","content":"done"}), json!({"role":"user","content":"latest"})];
        let selection = selection();
        let Prepared::Compact(plan) = prepare(Request { history: &history, base: "base", guidance: "",
            schemas: &[], contributions: &[], identity: CallIdentity { turn: 1, call: 1 },
            selection: &selection, max_bytes: 600, compaction: &test_config(100) }).unwrap()
            else { panic!("expected compaction") };
        assert_eq!(plan.drop_until, 2);
        let overlay = Contribution { id: "huge".into(), source: "native".into(), lifetime: Lifetime::Run,
            content: "x".repeat(600), turn: 0, call: 0 };
        assert!(prepare(Request { history: &history, base: "base", guidance: "", schemas: &[],
            contributions: &[overlay], identity: CallIdentity { turn: 1, call: 1 },
            selection: &selection, max_bytes: 600, compaction: &test_config(100) }).is_err());
    }
    #[test]
    fn large_administrator_instructions_use_whole_request_budget() {
        let base="a".repeat(110000);let guidance="g".repeat(50000);
        let selection=selection();let mut config=test_config(100);
        config.trigger_tokens=200000;config.tail_tokens=10000;
        let request=Request{history:&[],base:&base,guidance:&guidance,schemas:&[],contributions:&[],
            identity:CallIdentity{turn:1,call:1},selection:&selection,max_bytes:300000,compaction:&config};
        assert_eq!(pinned_instructions(&base,&[]).unwrap(),base);
        let Prepared::Ready(envelope)=prepare(request).unwrap() else {panic!("unexpected compaction")};
        assert_eq!(envelope.instructions,format!("{base}{guidance}"));
        let error=prepare(Request{max_bytes:160000,..request}).err().unwrap();
        assert_eq!(failure_code(&error),("overhead_exceeds_budget","budget"));
    }

    #[test]
    fn pinned_prompt_keeps_order_and_ignores_empty_slots() {
        assert_eq!(pinned_instructions("base", &[json!({"content":" a "}),
            json!({"content":" "}), json!({"content":"b"})]).unwrap(), "base\n\na\n\nb");
    }
    #[test]
    fn json_policy_triggers_below_request_ceiling() {
        let mut config = context_compaction::Config::configured().unwrap();
        config.trigger_tokens = 150; config.tail_tokens = 40; config.summary_tokens = 20;
        config.summary_reserve_bytes = 300;
        let history = vec![json!({"role":"user","content":"x".repeat(1000)}),
            json!({"role":"assistant","content":"done"}), json!({"role":"user","content":"new"})];
        let selection = selection();
        let Prepared::Compact(plan) = prepare(Request { history: &history, base: "base", guidance: "",
            schemas: &[], contributions: &[], identity: CallIdentity {turn:1,call:1},
            selection: &selection, max_bytes:4096, compaction:&config }).unwrap() else { panic!("expected history trigger") };
        assert_eq!(plan.reserve_bytes, 300);
        assert_eq!(plan.drop_until, 2);
    }

    #[test]
    fn whole_request_trigger_counts_instructions_schemas_and_call_guidance() {
        let mut config = test_config(300);
        config.trigger_tokens = 500;
        config.tail_tokens = 100;
        config.summary_tokens = 50;
        let history = vec![json!({"role":"user","content":"x".repeat(1100)}),
            json!({"role":"assistant","content":"done"}), json!({"role":"user","content":"latest"})];
        let selection = selection();
        let identity = CallIdentity {turn:1,call:1};
        let request = Request {history:&history,base:"base",guidance:"",schemas:&[],
            contributions:&[],identity,selection:&selection,max_bytes:8192,compaction:&config};
        assert!(matches!(prepare(request).unwrap(), Prepared::Ready(_)));
        let schemas = [json!({"name":"file_read","description":"s".repeat(500)})];
        let overlay = Contribution {id:"mode".into(),source:"native".into(),lifetime:Lifetime::Call,
            content:"o".repeat(200),turn:1,call:1};
        let base = "i".repeat(200);
        let contributions = [overlay];
        let Prepared::Compact(plan) = prepare(Request {history:&history,base:&base,
            guidance:"g".repeat(100).as_str(),schemas:&schemas,contributions:&contributions,
            identity,selection:&selection,max_bytes:8192,compaction:&config}).unwrap()
            else {panic!("whole request should trigger before history alone");};
        assert_eq!(plan.drop_until,2);
        assert_eq!(plan.source,history[..2]);
        let compacted = context_compaction::apply_summary(&history,&plan,"Prior work completed.").unwrap();
        assert_eq!(&compacted[1..],&history[2..]);
        assert!(matches!(prepare(Request {history:&compacted,base:&base,guidance:"",schemas:&schemas,
            contributions:&contributions,identity,selection:&selection,max_bytes:8192,compaction:&config}).unwrap(),Prepared::Ready(_)));
    }
    #[test]
    fn fixed_overhead_above_trigger_pauses_without_folding_history() {
        let mut config = test_config(100);
        config.trigger_tokens = 500;config.tail_tokens = 100;config.summary_tokens = 50;
        let selection = selection();
        let history = vec![json!({"role":"user","content":"old"}),json!({"role":"user","content":"latest"})];
        let base = "i".repeat(2200);
        let error = prepare(Request {history:&history,base:&base,guidance:"",schemas:&[],
            contributions:&[],identity:CallIdentity {turn:1,call:1},selection:&selection,
            max_bytes:8192,compaction:&config}).err().unwrap();
        assert_eq!(failure_code(&error),("trigger_overhead_exceeds_budget","compaction_plan"));
        assert_eq!(history[0]["content"],"old");
    }

}
