//! Pure replaceable planning: no file, store, provider, credential or authorization access.
use anyhow::{bail,Result};
use webagent_assets_contracts::{self as contract,Operation,Placement,Plan,Receipt,Request,State};
pub const IMPLEMENTATION_VERSION:&str=env!("CARGO_PKG_VERSION");

/// Apply an already-authorized operation to a snapshot; the host still commits with CAS.
fn transition(state:&State,operation:&Operation,input_digest:String)->Result<Plan> {
 let (id,expected)=contract::operation_identity(operation);
 let fingerprint=contract::digest(operation)?;
 if let Some(receipt)=state.receipts.iter().find(|receipt|receipt.request_id==id) {
  if receipt.fingerprint!=fingerprint {bail!("asset request id reused");}
  return Ok(Plan::Transition{state:state.clone(),removed:receipt.removed,replayed:true,result_revision:receipt.revision,input_digest});
 }
 if expected!=state.revision||state.receipts.len()>=contract::MAX_ITEMS {bail!("asset revision or receipt limit");}
 let mut next=state.clone();
 let removed=match operation {
  Operation::Activate{selectors,..}=>{
   for selector in selectors {if !next.selectors.contains(selector){next.selectors.push(selector.clone());}}
   None
  },
  Operation::Unload{selector,..}=>{
   let before=next.selectors.len();next.selectors.retain(|selected|selected!=selector);
   next.placements.clear();next.schema_order.clear();Some(before!=next.selectors.len())
  },
  Operation::Rebase{history_basis,..}=>{
   next.placements.clear();next.schema_order.clear();next.history_basis=history_basis.clone();None
  },
 };
 next.revision=next.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("asset revision overflow"))?;
 next.receipts.push(Receipt{request_id:id.into(),fingerprint,revision:next.revision,removed});
 let result_revision=next.revision;
 Ok(Plan::Transition{state:next,removed,replayed:false,result_revision,input_digest})
}

/// Preserve surviving schema order, then append genuinely new definitions deterministically.
fn schemas(state:&State,current:&[String])->Vec<String> {
 let mut ordered:Vec<String>=state.schema_order.iter().filter(|name|current.contains(name)).cloned().collect();
 for name in current {if !ordered.contains(name){ordered.push(name.clone());}}
 ordered
}

/// Keep old anchored placements; new blocks append only after a completed exchange.
fn place(request:&Request,input_digest:String)->Result<Plan> {
 let Request::Place{state,history,definitions,temporary,schema_names,history_basis,append,..}=request else {bail!("placement request");};
 let basis_changed=state.history_basis!=*history_basis;
 let initial=state.placements.is_empty();
 let safe=contract::safe_boundaries(history)?;
 let last=*safe.last().unwrap_or(&0);
 let mut rebased=basis_changed;
 let mut placements=Vec::with_capacity(definitions.len()+temporary.len());
 for id in definitions {
  let previous=state.placements.iter().find(|placement|&placement.id==id);
  let after_input=if !append||basis_changed||initial {0}
   else if let Some(previous)=previous {
    if previous.after_input==0 {0}
    else if let Some(anchor)=&previous.anchor {
     match contract::anchor_position(history,anchor)? {Some(position)=>position,None=>{rebased=true;0}}
    } else {rebased=true;0}
   } else {last};
  placements.push(Placement{id:id.clone(),after_input,anchor:contract::boundary_anchor(history,after_input)?});
 }
 // Temporary IDs are deliberately never looked up in prior permanent placements.
 for id in temporary {
  placements.push(Placement{id:id.clone(),after_input:last,anchor:contract::boundary_anchor(history,last)?});
 }
 Ok(Plan::Place{placements,schema_order:schemas(state,schema_names),rebased,input_digest})
}

/// Produce a request-bound plan and run the same fixed safety validator used by the host.
pub fn plan(request:&Request)->Result<Plan> {
 contract::validate_request(request)?;
 let input_digest=contract::digest(request)?;
 let result=match request {
  Request::Transition{state,operation,..}=>transition(state,operation,input_digest)?,
  Request::Place{..}=>place(request,input_digest)?,
 };
 contract::validate_plan(request,&result)?;
 Ok(result)
}

