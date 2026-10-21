//! Owned public fixtures exercise library IO and cache algorithms, not foreign authority.
use super::*;
use anyhow::{Result,anyhow};
use serde_json::{json,Value};
use std::{path::{Path,PathBuf},sync::Arc,cell::RefCell};
use sha2::{Digest,Sha256};
thread_local! {static SOURCE:RefCell<Option<PathBuf>>=const {RefCell::new(None)};static OPTIONAL:RefCell<Option<PathBuf>>=const {RefCell::new(None)};}
#[derive(Clone)]struct Fixture;
impl Host for Fixture {
 fn selected_source()->Result<Option<PathBuf>> {Ok(SOURCE.with(|v|v.borrow().clone()))}
 fn native_schema_snapshot_at(_:Option<&Path>)->Result<Vec<Value>> {Ok(vec![])}
 fn native_schemas_for_catalog(_: &Path,_:bool)->Result<Vec<Value>> {Ok(vec![])}
 fn native_schemas_without_administrator()->Vec<Value> {vec![]}
 fn native_schemas()->Vec<Value> {vec![json!({"name":"reserved_native"})]}
 fn administrator_supported(_: &str)->bool {false}
 fn module_known(_: &str)->bool {false}
 fn module_available(_: &str)->bool {false}
 fn optimization_folder()->PathBuf {OPTIONAL.with(|v|v.borrow().clone()).unwrap_or_default()}
 fn optimization_declaration()->Result<Option<(String,Value)>> {
  let Some(path)=OPTIONAL.with(|v|v.borrow().clone()) else{return Ok(None)};
  let bytes=std::fs::read(path.join("optimization.json"))?;
  let value:Value=serde_json::from_slice(&bytes).map_err(|_|anyhow!("Invalid fixture optional declaration"))?;
  Ok(Some((hex::encode(Sha256::digest(&bytes)),value)))
 }
 fn optimization_available()->bool {true}
 fn optimization_generation()->Option<String> {Self::optimization_declaration().ok().flatten().map(|(g,_)|g)}
 fn optimization_guidance()->Option<String> {Some("Owned fixture guidance".into())}
 fn mcp_folder()->PathBuf {PathBuf::new()}
 fn mcp_available()->bool {false}
 fn mcp_schemas()->Vec<Value> {vec![]}
 fn mcp_guidance()->&'static str {"Fixture MCP"}
}
fn package(root:&Path)->PathBuf {
 let path=root.join("Custom/fixture_echo");std::fs::create_dir_all(path.join("skills")).unwrap();
 for (name,body) in [
  ("fixture_echo.json",json!({"display_name":"Fixture echo","kind":"ability","description":"Owned fixture","readiness":"ready","tools":["fixture_echo"]})),
  ("plugin.json",json!({"id":"fixture_echo","dispatch":"isolated","protocol":1,"interface":1,"runtime":"python3","entrypoint":"runner.py","schemas":"schemas.json","effect":"write","tools":["fixture_echo"],"build":{"inputs":["runner.py","helper.py"],"assets":["fixture_echo.skill.md"]}})),
  ("schemas.json",json!([{"type":"function","name":"fixture_echo","description":"Echo fixture","parameters":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}]))
 ] {std::fs::write(path.join(name),serde_json::to_vec(&body).unwrap()).unwrap();}
 std::fs::write(path.join("fixture_echo.skill.md"),"Original guidance").unwrap();
 std::fs::write(path.join("runner.py"),"print('fixture')").unwrap();std::fs::write(path.join("helper.py"),"VALUE=1").unwrap();path
}
#[test]fn real_package_io_pins_generation_and_withdraws_invalid_selected_publication() {
 let root=tempfile::tempdir().unwrap();let first=root.path().join("first");let catalogs=first.join("plugins/abilities");let path=package(&catalogs);
 SOURCE.with(|s|*s.borrow_mut()=Some(first));
 let cache=CatalogCache::<Fixture>::default();let fallback=Arc::new(Catalog::<Fixture>::empty());
 let current=cache.current(&fallback).unwrap();let binding=current.plugin_binding("fixture_echo").unwrap();
 assert!(matches!(package::prepare_invocation(&current,"fixture_echo",&json!({"text":"safe"}),"fixture-id",&binding).unwrap(),package::Invocation::Ready(_)));
 assert!(matches!(package::prepare_invocation(&current,"fixture_echo",&json!({"text":true}),"fixture-id",&binding).unwrap(),package::Invocation::Denied(_)));
 let declaration=path.join("plugin.json");let original=std::fs::read(&declaration).unwrap();
 std::fs::remove_file(&declaration).unwrap();
 std::os::unix::fs::symlink(path.join("missing-plugin"),&declaration).unwrap();
 assert!(Catalog::<Fixture>::load_snapshot(&catalogs).is_err());
 assert!(package::prepare_invocation(&current,"fixture_echo",&json!({"text":"safe"}),"fixture-id",&binding).is_err());
 std::fs::remove_file(&declaration).unwrap();std::fs::write(&declaration,original).unwrap();
 std::fs::write(path.join("helper.py"),"VALUE=2").unwrap();
 assert!(matches!(package::prepare_invocation(&current,"fixture_echo",&json!({"text":"safe"}),"fixture-id",&binding).unwrap(),package::Invocation::Denied(_)));
 let second=root.path().join("second");std::fs::create_dir_all(second.join("plugins/abilities/Custom/bad")).unwrap();
 std::fs::write(second.join("plugins/abilities/Custom/bad/bad.json"),b"invalid").unwrap();
 SOURCE.with(|s|*s.borrow_mut()=Some(second));assert!(cache.current(&fallback).is_err());
 SOURCE.with(|s|*s.borrow_mut()=Some(root.path().join("missing")));assert!(cache.current(&fallback).is_err());
 assert!(Catalog::<Fixture>::load_snapshot(&root.path().join("absent")).is_err());
 SOURCE.with(|s|*s.borrow_mut()=None);
}
#[test]fn unconfigured_optional_metadata_edits_refresh_without_loading_empty_fallback() {
 let root=tempfile::tempdir().unwrap();let optional=tempfile::tempdir().unwrap();package(root.path());
 let path=optional.path().join("optimization.json");
 let mut declaration=json!({"display_name":"Original optional title","kind":"ability","description":"Owned optional fixture","readiness":"ready","tools":["performance_work"]});
 std::fs::write(&path,serde_json::to_vec(&declaration).unwrap()).unwrap();OPTIONAL.with(|s|*s.borrow_mut()=Some(optional.path().to_owned()));
 let fallback=Arc::new(Catalog::<Fixture>::load(root.path()).unwrap());let cache=CatalogCache::<Fixture>::default();
 assert!(Arc::ptr_eq(&cache.current(&fallback).unwrap(),&fallback));
 declaration["display_name"]=json!("Updated optional title");std::fs::write(&path,serde_json::to_vec(&declaration).unwrap()).unwrap();
 let next=cache.current(&fallback).unwrap();assert_ne!(next.version(),fallback.version());
 assert_eq!(next.configuration_catalog(&json!({}),&json!({})).iter().find(|entry|entry["id"]=="optimization").unwrap()["name"],json!("Updated optional title"));
 assert!(Arc::ptr_eq(&cache.current(&fallback).unwrap(),&next));
 std::fs::write(&path,b"invalid").unwrap();assert!(cache.current(&fallback).is_err());
 let empty=Arc::new(Catalog::<Fixture>::empty());assert!(Arc::ptr_eq(&cache.current(&empty).unwrap(),&empty));
 std::fs::write(&path,serde_json::to_vec(&declaration).unwrap()).unwrap();
 assert!(Arc::ptr_eq(&cache.current(&fallback).unwrap(),&next));
 OPTIONAL.with(|s|*s.borrow_mut()=None);
 let removed=cache.current(&fallback).unwrap();
 assert!(!removed.configuration_catalog(&json!({}),&json!({})).iter().any(|entry|entry["id"]=="optimization"));
 OPTIONAL.with(|s|*s.borrow_mut()=Some(optional.path().to_owned()));
 assert!(Arc::ptr_eq(&cache.current(&fallback).unwrap(),&next));
 OPTIONAL.with(|s|*s.borrow_mut()=None);
}
