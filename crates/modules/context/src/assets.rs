//! Materialization of validated plans; execution authority and durable history remain with the caller.
use crate::input as context_input;
// @code-index {"version":1,"feature":"context-permanent-assets","purpose":"Materialize live guidance and permanent conversation selectors through bounded component plans; native host retains authorization and canonical history. Config: plugins/engine/context-assets/component.json -> limits; config/engine-components.json -> disabled, revision initializes data-root controls; crates/modules/configuration/prompts -> base_instructions, closing_reporting, jev_guidance supplies published defaults merged with data-root overrides and saved default provenance.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled","/revision"]},{"path":"plugins/engine/context-assets/component.json","pointers":["/limits"]},{"path":"crates/modules/configuration/prompts","pointers":["/base_instructions","/closing_reporting","/jev_guidance"]}],"opportunities":[]},"aliases":[]}
use anyhow::{bail,Result};
use serde_json::{json,Value};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use webagent_assets_contracts::{State,Operation,Request,Plan};
use webagent_capability_contracts::{Guidance,PreparedCapability,PrepareRequest};
use webagent_provider_contracts::ProfileV1;

pub struct Working {
 pub instructions:String,pub guidance:String,pub input:Vec<Value>,pub schemas:Vec<Value>,
 pub extra_json_bytes:usize,pub contributions_projected:bool,
}





/// Normalize only identity fields from trusted native producers; old hashes never pin guidance.
fn skill_selector(value:&Value)->Result<Value> {
 let selector=json!({"type":"skill","ability_id":value["ability_id"],"kind":value["kind"],"skill_id":value["skill_id"]});
 webagent_assets_contracts::validate_selector(&selector)?;Ok(selector)
}











/// Preserve typed host settings classification separately from untrusted file guidance.
fn definition_body(definition:&webagent_capability_contracts::Definition)->String {
 let label=if definition.kind==webagent_capability_contracts::GuidanceKind::Ability
  &&definition.id.starts_with("ability:")&&definition.id.ends_with(":settings") {
  "Current host ability settings"
 } else {"Untrusted live guidance"};
 format!("\n\n## {label}: {}\n{}",definition.id,definition.body)
}

/// Map only authorized schema identities back to the exact current host objects.
fn schemas_in_order(schemas:&[Value],order:&[String])->Result<Vec<Value>> {
 order.iter().map(|name|schemas.iter().find(|schema|schema["name"]==name.as_str()).cloned()
  .ok_or_else(||anyhow::anyhow!("Context schema unavailable"))).collect()
}



/// Decode native call arguments while retaining their exact expected revisions and request IDs.
fn arguments(run:&Value)->Result<Value> {
 let value=&run["pending"]["arguments"];
 if let Some(raw)=value.as_str(){Ok(serde_json::from_str(raw)?)}else{Ok(value.clone())}
}

/// Stage only validated explicit selection effects before append_tool clears pending identity.
pub fn record_tool_operation(run:&mut Value,result:&Value,previous_tools:&Value,previous_skills:&Value)->Result<()> {
 let Some(name)=run["pending"]["name"].as_str() else {return Ok(());};
 let name=name.to_owned();
 if !matches!(name.as_str(),"context_assets"|"load_tool"|"search_tools"|"load_skill"|"load_ability")
  ||!matches!(result["status"].as_str(),Some("success"|"ok")) {return Ok(());}
 let args=arguments(run)?;
 if name=="context_assets" {
  if result["status"]!="success"||args["action"]!="unload" {return Ok(());}
  let operation=Operation::Unload{selector:args["selector"].clone(),
   expected_revision:args["expected_revision"].as_u64().ok_or_else(||anyhow::anyhow!("Unload revision"))?,
   request_id:args["request_id"].as_str().ok_or_else(||anyhow::anyhow!("Unload request ID"))?.into()};
  if result["output"]["operation"]!=json!(operation){bail!("Unload operation result mismatch");}
  webagent_assets_contracts::validate_selector(&args["selector"])?;
  run["_context_assets_operation"]=json!(operation);return Ok(());
 }
 let mut selectors=Vec::new();
 match name.as_str() {
  "load_tool"|"search_tools" if result["status"]=="success"&&result["output"]["activated"]==true=>{
   for tool in result["output"]["selected_tools"].as_array().into_iter().flatten() {
    if !run["selected_tools"].as_array().is_some_and(|items|items.contains(tool))
     ||previous_tools.as_array().is_some_and(|items|items.contains(tool)){continue;}
    let value=json!({"type":"tool","tool_name":tool});webagent_assets_contracts::validate_selector(&value)?;
    if !selectors.contains(&value){selectors.push(value);}
   }
  },
  "load_skill" if result["status"]=="success"&&result["output"]["status"]=="loaded"=>{
   let expected=skill_selector(&result["output"])?;
   if run["loaded_skills"].as_array().into_iter().flatten().any(|skill|skill_selector(skill).ok().as_ref()==Some(&expected))
    &&!previous_skills.as_array().into_iter().flatten().any(|skill|skill_selector(skill).ok().as_ref()==Some(&expected)) {selectors.push(expected);}
  },
  "load_ability" if result["status"]=="ok"&&result["ability_id"]==args["ability_id"]=>{
   let id=args["ability_id"].clone();
   if run["loaded_abilities"].as_array().is_some_and(|items|items.contains(&id)) {
    let value=json!({"type":"ability","ability_id":id});webagent_assets_contracts::validate_selector(&value)?;selectors.push(value);
   }
  },
  _=>{},
 }
 if selectors.is_empty(){return Ok(());}
 let revision=run["context_assets_revision"].as_u64().unwrap_or(0);
 let request_id=format!("activation:{}",crate::digest(&json!([run["id"],run["pending"]["call_id"],name])));
 run["_context_assets_operation"]=json!(Operation::Activate{selectors,expected_revision:revision,request_id});
 Ok(())
}


