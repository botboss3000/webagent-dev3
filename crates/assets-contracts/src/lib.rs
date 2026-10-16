//! Versioned pure safety contracts for permanent selections and context placement.
//! The host owns scope, live authorization, storage, leases and transactional effects.
use anyhow::{bail,Result};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::collections::{BTreeMap,BTreeSet};

pub const VERSION:u32=1;
pub const MAX_ITEMS:usize=512;
pub const MAX_BYTES:usize=16*1024*1024;

#[derive(Clone,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default,deny_unknown_fields)]
pub struct State {
 pub revision:u64,
 pub selectors:Vec<Value>,
 pub placements:Vec<Placement>,
 pub schema_order:Vec<String>,
 pub history_basis:String,
 pub receipts:Vec<Receipt>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Placement {
 pub id:String,
 pub after_input:usize,
 #[serde(default,skip_serializing_if="Option::is_none")]
 pub anchor:Option<String>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
 pub request_id:String,
 pub fingerprint:String,
 pub revision:u64,
 pub removed:Option<bool>,
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Operation {
 Activate {selectors:Vec<Value>,expected_revision:u64,request_id:String},
 Unload {selector:Value,expected_revision:u64,request_id:String},
 Rebase {history_basis:String,expected_revision:u64,request_id:String},
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Request {
 Transition {version:u32,state:State,operation:Operation},
 Place {version:u32,state:State,history:Vec<Value>,definitions:Vec<String>,
  #[serde(default)] temporary:Vec<String>,
  schema_names:Vec<String>,history_basis:String,append:bool},
}
#[derive(Clone,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="snake_case",deny_unknown_fields)]
pub enum Plan {
 Transition {state:State,removed:Option<bool>,replayed:bool,result_revision:u64,input_digest:String},
 Place {placements:Vec<Placement>,schema_order:Vec<String>,rebased:bool,input_digest:String},
}
#[derive(Clone,Debug,PartialEq,Eq)]
pub struct Projected {
 pub input:Vec<Value>,
 /// Exact serialized array growth, including separators; not a token estimate.
 pub extra_json_bytes:usize,
}

/// Canonical compact JSON identity binds every plan to its complete immutable input.
pub fn digest<T:Serialize+?Sized>(value:&T)->Result<String> {
 Ok(format!("{:x}",Sha256::digest(serde_json::to_vec(value)?)))
}
/// Accept bounded opaque IDs without allowing control bytes to enter metadata.
pub fn valid_id(value:&str)->bool {
 !value.is_empty()&&value.len()<=256&&!value.chars().any(char::is_control)
}
/// Validate exact selector shapes; hashes/revisions and instruction bodies are forbidden.
pub fn validate_selector(value:&Value)->Result<()> {
 let Some(fields)=value.as_object() else {bail!("asset selector shape");};
 let keys:Vec<&str>=match value["type"].as_str() {
  Some("ability")=>vec!["type","ability_id"],
  Some("tool")=>vec!["type","tool_name"],
  Some("skill")=>{
   if !matches!(value["kind"].as_str(),Some("ability"|"tool"|"selectable"|"own")) {bail!("skill kind");}
   if value["kind"]=="own"&&value["ability_id"]!="base" {bail!("own skill scope");}
   vec!["type","ability_id","kind","skill_id"]
  },
  _=>bail!("asset selector type"),
 };
 if fields.len()!=keys.len()||keys.iter().any(|key|!fields.get(*key).and_then(Value::as_str).is_some_and(valid_id)) {bail!("asset selector fields");}
 Ok(())
}
/// Validate unique bounded names while retaining caller-defined order.
pub fn validate_names(names:&[String])->Result<()> {
 if names.len()>MAX_ITEMS {bail!("asset names limit");}
 let mut seen=BTreeSet::new();
 for name in names {if !valid_id(name)||!seen.insert(name) {bail!("asset names");}}
 Ok(())
}
/// Allow only stable runtime-call or immutable-item digest anchors.
pub fn validate_anchor(anchor:&str)->Result<()> {
 let valid=if let Some(id)=anchor.strip_prefix("call:") {valid_id(id)}
 else if let Some(hash)=anchor.strip_prefix("item:") {hash.len()==64&&hash.bytes().all(|b|b.is_ascii_hexdigit())}
 else {false};
 if !valid {bail!("asset anchor");} Ok(())
}
/// Reject corrupt or oversized canonical state before any projection or mutation.
pub fn validate_state(state:&State)->Result<()> {
 if state.selectors.len()>256||state.placements.len()>MAX_ITEMS||state.receipts.len()>MAX_ITEMS||state.history_basis.len()>256 {bail!("asset state limit");}
 let mut selectors=BTreeSet::new();
 for value in &state.selectors {validate_selector(value)?;if !selectors.insert(digest(value)?) {bail!("duplicate selector");}}
 validate_names(&state.schema_order)?;
 let names:Vec<String>=state.placements.iter().map(|p|p.id.clone()).collect();validate_names(&names)?;
 for item in &state.placements {if let Some(anchor)=&item.anchor {validate_anchor(anchor)?;}}
 let mut receipts=BTreeSet::new();
 for receipt in &state.receipts {
  if !valid_id(&receipt.request_id)||receipt.fingerprint.len()!=64||!receipt.fingerprint.bytes().all(|b|b.is_ascii_hexdigit())
   ||receipt.revision>state.revision||!receipts.insert(&receipt.request_id) {bail!("asset receipt");}
 }
 Ok(())
}
/// Return immutable mutation binding shared by component and host validation.
pub fn operation_identity(operation:&Operation)->(&str,u64) {
 match operation {
  Operation::Activate{request_id,expected_revision,..}|Operation::Unload{request_id,expected_revision,..}
  |Operation::Rebase{request_id,expected_revision,..}=>(request_id,*expected_revision),
 }
}
/// Validate bounded wire input without inferring authority from selected identities.
pub fn validate_request(request:&Request)->Result<()> {
 if serde_json::to_vec(request)?.len()>MAX_BYTES {bail!("asset request bytes");}
 match request {
  Request::Transition{version,state,operation}=>{
   if *version!=VERSION {bail!("asset version");}validate_state(state)?;
   let (id,_)=operation_identity(operation);if !valid_id(id) {bail!("asset request id");}
   match operation {
    Operation::Activate{selectors,..}=>{if selectors.len()>256 {bail!("activation limit");}for item in selectors {validate_selector(item)?;}},
    Operation::Unload{selector,..}=>validate_selector(selector)?,
    Operation::Rebase{history_basis,..}=>if history_basis.len()>256 {bail!("history basis");},
   }
  },
  Request::Place{version,state,history,definitions,temporary,schema_names,history_basis,..}=>{
   if *version!=VERSION||history.len()>65_536||history_basis.len()>256 {bail!("asset placement request");}
   validate_state(state)?;
   let names:Vec<String>=definitions.iter().chain(temporary).cloned().collect();
   validate_names(&names)?;validate_names(schema_names)?;safe_boundaries(history)?;
  }
 }Ok(())
}
/// Only complete canonical function-call exchanges permit an inserted instruction.
pub fn safe_boundaries(history:&[Value])->Result<Vec<usize>> {
 let mut pending=BTreeSet::new();let mut safe=vec![0];
 for (index,item) in history.iter().enumerate() {
  if !item.is_object() {bail!("history item shape");}
  match item["type"].as_str() {
   Some("function_call")=>{
    let id=item["call_id"].as_str().filter(|id|valid_id(id)).ok_or_else(||anyhow::anyhow!("history call id"))?;
    if !pending.insert(id) {bail!("duplicate history call");}
   },
   Some("function_call_output")=>{
    let id=item["call_id"].as_str().ok_or_else(||anyhow::anyhow!("history output id"))?;
    if !pending.remove(id) {bail!("orphan history output");}
   },
   _=>{}
  }
  if pending.is_empty()&&item["type"]!="reasoning" {safe.push(index+1);}
 }
 Ok(safe)
}
/// Bind a boundary to its previous durable item; zero is the permanent prefix.
pub fn boundary_anchor(history:&[Value],after:usize)->Result<Option<String>> {
 if after==0 {return Ok(None);}
 let item=history.get(after-1).ok_or_else(||anyhow::anyhow!("boundary out of bounds"))?;
 Ok(Some(if item["type"]=="function_call_output" {
  format!("call:{}",item["call_id"].as_str().ok_or_else(||anyhow::anyhow!("output id"))?)
 } else {format!("item:{}",digest(item)?)}))
}
/// Resolve one unique immutable anchor; ambiguity deliberately withdraws the old position.
pub fn anchor_position(history:&[Value],anchor:&str)->Result<Option<usize>> {
 validate_anchor(anchor)?;let safe=safe_boundaries(history)?;
 let mut result=None;
 for after in safe.into_iter().filter(|position|*position>0) {
  if boundary_anchor(history,after)?.as_deref()==Some(anchor) {
   if result.is_some() {return Ok(None);}result=Some(after);
  }
 }Ok(result)
}
/// Check exact mutation semantics so a service cannot add unrequested identities or receipts.
fn validate_transition(state:&State,operation:&Operation,candidate:&State,removed:Option<bool>,replayed:bool)->Result<()> {
 let (id,expected)=operation_identity(operation);let fingerprint=digest(operation)?;
 if let Some(receipt)=state.receipts.iter().find(|r|r.request_id==id) {
  if receipt.fingerprint!=fingerprint||candidate!=state||removed!=receipt.removed||!replayed {bail!("asset receipt replay mismatch");}
  return Ok(());
 }
 if expected!=state.revision||replayed||state.receipts.len()>=MAX_ITEMS {bail!("asset revision or quota");}
 let mut required=state.clone();
 let expected_removed=match operation {
  Operation::Activate{selectors,..}=>{for item in selectors {if !required.selectors.contains(item){required.selectors.push(item.clone());}}None},
  Operation::Unload{selector,..}=>{
   let before=required.selectors.len();required.selectors.retain(|item|item!=selector);
   required.placements.clear();required.schema_order.clear();Some(before!=required.selectors.len())
  },
  Operation::Rebase{history_basis,..}=>{required.placements.clear();required.schema_order.clear();required.history_basis=history_basis.clone();None},
 };
 required.revision=state.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("asset revision overflow"))?;
 required.receipts.push(Receipt{request_id:id.into(),fingerprint,revision:required.revision,removed:expected_removed});
 if candidate!=&required||removed!=expected_removed {bail!("asset transition mismatch");}
 validate_state(candidate)
}
/// Validate request binding, allowed state changes, definition references and safe boundaries.
pub fn validate_plan(request:&Request,plan:&Plan)->Result<()> {
 validate_request(request)?;
 match (request,plan) {
  (Request::Transition{state,operation,..},Plan::Transition{state:candidate,removed,replayed,result_revision,input_digest})=>{
   if *input_digest!=digest(request)? {bail!("asset input digest");}
   validate_transition(state,operation,candidate,*removed,*replayed)?;
   let (id,_)=operation_identity(operation);
   let effect=candidate.receipts.iter().find(|receipt|receipt.request_id==id).ok_or_else(||anyhow::anyhow!("asset result receipt"))?;
   if *result_revision!=effect.revision {bail!("asset result revision");}
  },
  (Request::Place{history,definitions,temporary,schema_names,history_basis,state,append,..},
   Plan::Place{placements,schema_order,rebased,input_digest})=>{
   if *input_digest!=digest(request)?||placements.len()!=definitions.len()+temporary.len() {bail!("placement binding");}
   validate_names(schema_order)?;
   if schema_order.iter().collect::<BTreeSet<_>>()!=schema_names.iter().collect::<BTreeSet<_>>() {bail!("schema references");}
   let safe=safe_boundaries(history)?;
   let suffix=*safe.last().unwrap_or(&0);
   for (index,(placement,id)) in placements.iter().zip(definitions.iter().chain(temporary)).enumerate() {
    if &placement.id!=id||!safe.contains(&placement.after_input)
     ||placement.anchor!=boundary_anchor(history,placement.after_input)? {bail!("unsafe definition placement");}
    if index<definitions.len() {
     if (!append||state.history_basis!=*history_basis)&&placement.after_input!=0 {bail!("placement requires prefix");}
    } else if placement.after_input!=suffix {bail!("temporary placement requires suffix");}
   }
   if state.history_basis!=*history_basis&&!*rebased {bail!("history rebase flag");}
  },
  _=>bail!("asset plan kind"),
 }Ok(())
}
/// Reconstruct only supplied definitions around unchanged history and charge array growth once.
pub fn project(request:&Request,plan:&Plan,bodies:&BTreeMap<String,String>,role:&str)->Result<Projected> {
 validate_plan(request,plan)?;
 if !matches!(role,"system"|"developer") {bail!("instruction role");}
 let (history,definitions,temporary,placements)=match (request,plan) {
  (Request::Place{history,definitions,temporary,..},Plan::Place{placements,..})=>(history,definitions,temporary,placements),
  _=>bail!("projection requires placement"),
 };
 if bodies.keys().collect::<BTreeSet<_>>()!=definitions.iter().chain(temporary).collect::<BTreeSet<_>>() {bail!("definition bodies mismatch");}
 let mut groups:BTreeMap<usize,Vec<&str>>=BTreeMap::new();
 for placement in placements {groups.entry(placement.after_input).or_default().push(&bodies[&placement.id]);}
 let mut input=Vec::with_capacity(history.len()+groups.len());
 for after in 0..=history.len() {
  if let Some(texts)=groups.get(&after) {input.push(json!({"role":role,"content":texts.join("\n\n")}));}
  if let Some(item)=history.get(after) {input.push(item.clone());}
 }
 let bytes=serde_json::to_vec(&input)?.len();if bytes>MAX_BYTES {bail!("projected context bytes");}
 Ok(Projected{input,extra_json_bytes:bytes.saturating_sub(serde_json::to_vec(history)?.len())})
}

#[cfg(test)]
mod tests {
 use super::*;
 /// Strict identities never admit a frozen body or content revision.
 #[test]fn rejects_body_and_revision() {
  assert!(validate_selector(&json!({"type":"skill","ability_id":"base","kind":"own","skill_id":"review","sha256":"old"})).is_err());
  assert!(validate_selector(&json!({"type":"skill","ability_id":"base","kind":"own","skill_id":"review"})).is_ok());
 }
 /// Parallel calls are indivisible until every pending result arrives.
 #[test]fn completed_exchange_boundaries() {
  let history=vec![json!({"type":"function_call","call_id":"a"}),json!({"type":"function_call","call_id":"b"}),
   json!({"type":"function_call_output","call_id":"a"}),json!({"type":"function_call_output","call_id":"b"})];
  assert_eq!(safe_boundaries(&history).unwrap(),vec![0,4]);
  assert!(safe_boundaries(&[json!({"type":"function_call_output","call_id":"missing"})]).is_err());
 }
}
