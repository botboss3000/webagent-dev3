//! Host materialization of validated component plans; authority and durable history remain native.
// @code-index {"version":1,"feature":"context-permanent-assets","purpose":"Materialize live guidance and permanent conversation selectors through bounded component plans; native host retains authorization and canonical history. Config: plugins/engine/context-assets/component.json -> limits; config/engine-components.json -> disabled, revision; config/prompts.json -> base_instructions, closing_reporting, jev_guidance.","span_lines":40,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled","/revision"]},{"path":"plugins/engine/context-assets/component.json","pointers":["/limits"]},{"path":"config/prompts.json","pointers":["/base_instructions","/closing_reporting","/jev_guidance"]}],"opportunities":[]},"aliases":[]}
use crate::{App,ability_controls,session_mode,context_input,context_assembly,provider};
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

/// Reload current published files; a changed body cannot hide behind an old cached manifest.
pub(crate) fn catalog(app:&App)->Result<crate::abilities::Catalog> {
 if let Some(root)=crate::live_assets::published_root() {
  crate::abilities::Catalog::load_snapshot(&root.join("plugins/abilities"))
 } else {crate::abilities::Catalog::load_current(app.abilities.snapshot_root())}
}

/// Fence synchronous file-backed sources around body assembly and the final provider poll boundary.
/// A catalog loaded before a publication cannot authorize bodies under a newer source fingerprint.
pub(crate) fn source_fence(app:&App)->Result<String> {
 let fresh=catalog(app)?;
 if fresh.version()!=app.abilities.version()||fresh.live_version()!=app.abilities.live_version() {
  bail!("Context assets source changed");
 }
 let base=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,&app.store.root,fresh.source_root())?;
 let engines=crate::engine_components::catalog()?;
 let controls=crate::engine_components::controls(&app.store.root)?;
 Ok(crate::digest(&json!([app.abilities.version(),app.abilities.live_version(),
  fresh.version(),fresh.live_version(),base,engines.revision,controls])))
}

/// Normalize only identity fields from trusted native producers; old hashes never pin guidance.
fn skill_selector(value:&Value)->Result<Value> {
 let selector=json!({"type":"skill","ability_id":value["ability_id"],"kind":value["kind"],"skill_id":value["skill_id"]});
 webagent_assets_contracts::validate_selector(&selector)?;Ok(selector)
}

/// Resolve immutable run identity without accepting model-selected scope arguments.
fn identity(app:&App,run:&Value)->Result<(String,String,String)> {
 let sid=run["session_id"].as_str().ok_or_else(||anyhow::anyhow!("Context assets session missing"))?;
 let agent=run["agent_id"].as_str().unwrap_or("shared_default");
 let requester=run["requester_user_id"].as_str().unwrap_or(app.store.current_user_id());
 if [sid,agent,requester].iter().any(|id|!webagent_assets_contracts::valid_id(id)){bail!("Context assets identity");}
 Ok((sid.into(),agent.into(),requester.into()))
}

/// Import only canonical encrypted native selection projections once, never automatic dependencies.
async fn legacy_seed(app:&App,run:&mut Value,state:&State)->Result<bool> {
 if state.revision!=0||!run["context_assets_revision"].is_null()||run["status"]!="running" {return Ok(false);}
 let tools=run["selected_tools"].as_array().cloned().unwrap_or_default();
 let skills=run["loaded_skills"].as_array().cloned().unwrap_or_default();
 if tools.is_empty()&&skills.is_empty(){return Ok(false);}
 let (sid,agent,requester)=identity(app,run)?;
 let rid=run["id"].as_str().ok_or_else(||anyhow::anyhow!("Context assets run missing"))?.to_owned();
 let canonical=app.run(&rid).await?;
 let canonical_requester=canonical["requester_user_id"].as_str().unwrap_or(app.store.current_user_id());
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
 run["_context_assets_operation"]=json!(operation);
 app.save(run).await?;Ok(true)
}

