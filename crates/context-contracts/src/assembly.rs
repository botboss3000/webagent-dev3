//! Pure assembly of a model-call envelope beside the runtime loop.
//! Producers resolve authorization before calling this module. Credentials,
//! dispatch, approvals and durable history writes remain with the runtime.
use anyhow::Result;
use serde::{Serialize,Deserialize};
use serde_json::Value;
use crate::{context_compaction, context_input};

use crate::context_accounting::HistoryReport;

/// Fixed, content-free failures survive the pause boundary.
#[derive(Debug)]
pub struct Failure {pub code:&'static str,pub stage:&'static str}
impl Failure {pub fn new(code:&'static str,stage:&'static str)->Self {Self {code,stage}}}
impl std::fmt::Display for Failure {
 fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {write!(f,"{}: {}",self.stage,self.code)}
}
impl std::error::Error for Failure {}
pub fn failure_code(error:&anyhow::Error)->(&'static str,&'static str) {
 error.downcast_ref::<Failure>().map(|e|(e.code,e.stage)).unwrap_or(("assembly_internal","assembly"))
}

/// Normalize the compactor's existing fixed errors at the assembly boundary.
/// No private reason string is copied into a diagnostic or public run status.
pub fn compaction_error(error:anyhow::Error)->anyhow::Error {
    if error.downcast_ref::<Failure>().is_some(){return error;}
    let (code,stage)=match error.to_string().as_str(){
        "Compaction trigger cannot fit fixed request overhead"=>("trigger_overhead_exceeds_budget","compaction_plan"),
        "Invalid compaction measurements or budget"=>("invalid_measurements","compaction_plan"),
        "Context exceeds budget without a completed turn or tool exchange to summarize"=>("no_safe_boundary","compaction_plan"),
        "Stale or invalid context compaction plan"=>("stale_plan","summary_validation"),
        "Context summary has invalid length"=>("summary_length","summary_validation"),
        "Compacted context did not shrink toward the budget"=>("summary_did_not_shrink","summary_validation"),
        _=>return error,
    };
    Failure::new(code,stage).into()
}

/// Provider-neutral envelope using v3's existing Responses-shaped history.
/// This is deliberately NOT the final provider-specific HTTP request body.
pub struct Envelope<'a> {
    pub input: &'a [Value],
    pub instructions: String,
    pub schemas: &'a [Value],
    pub report: Report,
}
pub enum Prepared<'a> { Compact(context_compaction::Plan), Ready(Envelope<'a>) }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Overlay {
    pub id: String,
    pub source: String,
    pub lifetime: context_input::Lifetime,
    pub disposition: String,
    pub content_bytes: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Schema { pub name: String, pub bytes: usize, pub bootstrap: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Report {
    pub version: u32,
    pub scope: String,
    pub accounting: String,
    pub provider: String,
    pub model: String,
    pub transport: String,
    pub turn: u64,
    pub call: u64,
    pub max_bytes: usize,
    pub history_budget_bytes: usize,
    pub summary_reserve_bytes: usize,
    pub envelope_bytes: usize,
    pub instruction_json_bytes: usize,
    pub base_content_bytes: usize,
    pub guidance_content_bytes: usize,
    pub schemas_json_bytes: usize,
    pub history_json_bytes: usize,
    pub framing_bytes: usize,
    pub history: HistoryReport,
    pub overlays: Vec<Overlay>,
    pub schemas: Vec<Schema>,
}
/// Keep existing base/slot ordering when pinning a run's prompt.
pub fn pinned_instructions(base: &str, slots: &[Value]) -> Result<String> {
    let mut instructions = base.to_owned();
    for slot in slots {
        if let Some(content) = slot["content"].as_str().filter(|s| !s.trim().is_empty()) {
            instructions.push_str("\n\n");
            instructions.push_str(content.trim());
        }
    }
    Ok(instructions)
}
