//! Owned live adapter uses the same algorithm as linked callers.
use anyhow::Result;
use crate::{context_input::Request,context_assembly::Prepared,context_accounting::ContextAccounting};
pub use webagent_context_contracts::protocol::*;
/// Restore ephemeral host-resolved policy after transport decoding; policy storage stays outside this crate.
pub fn prepare_owned(request:&PrepareRequest,state:&mut ContextAccounting)->Result<PreparedResult> {
 state.projection_extra_bytes=request.projection_extra_bytes;
 state.contributions_projected=request.contributions_projected;
 let mut config=request.compaction.clone();config.system_input_budget=request.system_input_budget;config.summary_instructions=request.summary_instructions.clone();config.validate()?;
 let result=crate::context_assembly::prepare_incremental(Request {history:&request.history,base:&request.base,guidance:&request.guidance,schemas:&request.schemas,contributions:&request.contributions,identity:request.identity,model:request.model.clone(),max_bytes:request.max_bytes,compaction:&config},state)?;
 Ok(match result {Prepared::Ready(envelope)=>PreparedResult::Ready{instructions:envelope.instructions,report:envelope.report},Prepared::Compact(plan)=>PreparedResult::Compact{plan}})
}
#[cfg(test)]
mod tests {
 use super::*;
 use serde_json::Value;
 use crate::{context_input::{CallIdentity,Contribution,ModelMetadata},context_compaction::Config};
 use serde_json::json;
 /// Construct a valid bounded owned request shared by transport and accounting regressions.
 fn request(history:Vec<Value>)->PrepareRequest {
  let compaction:Config=serde_json::from_value(json!({"schema_version":1,"trigger_tokens":10000,"tail_tokens":40,"summary_tokens":20,"characters_per_token":4,"summary_reserve_bytes":300,"max_source_bytes":262144,"max_summary_bytes":300,"summary_prefix":"Earlier conversation summary (background context, not a new request):\n","active_summary_prefix":"Active task progress summary (background context, not a new request):\n","summary_prompt":"Summarize under {summary_characters} characters. Conversation JSON: {source}"})).unwrap();
  PrepareRequest{history,base:"base é🌍".into(),guidance:" guidance".into(),schemas:vec![json!({"name":"read","parameters":{}})],contributions:vec![Contribution{id:"mode".into(),source:"trusted".into(),lifetime:crate::context_input::Lifetime::Call,content:"overlay".into(),turn:1,call:2}],identity:CallIdentity{turn:1,call:2},model:ModelMetadata{provider:"test".into(),model:"resolved".into(),transport:"responses".into()},max_bytes:4096,compaction,projection_extra_bytes:0,contributions_projected:false,system_input_budget:None,summary_instructions:"summary instructions".into()}
 }
 #[test]
 fn transport_roundtrip_matches_borrowed_linked_and_never_returns_history() {
  let request=request(vec![json!({"role":"user","content":"private transcript é🌍\""})]);
  let decoded:PrepareRequest=serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
  let owned=prepare_owned(&decoded,&mut ContextAccounting::default()).unwrap();
  let mut config=request.compaction.clone();config.summary_instructions=request.summary_instructions.clone();
  let Prepared::Ready(linked)=crate::context_assembly::prepare(Request{history:&request.history,base:&request.base,guidance:&request.guidance,schemas:&request.schemas,contributions:&request.contributions,identity:request.identity,model:request.model.clone(),max_bytes:request.max_bytes,compaction:&config}).unwrap() else {panic!("expected ready")};
  let PreparedResult::Ready{instructions,report}=owned else {panic!("expected ready")};
  assert_eq!(instructions,"base é🌍 guidance\noverlay");
  assert_eq!(instructions,linked.instructions);
  assert_eq!(serde_json::to_value(&report).unwrap(),serde_json::to_value(linked.report).unwrap());
  let serialized=serde_json::to_string(&PreparedResult::Ready{instructions,report}).unwrap();
  assert!(!serialized.contains("private transcript"));
  let _:PreparedResult=serde_json::from_str(&serialized).unwrap();
 }
 #[test]
 fn owned_append_cache_and_reconstruction_have_identical_results() {
  let mut request=request(vec![json!({"role":"user","content":"é🌍"})]);
  let mut cache=ContextAccounting::default();prepare_owned(&request,&mut cache).unwrap();
  prepare_owned(&request,&mut cache).unwrap();assert_eq!(cache.measured_items,1);
  request.history.push(json!({"role":"assistant","content":"done"}));
  let incremental=prepare_owned(&request,&mut cache).unwrap();assert_eq!(cache.measured_items,2);
  let rebuilt=prepare_owned(&request,&mut ContextAccounting::default()).unwrap();
  assert_eq!(serde_json::to_value(incremental).unwrap(),serde_json::to_value(rebuilt).unwrap());
 }
 #[test]
 fn compaction_roundtrip_preserves_boundary_and_rejects_stale_summary() {
  let mut request=request(vec![json!({"role":"user","content":"é🌍".repeat(600)}),json!({"role":"assistant","content":"done"}),json!({"role":"user","content":"continue"})]);
  request.compaction.trigger_tokens=150;request.compaction.tail_tokens=40;
  let result=prepare_owned(&request,&mut ContextAccounting::default()).unwrap();
  let wire=serde_json::to_value(result).unwrap();
  let PreparedResult::Compact{plan}=serde_json::from_value(wire).unwrap() else {panic!("expected compact")};
  assert_eq!(plan.drop_until,2);
  let mut effective=request.compaction.clone();effective.summary_instructions=request.summary_instructions.clone();
  crate::context_compaction::validate_external_plan(&request.history,&plan,&effective,plan.max_bytes).unwrap();
  let mut forged=plan.clone();forged.reserve_bytes+=1;
  assert!(crate::context_compaction::validate_external_plan(&request.history,&forged,&effective,plan.max_bytes).is_err());
  let mut forged=plan.clone();forged.summary_instructions="untrusted".into();
  assert!(crate::context_compaction::validate_external_plan(&request.history,&forged,&effective,plan.max_bytes).is_err());
  let mut wire=serde_json::to_value(&plan).unwrap();wire["config"]["max_summary_bytes"]=json!(65536);
  let forged=serde_json::from_value(wire).unwrap();
  assert!(crate::context_compaction::validate_external_plan(&request.history,&forged,&effective,plan.max_bytes).is_err());
  let compacted=crate::context_compaction::apply_summary(&request.history,&plan,"prior work complete").unwrap();
  assert_eq!(&compacted[1..],&request.history[2..]);
  request.history[0]=json!({"role":"user","content":"changed"});
  assert!(crate::context_compaction::apply_summary(&request.history,&plan,"memory").is_err());
 }
 #[test]
 fn resolved_summary_budget_survives_transport() {
  let mut request=request(vec![json!({"role":"user","content":"x".repeat(2000)}),json!({"role":"assistant","content":"done"}),json!({"role":"user","content":"continue"})]);
  request.system_input_budget=Some(10);
  let decoded:PrepareRequest=serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
  assert_eq!(decoded.system_input_budget,Some(10));
  assert!(prepare_owned(&decoded,&mut ContextAccounting::default()).is_err());
 }
 /// Synthetic messages are charged once while call-scoped contributions retain their report lifetime.
 #[test]
 fn projected_growth_matches_exact_envelope_without_duplicating_native_contributions() {
  let mut request=request(vec![json!({"role":"user","content":"canonical é"})]);
  let mut projected=request.history.clone();
  projected.push(json!({"role":"developer","content":"overlay and current skill é🌍"}));
  request.projection_extra_bytes=crate::context_accounting::json_size(&projected).unwrap()
   -crate::context_accounting::json_size(&request.history).unwrap();
  request.contributions_projected=true;
  let PreparedResult::Ready{instructions,report}=prepare_owned(&request,&mut ContextAccounting::default()).unwrap()
   else {panic!("expected ready")};
  assert_eq!(instructions,"base é🌍 guidance");
  let envelope=json!({"instructions":instructions,"tools":request.schemas,"input":projected});
  assert_eq!(report.envelope_bytes,crate::context_accounting::json_size(&envelope).unwrap());
  assert_eq!(report.projection_extra_bytes,request.projection_extra_bytes);
  assert_eq!(report.overlays.len(),1);
  assert_eq!(report.overlays[0].disposition,"included");
  assert_eq!(request.history.len(),1);
 }
 /// Empty durable input contributes its two brackets exactly once, including through owned transport.
 #[test]
 fn empty_history_envelope_accounts_brackets_once() {
  let request=request(Vec::new());
  let PreparedResult::Ready{instructions,report}=prepare_owned(&request,&mut ContextAccounting::default()).unwrap()
   else {panic!("expected ready")};
  let envelope=json!({"instructions":instructions,"tools":request.schemas,"input":[]});
  assert_eq!(report.history_json_bytes,2);
  assert_eq!(report.envelope_bytes,crate::context_accounting::json_size(&envelope).unwrap());
 }

}