/// Hydrate permanent identities before policy/catalog assembly and persist changed native projections.
pub(crate) async fn hydrate(app:&App,run:&mut Value)->Result<State> {
 let (sid,agent,requester)=identity(app,run)?;
 let scoped=app.store.for_agent(&agent).await?;
 let mut state=scoped.context_assets(&sid,&agent,&requester).await?;
 if legacy_seed(app,run,&state).await? {state=scoped.context_assets(&sid,&agent,&requester).await?;}
 let fresh=catalog(app)?;
 let (_,_,visibility,session_state)=ability_controls(app,&sid).await?;
 let mut loaded=Vec::new();let mut tools=Vec::new();let mut skills=Vec::new();
 for selector in &state.selectors {
  match selector["type"].as_str() {
   Some("ability")=>{let value=selector["ability_id"].clone();if !loaded.contains(&value){loaded.push(value);}},
   Some("tool")=>{
    let Some(name)=selector["tool_name"].as_str() else {continue;};
    tools.push(json!(name));let marker=json!(crate::tools::core_tool_search::marker(name));
    if !loaded.contains(&marker){loaded.push(marker);}
    for owner in fresh.tool_abilities(name,&visibility,&session_state,&json!(loaded)) {
     let value=json!(owner);if !loaded.contains(&value){loaded.push(value);}
    }
   },
   Some("skill")=>skills.push(json!({"ability_id":selector["ability_id"],"kind":selector["kind"],"skill_id":selector["skill_id"]})),
   _=>bail!("Invalid context asset selector"),
  }
 }
 let before=crate::digest(&json!([run["loaded_abilities"],run["selected_tools"],run["loaded_skills"],run["context_assets_revision"]]));
 run["loaded_abilities"]=json!(loaded);run["selected_tools"]=json!(tools);run["loaded_skills"]=json!(skills);
 run["context_assets_revision"]=json!(state.revision);
 let after=crate::digest(&json!([run["loaded_abilities"],run["selected_tools"],run["loaded_skills"],run["context_assets_revision"]]));
 if before!=after {app.save(run).await?;}
 Ok(state)
}

/// Bind every current source/authority input, excluding self-mutating placement revisions.
pub(crate) async fn fingerprint(app:&App,sid:&str,agent_id:&str,loaded:&Value,selected:&Value,
 skills:&Value,optimization:&Value,requester:&str)->Result<String> {
 let fresh=catalog(app)?;
 let access=crate::shared_chat::live_access(&app.store,requester,sid,agent_id).await?;
 let (policy,mode_revision,visibility,state)=ability_controls(app,sid).await?;
 let mode=session_mode(app,sid).await?;
 let scoped=app.store.for_agent(agent_id).await?;
 let ledger=scoped.context_assets(sid,agent_id,requester).await?;
 let own=scoped.for_user(requester).await?.list_own_skills().await?;
 let base=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,&app.store.root,fresh.source_root())?;
 let slots=scoped.list_agent_prompt_slots(agent_id).await?;
 let engines=crate::engine_components::catalog()?;
 let controls=crate::engine_components::controls(&app.store.root)?;
 Ok(crate::digest(&json!([fresh.version(),fresh.live_version(),policy,mode_revision,visibility,state,mode,
  loaded,selected,skills,optimization,access.context_fingerprint(),ledger.selectors,own["revision"],own,
  base,slots,engines.revision,controls])))
}

