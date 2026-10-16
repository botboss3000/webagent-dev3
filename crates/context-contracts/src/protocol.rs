//! Owned transport contracts. Ready results never copy private history back to the host.
use serde::{Serialize,Deserialize};
use serde_json::Value;
use crate::{context_input::{CallIdentity,Contribution,ModelMetadata},context_compaction::{Config,Plan},context_assembly::Report};
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
 pub history:Vec<Value>,pub base:String,pub guidance:String,pub schemas:Vec<Value>,pub contributions:Vec<Contribution>,pub identity:CallIdentity,
 pub model:ModelMetadata,pub max_bytes:usize,pub compaction:Config,
 pub projection_extra_bytes:usize,pub contributions_projected:bool,
 pub system_input_budget:Option<usize>,pub summary_instructions:String,
}
#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum PreparedResult {Ready {instructions:String,report:Report},Compact {plan:Plan}}
