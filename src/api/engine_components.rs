//! Installation-administrator controls fence stale edits without granting module authority.
use super::*;
/// Read installed generations, per-component availability and content-free affected-work counts.
pub(crate) async fn read(State(app):State<App>)->Api {
 let catalog=crate::engine_components::catalog()?;
 let controls=crate::engine_components::controls(&app.store.root)?;
 let mut affected=0usize;let mut active_runs=0usize;let mut active_tasks=0usize;
 for user in crate::agent_loop::user_apps(&app).await? {
  for task in user.store.background_scan(None).await? {if matches!(task["status"].as_str(),Some("queued"|"running")){active_tasks+=1;}}
  for run in user.store.active_runs().await? {
   if matches!(run["status"].as_str(),Some("queued"|"running"|"waiting_approval"|"waiting_external")){active_runs+=1;}
   if run["status"]=="paused" {let id=run["id"].as_str().unwrap_or("");let full=user.run(id).await?;
    if matches!(full["component_pause"]["component"].as_str(),Some(crate::engine_components::CONTEXT|crate::engine_components::BACKGROUND)) {affected+=1;}
   }
  }
 }
 let mut entries=Vec::new();
 for descriptor in catalog.entries.values() {
  let (version,generation)=crate::engine_components::execution_identity(descriptor);
  entries.push(json!({"id":descriptor.id,"group":descriptor.group,"interface_version":descriptor.interface_version,
   "implementation_version":version,"generation":generation,
   "installed_implementation_version":descriptor.implementation_version,"installed_generation":descriptor.artifact_sha256,
   "mode":if cfg!(feature="live-context"){"live"}else{"linked"},"enabled":!controls.disabled.contains(&descriptor.id),
   "available":crate::engine_components::component_available(&app.store.root,&descriptor.id),"runtime_requirements":descriptor.runtime_requirements,
   "build_dependencies":descriptor.build.source_inputs}));
 }
 for id in &catalog.withdrawn {entries.push(json!({"id":id,"group":"withdrawn","mode":if cfg!(feature="live-context"){"live"}else{"linked"},"available":false,"enabled":!controls.disabled.contains(id),"installed":false,"withdrawn":true}));}
 if entries.is_empty(){entries.push(json!({"id":crate::engine_components::CONTEXT,"group":"engine-loop","available":false,"enabled":!controls.disabled.iter().any(|id|id==crate::engine_components::CONTEXT),"installed":false}));}
 let adoption=|role:&str|std::fs::read(app.store.root.join("publication").join(format!("engine-{role}.json"))).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
 Ok(Json(json!({"namespace":"engine","revision":controls.revision,"catalog_revision":catalog.revision,"components":entries,
 "affected_work":affected,"active_work":{"runs":active_runs,"tasks":active_tasks},"adoption":{"api":adoption("api"),"loop":adoption("loop")}})))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Update {pub expected_revision:u64,pub expected_catalog_revision:String,pub enabled:bool}
/// The owning API serializes controls; packages remain exclusively broker-managed.
pub(crate) async fn update(State(app):State<App>,Path(id):Path<String>,Json(body):Json<Update>)->Api {
 let installed=crate::engine_components::catalog()?;
 if !installed.entries.contains_key(&id)&&!installed.withdrawn.contains(&id)&&id!=crate::engine_components::CONTEXT{return Err(bad("Unknown engine component"));}
 let _guard=app.gate.lock().await;
 let catalog=crate::engine_components::catalog()?;
 let mut current=crate::engine_components::controls(&app.store.root)?;
 if current.revision!=body.expected_revision||catalog.revision!=body.expected_catalog_revision {
  return Err(conflict("Engine components changed; reload before saving"));
 }
 if body.enabled&&!catalog.entries.contains_key(&id){return Err(bad("Install a compatible component through the build broker first"));}
 current.revision=current.revision.checked_add(1).ok_or_else(||bad("Component revision exhausted"))?;
 current.disabled.retain(|v|v!=&id);if !body.enabled{current.disabled.push(id);}
 crate::engine_components::save_controls(&app.store.root,&current)?;
 drop(_guard);read(State(app)).await
}

#[cfg(test)]mod tests {
 use super::*;
 #[tokio::test]
 async fn disable_fences_stale_saves_and_keeps_removed_package_controls(){
  let (_root,app)=crate::runtime_tests::fixture().await;let before=read(State(app.clone())).await.unwrap().0;
  let body=Update{expected_revision:before["revision"].as_u64().unwrap(),expected_catalog_revision:before["catalog_revision"].as_str().unwrap().into(),enabled:false};
  let after=update(State(app.clone()),Path(crate::engine_components::CONTEXT.into()),Json(body)).await.unwrap().0;
  assert_eq!(after["components"].as_array().unwrap().iter().find(|item|item["id"]==crate::engine_components::CONTEXT).unwrap()["available"],false);assert_eq!(after["revision"],1);
  let stale=Update{expected_revision:0,expected_catalog_revision:before["catalog_revision"].as_str().unwrap().into(),enabled:true};
  assert!(update(State(app.clone()),Path(crate::engine_components::CONTEXT.into()),Json(stale)).await.is_err());
  let enabled=Update{expected_revision:1,expected_catalog_revision:before["catalog_revision"].as_str().unwrap().into(),enabled:true};
  let restored=update(State(app.clone()),Path(crate::engine_components::CONTEXT.into()),Json(enabled)).await.unwrap().0;assert_eq!(restored["components"].as_array().unwrap().iter().find(|item|item["id"]==crate::engine_components::CONTEXT).unwrap()["available"],true);
 }
}