/// Resolve current host-owned prompt sources and submit only authorized bodies to the composer.
pub(crate) async fn prepare_capability(app:&App,run:&Value,schemas:&[Value],guidance:Vec<Guidance>,
 menu:String,sourcefingerprint:&str,cancel:&CancellationToken)->Result<PreparedCapability> {
 let (sid,agent,requester)=identity(app,run)?;let scoped=app.store.for_agent(&agent).await?;
 let fresh=catalog(app)?;
 let base_prompt=provider::Provider::instructions_for_snapshot(provider::DEFAULT_INSTRUCTIONS,&app.store.root,fresh.source_root())?;
 let slots=scoped.list_agent_prompt_slots(&agent).await?;
 let live_slots=context_assembly::pinned_instructions("",&slots)?;
 let ledger=scoped.context_assets(&sid,&agent,&requester).await?;
 let request=PrepareRequest{version:1,source_fingerprint:sourcefingerprint.into(),base_prompt,live_slots,guidance,
  permitted_schemas:schemas.iter().map(|schema|schema["name"].as_str().map(str::to_owned).ok_or_else(||anyhow::anyhow!("Schema identity"))).collect::<Result<_>>()?,
  prior_schema_order:ledger.schema_order,menu};
 crate::engine_components::capability_prepare(&app.store.root,&request,cancel).await
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

/// Place permanent references and temporary native additions without ever changing durable history.
pub(crate) async fn project(app:&App,run:&mut Value,cap:&PreparedCapability,profile:&ProfileV1,
 schemas:&[Value],history:&[Value],contributions:&[context_input::Contribution],
 call:context_input::CallIdentity,cancel:&CancellationToken)->Result<Working> {
 let (sid,agent,requester)=identity(app,run)?;let scoped=app.store.for_agent(&agent).await?;
 let ledger=scoped.context_assets(&sid,&agent,&requester).await?;
 let session=scoped.get(&format!("session:{sid}")).await?.ok_or_else(||anyhow::anyhow!("Context session missing"))?;
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
 let plan=crate::engine_components::assets_plan(&app.store.root,&request,cancel).await?;
 webagent_assets_contracts::validate_plan(&request,&plan)?;
 let Plan::Place{placements,schema_order,..}=&plan else {bail!("Context placement kind");};
 let mut next=ledger.clone();
 next.placements=placements.iter().filter(|placement|definitions.contains(&placement.id)).cloned().collect();
 next.schema_order=schema_order.clone();next.history_basis=history_basis;
 if next!=ledger {
  next.revision=ledger.revision.checked_add(1).ok_or_else(||anyhow::anyhow!("Context assets revision exhausted"))?;
  run["_context_assets_write"]=json!({"scope":crate::store::context_assets::scope_key(&agent,&requester),
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

/// Decode native call arguments while retaining their exact expected revisions and request IDs.
fn arguments(run:&Value)->Result<Value> {
 let value=&run["pending"]["arguments"];
 if let Some(raw)=value.as_str(){Ok(serde_json::from_str(raw)?)}else{Ok(value.clone())}
}

/// Stage only validated explicit selection effects before append_tool clears pending identity.
pub(crate) fn record_tool_operation(run:&mut Value,result:&Value,previous_tools:&Value,previous_skills:&Value)->Result<()> {
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

#[cfg(test)]
mod tests {
 use super::*;
 /// Native output and changed current projections must agree before any activation is staged.
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
 /// Legacy identity conversion discards source pins and cannot import automatic ability IDs.
 #[test]fn normalized_skill_is_identity_only() {
  let value=skill_selector(&json!({"ability_id":"base","kind":"own","skill_id":"guide","sha256":"old","revision":8})).unwrap();
  assert_eq!(value,json!({"type":"skill","ability_id":"base","kind":"own","skill_id":"guide"}));
 }

 /// Initialize a canonical queued run without a provider or worker; all state uses the real Store.
 #[cfg(feature="linked-context")]
 async fn bridge_fixture()->(tempfile::TempDir,App,Value) {
  let (root,app)=crate::runtime_tests::fixture_uninitialized().await;
  app.store.put("session:assets-bridge",&json!({"id":"assets-bridge","agent_id":"shared_default"})).await.unwrap();
  let mut run=json!({"id":"assets-run","session_id":"assets-bridge","agent_id":"shared_default",
   "status":"queued","run_revision":0,"lease_epoch":0,"generation":"fixture",
   "requester_user_id":app.store.current_user_id(),"input":[{"role":"user","content":"durable user input"}]});
  app.save(&mut run).await.unwrap();(root,app,run)
 }
 /// Resolve a real capability through the host bridge with a selected, untrusted file body.
 #[cfg(feature="linked-context")]
 async fn bridge_capability(app:&App,run:&Value)->PreparedCapability {
  prepare_capability(app,run,&[json!({"type":"function","name":"file_read","parameters":{"type":"object"}})],
   vec![Guidance{id:"fixture-skill".into(),kind:webagent_capability_contracts::GuidanceKind::Selectable,
    body:"file guidance sentinel".into(),selected:true,natural:false}],
   "temporary menu sentinel".into(),&"a".repeat(64),&CancellationToken::new()).await.unwrap()
 }
 /// Saved main prompt changes are resolved on each preparation, including the synchronous dispatch fence.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn preparation_rereads_current_main_prompt() {
  let (root,app,run)=bridge_fixture().await;
  std::fs::create_dir_all(root.path().join("config")).unwrap();
  let path=root.path().join("config/prompts.json");
  std::fs::write(&path,serde_json::to_vec(&json!({"base_instructions":"first prompt sentinel"})).unwrap()).unwrap();
  let first_fence=source_fence(&app).unwrap();
  let first=Box::pin(bridge_capability(&app,&run)).await;
  assert!(first.main.contains("first prompt sentinel"));
  std::fs::write(&path,serde_json::to_vec(&json!({"base_instructions":"second prompt sentinel"})).unwrap()).unwrap();
  let second=Box::pin(bridge_capability(&app,&run)).await;
  assert!(second.main.contains("second prompt sentinel"));
  assert!(!second.main.contains("first prompt sentinel"));
  assert_ne!(first_fence,source_fence(&app).unwrap());
 }
 /// Exercise activation, projection, compaction and unload through App/Store without persisting synthetic text.
 #[cfg(feature="linked-context")]
 #[tokio::test]
 async fn projected_guidance_stays_out_of_durable_history() {
  let (_root,app,mut run)=bridge_fixture().await;
  let selected=json!({"type":"tool","tool_name":"file_read"});
  run["context_assets_revision"]=json!(0);
  run["pending"]=json!({"name":"load_tool","call_id":"load-fixture","arguments":{"tool_name":"file_read"}});
  run["selected_tools"]=json!(["file_read"]);
  record_tool_operation(&mut run,&json!({"status":"success","output":{"activated":true,"selected_tools":["file_read"]}}),&json!([]),&json!([])).unwrap();
  run.as_object_mut().unwrap().remove("pending");
  app.save(&mut run).await.unwrap();
  let state=hydrate(&app,&mut run).await.unwrap();assert_eq!(state.selectors,vec![selected.clone()]);
  let cap=Box::pin(bridge_capability(&app,&run)).await;
  let profile:ProfileV1=serde_json::from_value(json!({"append_instructions":true,"instruction_role":"developer",
   "transport":"responses","reasoning_replay":"responses_items","cache_control":"implicit_prefix"})).unwrap();
  let original=run["input"].as_array().unwrap().clone();
  let schemas=vec![json!({"type":"function","name":"file_read","parameters":{"type":"object"}})];
  let working=Box::pin(project(&app,&mut run,&cap,&profile,&schemas,&original,&[],context_input::CallIdentity{turn:1,call:1},&CancellationToken::new())).await.unwrap();
  assert_eq!(working.input.len(),3);assert_eq!(working.input[1],original[0]);
  assert!(working.input[0]["content"].as_str().unwrap().contains("file guidance sentinel"));
  assert!(working.input[2]["content"].as_str().unwrap().contains("temporary menu sentinel"));
  assert_eq!(working.extra_json_bytes,serde_json::to_vec(&working.input).unwrap().len()-serde_json::to_vec(&original).unwrap().len());
  app.save(&mut run).await.unwrap();
  assert_eq!(run["input"],json!(original));
  assert_eq!(app.run("assets-run").await.unwrap()["input"],json!(original));
  let ledger=app.store.context_assets("assets-bridge","shared_default",app.store.current_user_id()).await.unwrap();
  assert_eq!(ledger.placements.len(),1);assert_eq!(ledger.placements[0].id,"fixture-skill");
  run["input"]=json!([{"role":"assistant","content":"canonical summary"}]);run["compactions"]=json!(1);
  app.store.save_run(&mut run,"context_compacted",&json!({"source":original,"source_digest":crate::digest(&original),"dropped_items":1,"segment_id":"assets-run-1"})).await.unwrap();
  let rebased=hydrate(&app,&mut run).await.unwrap();
  assert_eq!(rebased.selectors,vec![selected.clone()]);assert!(rebased.placements.is_empty());
  let operation=Operation::Unload{selector:selected.clone(),expected_revision:rebased.revision,request_id:"fixture-unload".into()};
  run["pending"]=json!({"name":"context_assets","call_id":"unload-fixture","arguments":{"action":"unload","selector":selected,"expected_revision":rebased.revision,"request_id":"fixture-unload"}});
  let tools=run["selected_tools"].clone();let skills=run["loaded_skills"].clone();
  record_tool_operation(&mut run,&json!({"status":"success","output":{"operation":operation}}),&tools,&skills).unwrap();
  run.as_object_mut().unwrap().remove("pending");app.save(&mut run).await.unwrap();
  let unloaded=hydrate(&app,&mut run).await.unwrap();assert!(unloaded.selectors.is_empty());
  assert_eq!(run["selected_tools"],json!([]));
  let saved=app.run("assets-run").await.unwrap();
  assert_eq!(saved["input"],json!([{"role":"assistant","content":"canonical summary"}]));
  assert!(saved.get("_context_assets_write").is_none()&&saved.get("_context_assets_operation").is_none());
 }
}