pub fn identity(run:&Value,current_user_id:&str)->Result<(String,String,String)> {
 let sid=run["session_id"].as_str().ok_or_else(||anyhow::anyhow!("Context assets session missing"))?;
 let agent=run["agent_id"].as_str().unwrap_or("shared_default");
 let requester=run["requester_user_id"].as_str().unwrap_or(current_user_id);
 if [sid,agent,requester].iter().any(|id|!webagent_assets_contracts::valid_id(id)){bail!("Context assets identity");}
 Ok((sid.into(),agent.into(),requester.into()))
}
/// Hydrate exact permanent selectors using the current owner-supplied catalog operation.
pub fn hydrate_selectors(run:&mut Value,state:&State,tool_marker:impl Fn(&str)->String,tool_abilities:impl Fn(&str,&Value)->Vec<String>)->Result<bool> {
 let mut loaded=Vec::new();let mut tools=Vec::new();let mut skills=Vec::new();
 for selector in &state.selectors {
  match selector["type"].as_str() {
   Some("ability")=>{let value=selector["ability_id"].clone();if !loaded.contains(&value){loaded.push(value);}},
   Some("tool")=>{
    let Some(name)=selector["tool_name"].as_str() else {continue;};
    tools.push(json!(name));let marker=json!(tool_marker(name));
    if !loaded.contains(&marker){loaded.push(marker);}
    for owner in tool_abilities(name,&json!(loaded)) {let value=json!(owner);if !loaded.contains(&value){loaded.push(value);}}
   },
   Some("skill")=>skills.push(json!({"ability_id":selector["ability_id"],"kind":selector["kind"],"skill_id":selector["skill_id"]})),
   _=>bail!("Invalid context asset selector"),
  }
 }
 let before=crate::digest(&json!([run["loaded_abilities"],run["selected_tools"],run["loaded_skills"],run["context_assets_revision"]]));
 run["loaded_abilities"]=json!(loaded);run["selected_tools"]=json!(tools);run["loaded_skills"]=json!(skills);
 run["context_assets_revision"]=json!(state.revision);
 Ok(before!=crate::digest(&json!([run["loaded_abilities"],run["selected_tools"],run["loaded_skills"],run["context_assets_revision"]])))
}
pub fn legacy_seed_needed(run:&Value,state:&State)->bool {
 state.revision==0&&run["context_assets_revision"].is_null()&&run["status"]=="running"&&
 (!run["selected_tools"].as_array().is_none_or(|v|v.is_empty())||!run["loaded_skills"].as_array().is_none_or(|v|v.is_empty()))
}
pub fn legacy_operation(run:&Value,canonical:&Value,current_user_id:&str)->Result<Operation> {
 let tools=run["selected_tools"].as_array().cloned().unwrap_or_default();
 let skills=run["loaded_skills"].as_array().cloned().unwrap_or_default();
 let (sid,agent,requester)=identity(run,current_user_id)?;
 let rid=run["id"].as_str().ok_or_else(||anyhow::anyhow!("Context assets run missing"))?.to_owned();
 let canonical_requester=canonical["requester_user_id"].as_str().unwrap_or(current_user_id);
 if canonical["status"]!="running"||canonical["session_id"]!=sid
  ||canonical["agent_id"].as_str().unwrap_or("shared_default")!=agent||canonical_requester!=requester
  ||canonical["run_revision"]!=run["run_revision"]||canonical["selected_tools"]!=run["selected_tools"]
  ||canonical["loaded_skills"]!=run["loaded_skills"] {bail!("Context assets legacy checkpoint changed");}
 let mut selectors=Vec::new();
 for tool in tools {
  let value=json!({"type":"tool","tool_name":tool});webagent_assets_contracts::validate_selector(&value)?;
  if !selectors.contains(&value){selectors.push(value);}
 }
 for skill in skills {let value=skill_selector(&skill)?;if !selectors.contains(&value){selectors.push(value);}}
 let operation=Operation::Activate{selectors,expected_revision:0,
  request_id:format!("legacy:{}",crate::digest(&json!([sid,agent,requester,rid])))};
 Ok(operation)
}
pub async fn project_context(run:&mut Value,cap:&PreparedCapability,profile:&ProfileV1,
 schemas:&[Value],history:&[Value],contributions:&[context_input::Contribution],call:context_input::CallIdentity,
 ledger:State,session:&Value,agent:&str,requester:&str,root:&std::path::Path,planner:&impl crate::ports::Planner,cancel:&CancellationToken)->Result<Working> {
 let history_basis=crate::digest(&json!([session["history_revision"],session["checkpoint_through"]]));
 let mut bodies=BTreeMap::new();let mut definitions=Vec::new();
 for definition in &cap.definitions {
  if bodies.insert(definition.id.clone(),definition_body(definition)).is_some(){bail!("Duplicate capability definition");}
  definitions.push(definition.id.clone());
 }
 let mut native=cap.temporary_menu.clone();
 for selected in context_input::select(contributions,call)? {
  if selected.disposition=="included" {native.push('\n');native.push_str(&selected.item.content);}
 }
 let temporary=if profile.append_instructions&&!native.is_empty() {
  if bodies.insert("native-call".into(),native.clone()).is_some(){bail!("Reserved native definition ID");}
  vec!["native-call".into()]
 } else {Vec::new()};
 let request=Request::Place{version:1,state:ledger.clone(),history:history.to_vec(),definitions:definitions.clone(),
  temporary,schema_names:cap.schema_order.clone(),history_basis:history_basis.clone(),append:profile.append_instructions};
 let plan=planner.assets_plan(root,&request,cancel).await?;
 webagent_assets_contracts::validate_plan(&request,&plan)?;
 let Plan::Place{placements,schema_order,..}=&plan else {bail!("Context placement kind");};
 let mut next=ledger.clone();
 next.placements=placements.iter().filter(|placement|definitions.contains(&placement.id)).cloned().collect();
 next.schema_order=schema_order.clone();next.history_basis=history_basis;
 if next!=ledger {
  next.revision=ledger.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Context assets revision exhausted"))?;
  run["_context_assets_write"]=json!({"scope":crate::ledger::scope_key(&agent,&requester),
   "expected_revision":ledger.revision,"state":next});
 }
 let schemas=schemas_in_order(schemas,schema_order)?;
 if profile.append_instructions {
  let projected=webagent_assets_contracts::project(&request,&plan,&bodies,profile.instruction_role.as_str())?;
  Ok(Working{instructions:cap.main.clone(),guidance:String::new(),input:projected.input,schemas,
   extra_json_bytes:projected.extra_json_bytes,contributions_projected:true})
 } else {
  let mut guidance=definitions.iter().map(|id|bodies[id].as_str()).collect::<Vec<_>>().join("");
  guidance.push_str(&cap.temporary_menu);
  Ok(Working{instructions:cap.main.clone(),guidance,input:history.to_vec(),schemas,extra_json_bytes:0,contributions_projected:false})
 }
}
#[cfg(test)] mod tests {use super::*;
 #[test]fn explicit_tool_delta_only() {
  let mut run=json!({"id":"r","context_assets_revision":4,"pending":{"name":"load_tool","call_id":"c","arguments":{"tool_name":"file_read"}},
   "selected_tools":["file_read"],"loaded_abilities":["automatic"]});
  record_tool_operation(&mut run,&json!({"status":"success","output":{"activated":true,"selected_tools":["file_read"]}}),&json!([]),&json!([])).unwrap();
  let operation:Operation=serde_json::from_value(run["_context_assets_operation"].clone()).unwrap();
  let Operation::Activate{selectors,expected_revision,..}=operation else {panic!()};
  assert_eq!(expected_revision,4);assert_eq!(selectors,vec![json!({"type":"tool","tool_name":"file_read"})]);
  let mut denied=run.clone();denied.as_object_mut().unwrap().remove("_context_assets_operation");
  record_tool_operation(&mut denied,&json!({"status":"denied"}),&json!([]),&json!([])).unwrap();
  assert!(denied.get("_context_assets_operation").is_none());
 }
 #[test]fn normalized_skill_is_identity_only() {
  let value=skill_selector(&json!({"ability_id":"base","kind":"own","skill_id":"guide","sha256":"old","revision":8})).unwrap();
  assert_eq!(value,json!({"type":"skill","ability_id":"base","kind":"own","skill_id":"guide"}));
 }
}

pub fn capability_request(base_prompt:String,live_slots:String,ledger:State,schemas:&[Value],guidance:Vec<Guidance>,menu:String,sourcefingerprint:&str)->Result<PrepareRequest> {
 Ok(PrepareRequest{version:1,source_fingerprint:sourcefingerprint.into(),base_prompt,live_slots,guidance,
  permitted_schemas:schemas.iter().map(|schema|schema["name"].as_str().map(str::to_owned).ok_or_else(||anyhow::anyhow!("Schema identity"))).collect::<Result<_>>()?,
  prior_schema_order:ledger.schema_order,menu})
}