#[cfg(test)]
mod tests {
 use super::*;
 use serde_json::json;
 use std::collections::BTreeMap;
 /// Use complete tool exchanges so placement tests cannot mask unsafe split boundaries.
 fn history()->Vec<serde_json::Value> {
  vec![json!({"role":"user","content":"hello"}),json!({"type":"function_call","call_id":"a"}),
   json!({"type":"function_call_output","call_id":"a","output":"ack"})]
 }
 /// Exercise the same serialized input/output seam used by the thin service callback.
 fn service_callback(value:serde_json::Value)->Result<serde_json::Value> {
  let request:Request=serde_json::from_value(value)?;
  Ok(json!({"status":"success","plan":plan(&request)?}))
 }
 /// Stable placements survive turns, and fresh definitions append without disturbing old prefixes.
 #[test]fn append_and_projection_charge_once() {
  let mut state=State{history_basis:"epoch".into(),placements:vec![Placement{id:"main".into(),after_input:0,anchor:None}],..Default::default()};
  state.schema_order=vec!["old".into()];
  let request=Request::Place{version:1,temporary:vec![],state,history:history(),definitions:vec!["main".into(),"skill".into()],
   schema_names:vec!["new".into(),"old".into()],history_basis:"epoch".into(),append:true};
  let result=plan(&request).unwrap();
  let Plan::Place{placements,schema_order,..}=&result else {panic!()};
  assert_eq!(placements.iter().map(|item|item.after_input).collect::<Vec<_>>(),vec![0,3]);
  assert_eq!(schema_order,&vec!["old".to_owned(),"new".to_owned()]);
  let bodies=BTreeMap::from([("main".into(),"core".into()),("skill".into(),"fresh skill".into())]);
  let projected=contract::project(&request,&result,&bodies,"developer").unwrap();
  assert_eq!(projected.input.len(),5);
  let Request::Place{history,..}=&request else {panic!()};
  assert_eq!(projected.extra_json_bytes,serde_json::to_vec(&projected.input).unwrap().len()-serde_json::to_vec(history).unwrap().len());
  assert_eq!(&projected.input[1..4],history.as_slice());
  assert_eq!(projected.input[4]["content"],"fresh skill");
 }
 /// Compaction epochs and prefix-only providers consolidate all definitions safely.
 #[test]fn epoch_and_prefix_rebase() {
  let state=State{history_basis:"old".into(),placements:vec![Placement{id:"main".into(),after_input:3,anchor:Some("call:a".into())}],..Default::default()};
  for append in [true,false] {
   let request=Request::Place{version:1,temporary:vec![],state:state.clone(),history:history(),definitions:vec!["main".into()],
    schema_names:vec![],history_basis:"new".into(),append};
   let Plan::Place{placements,rebased,..}=plan(&request).unwrap() else {panic!()};
   assert_eq!(placements[0].after_input,0);assert!(rebased);
  }
 }
 /// Unload receipts replay exactly and stale activation cannot resurrect a selector.
 #[test]fn receipt_and_revision_safety() {
  let selected=json!({"type":"tool","tool_name":"file_read"});
  let state=State{revision:4,selectors:vec![selected.clone()],..Default::default()};
  let operation=Operation::Unload{selector:selected.clone(),expected_revision:4,request_id:"unload-1".into()};
  let request=Request::Transition{version:1,state,operation:operation.clone()};
  let Plan::Transition{state,removed,..}=plan(&request).unwrap() else {panic!()};assert_eq!(removed,Some(true));
  assert!(state.selectors.is_empty());assert_eq!(state.revision,5);
  let replay=Request::Transition{version:1,state:state.clone(),operation};
  assert!(matches!(plan(&replay).unwrap(),Plan::Transition{replayed:true,..}));
  let stale=Request::Transition{version:1,state,operation:Operation::Activate{selectors:vec![selected],expected_revision:4,request_id:"activate-2".into()}};
  assert!(plan(&stale).is_err());
 }
 /// Host validation rejects a fabricated identity, split call/result or changed request binding.
 #[test]fn forged_plans_fail() {
  let request=Request::Place{version:1,temporary:vec![],state:State::default(),history:history(),definitions:vec!["main".into()],
   schema_names:vec![],history_basis:"epoch".into(),append:true};
  let mut result=plan(&request).unwrap();
  if let Plan::Place{placements,..}=&mut result {placements[0].after_input=2;placements[0].anchor=Some("call:a".into());}
  assert!(contract::validate_plan(&request,&result).is_err());
  let request=Request::Transition{version:1,state:State::default(),operation:Operation::Activate{
   selectors:vec![json!({"type":"tool","tool_name":"file_read"})],expected_revision:0,request_id:"a".into()}};
  let mut result=plan(&request).unwrap();
  if let Plan::Transition{state,..}=&mut result {state.selectors.push(json!({"type":"tool","tool_name":"file_write"}));}
  assert!(contract::validate_plan(&request,&result).is_err());
 }
 /// Owned wire transport and linked calls must produce identical typed plans.
 #[test]fn linked_service_callback_equivalence() {
  let request=Request::Place{version:1,temporary:vec![],state:State::default(),history:history(),definitions:vec!["main".into()],
   schema_names:vec!["tool".into()],history_basis:"epoch".into(),append:true};
  let encoded=serde_json::to_vec(&request).unwrap();
  let response=service_callback(serde_json::from_slice(&encoded).unwrap()).unwrap();
  let service:Plan=serde_json::from_value(response["plan"].clone()).unwrap();
  assert_eq!(service,plan(&request).unwrap());contract::validate_plan(&request,&service).unwrap();
 }

 /// Call-scoped guidance always occupies the last completed boundary, including the first call.
 #[test]fn temporary_is_suffix_without_entering_permanent_state() {
  for append in [true,false] {
   let request=Request::Place{version:1,state:State::default(),history:history(),
    definitions:vec!["main".into()],temporary:vec!["native-call".into()],schema_names:vec![],
    history_basis:"epoch".into(),append};
   let result=plan(&request).unwrap();
   let Plan::Place{placements,..}=&result else {panic!()};
   assert_eq!(placements[0].after_input,0);assert_eq!(placements[1].after_input,3);
   let bodies=BTreeMap::from([("main".into(),"core".into()),("native-call".into(),"current mode".into())]);
   let projected=contract::project(&request,&result,&bodies,"developer").unwrap();
   assert_eq!(projected.input[4]["content"],"current mode");
   let mut forged=result.clone();
   if let Plan::Place{placements,..}=&mut forged {placements[1].after_input=0;placements[1].anchor=None;}
   assert!(contract::validate_plan(&request,&forged).is_err());
  }
 }
 /// A definition cannot impersonate both permanent and call-scoped guidance.
 #[test]fn temporary_ids_must_be_disjoint() {
  let request=Request::Place{version:1,state:State::default(),history:history(),
   definitions:vec!["same".into()],temporary:vec!["same".into()],schema_names:vec![],
   history_basis:"epoch".into(),append:true};
  assert!(plan(&request).is_err());
 }

 /// Changed or ambiguous anchors withdraw old offsets instead of inserting into a different exchange.
 #[test]fn missing_anchor_consolidates() {
  let request=Request::Place{version:1,temporary:vec![],state:State{history_basis:"epoch".into(),placements:vec![
   Placement{id:"main".into(),after_input:3,anchor:Some("call:gone".into())}],..Default::default()},
   history:history(),definitions:vec!["main".into()],schema_names:vec![],history_basis:"epoch".into(),append:true};
  let Plan::Place{placements,rebased,..}=plan(&request).unwrap() else {panic!()};
  assert_eq!(placements[0].after_input,0);assert!(rebased);
 }
}
