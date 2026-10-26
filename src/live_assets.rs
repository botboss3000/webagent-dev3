//! Immutable publication snapshots. Only the protected broker switches current.
use crate::*;
use std::sync::OnceLock;
/// Permit published native descriptions while rejecting changes to compiled tool contracts.
/// Load all native Administrator schema metadata from an authoritative external file.
/// Supported execution identities and runtime argument/authority checks remain fixed Rust code.
pub(crate) fn native_schema_snapshot_at(source:Option<&std::path::Path>)->Result<Vec<Value>> {
 use std::io::Read;
 let selected=default_source(source)?;
 let path=match selected {
  Some(root)=>root.join("config/administration/native-tools.json"),
  None=>if cfg!(test) {PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/administration/native-tools.json")}
   else {PathBuf::from("/usr/local/share/webagent/defaults/native-tools.json")},
 };
 let meta=std::fs::symlink_metadata(&path)?;
 if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>512*1024 {anyhow::bail!("Invalid native schema file");}
 let mut bytes=Vec::new();std::fs::File::open(path)?.take(512*1024+1).read_to_end(&mut bytes)?;
 if bytes.len()>512*1024 {anyhow::bail!("Native schemas exceed limit");}
 let schemas:Vec<Value>=serde_json::from_slice(&bytes).map_err(|_|anyhow!("Invalid native schemas"))?;
 let mut ids=std::collections::BTreeSet::new();
 if schemas.len()!=42+crate::source_control::TOOLS.len() {anyhow::bail!("Invalid native schema set");}
 for schema in &schemas {
  let name=schema["name"].as_str().ok_or_else(||anyhow!("Invalid native schema identity"))?;
  let Some(object)=schema.as_object() else {anyhow::bail!("Invalid native schema");};
  if !crate::tools::administrator::supported_schema_name(name)||!ids.insert(name)
   ||schema["type"]!="function"||object.get("strict").is_some_and(|value|!value.is_boolean())
   ||schema["description"].as_str().is_none_or(|text|text.trim().is_empty()||text.len()>8192)
   ||object.keys().any(|key|!matches!(key.as_str(),"type"|"name"|"description"|"strict"|"parameters"))
   ||schema["parameters"]["type"]!="object"||!schema["parameters"]["properties"].is_object()
   ||!schema["parameters"]["required"].is_array()||!schema["parameters"]["additionalProperties"].is_boolean() {
   anyhow::bail!("Invalid native schema interface");
  }
  let properties=schema["parameters"]["properties"].as_object().unwrap();
  let required=schema["parameters"]["required"].as_array().unwrap();
  let mut required_names=std::collections::BTreeSet::new();
  if required.len()>256||properties.len()>256||required.iter().any(|key|key.as_str().is_none_or(|key|!properties.contains_key(key)||!required_names.insert(key))) {
   anyhow::bail!("Invalid native schema parameters");
  }
 }
 Ok(crate::tools::normalize_native_schemas(schemas))
}
/// Preserve unconfigured local fixture/install fallback; immutable publication snapshots require their own complete file.
pub(crate) fn native_schemas_for_catalog(root:&std::path::Path,publication:bool)->Result<Vec<Value>> {
 let selected=root.ends_with("plugins/abilities").then(||root.parent().and_then(std::path::Path::parent)).flatten();
 let selected=selected.filter(|source|publication||std::env::var_os("V3_UI_ROOT").is_some()||source.join("config/administration/native-tools.json").exists());
 native_schema_snapshot_at(selected)
}

pub(crate) fn published_root() -> Option<PathBuf> {
    published_root_at(&PathBuf::from(std::env::var_os("V3_UI_ROOT")?)).ok()
}
fn published_root_at(ui:&std::path::Path)->Result<PathBuf> {webagent_configuration::prompts::published_root_at(ui)}
/// Explicit call snapshots and configured publication are authoritative; only an unconfigured installation uses packaged defaults.
fn default_source(source:Option<&std::path::Path>)->Result<Option<PathBuf>> {webagent_configuration::prompts::default_source(source)}

/// Main Chat guidance semantics are Chat-owned; the host supplies the installed defaults directory.
pub(crate) fn main_chat_prompts(root:&std::path::Path)->Result<Value> {
    webagent_chat::configuration::main_chat_prompts_at(root,None,&fallback_directory())
}
/// Atomically acknowledge the revision actually held by an API or loop catalog.
pub(crate) fn record_adoption(root:&std::path::Path,role:&str,catalog:&abilities::Catalog)->Result<()> {
    let Some(selected)=published_root()else{return Ok(());};
    if catalog.snapshot_root()!=selected.join("plugins/abilities"){anyhow::bail!("Catalog adoption not confirmed");}
    let revision=selected.parent().and_then(|p|p.file_name()).and_then(|s|s.to_str()).ok_or_else(||anyhow!("Invalid revision"))?;
    let engine=crate::engine_components::catalog()?;
    let value=json!({"revision":revision,"catalog_version":catalog.version(),"engine_revision":engine.revision});
    let dir=root.join("publication");std::fs::create_dir_all(&dir)?;
    let path=dir.join(format!("{role}.json"));
    if std::fs::read(&path).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok())==Some(value.clone()){return Ok(());}
    let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(&value)?)?;
    file.as_file().sync_all()?;file.persist(path).map_err(anyhow::Error::from)?;Ok(())
}
/// Read authoritative published defaults, or immutable packaged defaults when publication is not configured.
pub(crate) fn prompt_defaults() -> Result<Vec<u8>> {prompt_defaults_at(None)}
pub(crate) fn prompt_defaults_at(source:Option<&std::path::Path>)->Result<Vec<u8>> {webagent_configuration::prompts::prompt_defaults_at(source,&fallback_directory())}
pub(crate) fn retention_defaults()->Result<Vec<u8>> {webagent_configuration::prompts::retention_defaults_at(None,&fallback_directory())}

/// Publish first-start files and provenance from one pinned snapshot, without replacing saved configuration.
pub(crate) fn initialize_defaults(root:&std::path::Path)->Result<()> {initialize_defaults_at(root,None)}
fn initialize_defaults_at(root:&std::path::Path,source:Option<&std::path::Path>)->Result<()> {
 webagent_configuration::prompts::initialize_defaults_at(root,source,&fallback_directory(),|prompts,retention| {
  crate::api::live_configuration::validate_source_prompts(prompts)?;
  crate::tool_output::Policy::parse(retention)?;
  Ok(())
 })
}
/// Resolve the current installation defaults and saved prompt overrides.
pub(crate) fn resolved_prompts(root:&std::path::Path)->Result<Value> {
    resolved_prompts_at(root,None)
}
/// Resolve prompts for a pinned model-call source revision. Reads config/prompts.json and saved leaf provenance.
pub(crate) fn resolved_prompts_at(root:&std::path::Path,source:Option<&std::path::Path>)->Result<Value> {
 webagent_configuration::prompts::resolved_prompts_at(root,source,&fallback_directory())
}
/// Keep saved values that differ from migration defaults; let unchanged leaves inherit published updates.
fn merge_prompt_overrides(defaults:&mut Value,live:&Value,baseline:&Value) {
    if let Some(saved)=live.as_object() {
        for (key,value)in saved{
            let before=baseline.get(key).unwrap_or(&Value::Null);
            if value.is_object()&&before.is_object(){
                if !defaults[key].is_object(){defaults[key]=json!({});}
                merge_prompt_overrides(&mut defaults[key],value,before);
            }else if before.as_str()!=Some(digest(value).as_str()){defaults[key]=value.clone();}
        }
    }else{*defaults=live.clone();}
}
#[cfg(test)]mod prompt_tests{
 use super::*;
 #[test]fn saved_setting_does_not_hide_an_updated_sibling_prompt(){
  let mut published=json!({"session_naming":{"prompt":"new","attempts_per_job":3}});
  let saved=json!({"session_naming":{"prompt":"old","attempts_per_job":5}});
  let baseline=json!({"session_naming":{"prompt":digest(&json!("old")),"attempts_per_job":digest(&json!(3))}});
  merge_prompt_overrides(&mut published,&saved,&baseline);
  assert_eq!(published,json!({"session_naming":{"prompt":"new","attempts_per_job":5}}));
 }
}
/// Cache only validated immutable revisions; selected source errors remain errors.
pub(crate) fn catalog(fallback:&Arc<abilities::Catalog>) -> Result<Arc<abilities::Catalog>> {
    static CACHE:OnceLock<webagent_abilities::CatalogCache<abilities::NativeHost>>=OnceLock::new();
    CACHE.get_or_init(webagent_abilities::CatalogCache::default).current(fallback)
}

#[cfg(test)]mod external_default_tests {
 use super::*;
 fn source()->tempfile::TempDir {
  let root=tempfile::tempdir().unwrap();std::fs::create_dir(root.path().join("config")).unwrap();
  for name in ["prompts.json","tool-output-retention.json"] {
   std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config").join(name),root.path().join("config").join(name)).unwrap();
  }root
 }
 #[test]fn configured_source_is_authoritative_and_bounded() {
  use std::os::unix::fs::symlink;
  let source=source();let path=source.path().join("config/prompts.json");
  std::fs::write(&path,b"{}").unwrap();
  let value:Value=serde_json::from_slice(&prompt_defaults_at(Some(source.path())).unwrap()).unwrap();
  assert!(value.get("base_instructions").is_none());
  assert!(crate::api::live_configuration::validate_prompts(&value).is_err());
  std::fs::write(&path,b"[]").unwrap();assert!(prompt_defaults_at(Some(source.path())).is_err());
  std::fs::write(&path,b"PRIVATE_PROMPT_MARKER").unwrap();
  let error=prompt_defaults_at(Some(source.path())).unwrap_err().to_string();assert!(!error.contains("PRIVATE_PROMPT_MARKER"));
  std::fs::write(&path,vec![b' ';4*1024*1024+1]).unwrap();assert!(prompt_defaults_at(Some(source.path())).is_err());
  std::fs::remove_file(&path).unwrap();assert!(prompt_defaults_at(Some(source.path())).is_err());
  symlink(source.path().join("config/tool-output-retention.json"),&path).unwrap();assert!(prompt_defaults_at(Some(source.path())).is_err());
  assert!(published_root_at(source.path()).is_err());
  assert!(published_root_at(&source.path().join("missing/current/v3/ui")).is_err());
 }
 #[test]fn concurrent_seed_and_saved_override_adoption_preserve_provenance() {
  let source=source();let state=tempfile::tempdir().unwrap();
  std::thread::scope(|scope| {for _ in 0..2 {scope.spawn(||initialize_defaults_at(state.path(),Some(source.path())).unwrap());}});
  let saved_path=state.path().join("config/prompts.json");let baseline=state.path().join("config/.prompt-defaults-baseline.json");
  let first_baseline=std::fs::read(&baseline).unwrap();
  let mut saved:Value=serde_json::from_slice(&std::fs::read(&saved_path).unwrap()).unwrap();saved["jev_guidance"]=json!("saved sibling");
  std::fs::write(&saved_path,serde_json::to_vec(&saved).unwrap()).unwrap();let saved_bytes=std::fs::read(&saved_path).unwrap();
  let path=source.path().join("config/prompts.json");let mut next:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  next["base_instructions"]=json!("new inherited source");std::fs::write(path,serde_json::to_vec(&next).unwrap()).unwrap();
  initialize_defaults_at(state.path(),Some(source.path())).unwrap();
  assert_eq!(std::fs::read(&saved_path).unwrap(),saved_bytes);assert_eq!(std::fs::read(baseline).unwrap(),first_baseline);
  let effective=crate::provider::Provider::instructions_for_snapshot("",state.path(),Some(source.path())).unwrap();
  assert!(effective.contains("new inherited source"));assert!(effective.contains("saved sibling"));
 }
 #[test]fn invalid_source_never_creates_partial_runtime_defaults() {
  let source=source();let state=tempfile::tempdir().unwrap();
  std::fs::write(source.path().join("config/tool-output-retention.json"),br#"{"lifetime_calls":0}"#).unwrap();
  assert!(initialize_defaults_at(state.path(),Some(source.path())).is_err());assert!(!state.path().join("config/prompts.json").exists());
 }
 #[test]fn source_without_optional_naming_stays_without_naming_after_seed_and_resolution() {
  let source=source();let state=tempfile::tempdir().unwrap();let path=source.path().join("config/prompts.json");
  let mut prompts:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  prompts.as_object_mut().unwrap().remove("session_naming");
  crate::api::live_configuration::validate_source_prompts(&prompts).unwrap();
  std::fs::write(&path,serde_json::to_vec(&prompts).unwrap()).unwrap();
  initialize_defaults_at(state.path(),Some(source.path())).unwrap();
  let resolved=resolved_prompts_at(state.path(),Some(source.path())).unwrap();
  assert!(resolved.get("session_naming").is_none());
  assert!(crate::session_naming::validate_source_settings(&resolved["session_naming"]).is_err());
 }
 #[test]fn partial_source_naming_cannot_validate_using_another_default_snapshot() {
  let current=source();let state=tempfile::tempdir().unwrap();
  let path=current.path().join("config/prompts.json");
  let mut complete:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  complete["session_naming"]["attempts_per_job"]=json!(7);
  std::fs::write(&path,serde_json::to_vec(&complete).unwrap()).unwrap();
  initialize_defaults_at(state.path(),Some(current.path())).unwrap();
  let mut candidate=complete.clone();candidate["session_naming"]=json!({"prompt":"candidate-only title"});
  assert!(crate::api::live_configuration::validate_source_prompts(&candidate).is_err());
  let candidate_source=source();
  std::fs::write(candidate_source.path().join("config/prompts.json"),serde_json::to_vec(&candidate).unwrap()).unwrap();
  let fresh_state=tempfile::tempdir().unwrap();
  assert!(initialize_defaults_at(fresh_state.path(),Some(candidate_source.path())).is_err());
  assert!(!fresh_state.path().join("config/prompts.json").exists());
  // A saved partial section still inherits the explicitly selected complete source.
  std::fs::write(state.path().join("config/prompts.json"),br#"{"session_naming":{"prompt":"saved title"}}"#).unwrap();
  let resolved=resolved_prompts_at(state.path(),Some(current.path())).unwrap();
  assert_eq!(resolved["session_naming"]["prompt"],"saved title");
  assert_eq!(resolved["session_naming"]["attempts_per_job"],7);
  crate::session_naming::validate_source_settings(&resolved["session_naming"]).unwrap();
  assert_eq!(complete["session_naming"]["attempts_per_job"],7);
 }
 #[test]fn existing_malformed_saved_bytes_and_baseline_are_never_replaced() {
  let source=source();let state=tempfile::tempdir().unwrap();let dir=state.path().join("config");std::fs::create_dir(&dir).unwrap();
  let saved=dir.join("prompts.json");let baseline=dir.join(".prompt-defaults-baseline.json");
  std::fs::write(&saved,b"PRIVATE_INVALID_SAVED_JSON").unwrap();std::fs::write(&baseline,b"preserved prior baseline").unwrap();
  initialize_defaults_at(state.path(),Some(source.path())).unwrap();
  assert_eq!(std::fs::read(&saved).unwrap(),b"PRIVATE_INVALID_SAVED_JSON");
  assert_eq!(std::fs::read(&baseline).unwrap(),b"preserved prior baseline");
  assert!(resolved_prompts_at(state.path(),Some(source.path())).is_err());
 }
 #[test]fn missing_baseline_does_not_guess_away_saved_leaf_overrides() {
  let source=source();let state=tempfile::tempdir().unwrap();let dir=state.path().join("config");std::fs::create_dir(&dir).unwrap();
  let saved=br#"{"base_instructions":"saved base","jev_guidance":"saved sibling","extension":{"keep":true}}"#;
  std::fs::write(dir.join("prompts.json"),saved).unwrap();
  initialize_defaults_at(state.path(),Some(source.path())).unwrap();
  let baseline=dir.join(".prompt-defaults-baseline.json");let bytes=std::fs::read(&baseline).unwrap();
  let provenance:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(provenance["defaults"],json!({}));
  let path=source.path().join("config/prompts.json");let mut changed:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  changed["base_instructions"]=json!("changed source base");changed["jev_guidance"]=json!("changed source sibling");
  std::fs::write(&path,serde_json::to_vec(&changed).unwrap()).unwrap();
  initialize_defaults_at(state.path(),Some(source.path())).unwrap();
  let resolved=resolved_prompts_at(state.path(),Some(source.path())).unwrap();
  assert_eq!(resolved["base_instructions"],"saved base");assert_eq!(resolved["jev_guidance"],"saved sibling");assert_eq!(resolved["extension"]["keep"],true);
  assert_eq!(std::fs::read(dir.join("prompts.json")).unwrap(),saved);assert_eq!(std::fs::read(baseline).unwrap(),bytes);
 }

}

/// The root test crate supplies its own source fixture directory explicitly.
pub(crate) fn fallback_directory()->PathBuf {
 if cfg!(test) {PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config")}
 else {PathBuf::from("/usr/local/share/webagent/defaults")}
}

#[cfg(test)]
pub(crate) mod native_schema_file_tests {
 use super::*;
 pub(crate) fn source_fixture(with_abilities_directory:bool)->tempfile::TempDir {
  let root=tempfile::tempdir().unwrap();
  std::fs::create_dir_all(root.path().join("config/administration")).unwrap();
  if with_abilities_directory {std::fs::create_dir_all(root.path().join("plugins/abilities")).unwrap();}
  // Checked-in public source bytes only; no copied resolver, catalog or handler authority.
  std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config/administration/native-tools.json"),root.path().join("config/administration/native-tools.json")).unwrap();
  root
 }
 pub(crate) fn change_parameter(root:&std::path::Path) {
  let path=root.join("config/administration/native-tools.json");
  let mut schemas:Vec<Value>=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  let schema=schemas.iter_mut().find(|s|s["name"]=="code_index_scan").unwrap();
  assert_ne!(schema["parameters"]["properties"]["limit"]["maximum"],123);
  schema["parameters"]["properties"]["limit"]["maximum"]=json!(123);
  std::fs::write(&path,serde_json::to_vec(&schemas).unwrap()).unwrap();
 }
 #[test]
 fn parameter_only_change_versions_existing_and_missing_ability_roots_and_rejects_stale_snapshot() {
  for with_directory in [true,false] {
   let root=source_fixture(with_directory);let plugins=root.path().join("plugins/abilities");
   let catalog=crate::abilities::Catalog::load_current(&plugins).unwrap();
   assert!(catalog.native_snapshot_current().unwrap());
   change_parameter(root.path());
   let fresh=crate::abilities::Catalog::load_current(&plugins).unwrap();
   assert_ne!(catalog.version(),fresh.version(),"native parameters must identify the catalog even with a missing abilities directory");
   assert!(!catalog.native_snapshot_current().unwrap());assert!(fresh.native_snapshot_current().unwrap());
   let schema=fresh.all_schemas().into_iter().find(|s|s["name"]=="code_index_scan").unwrap();
   assert_eq!(schema["parameters"]["properties"]["limit"]["maximum"],123);
  }
 }
 #[test]
 fn unsupported_handler_and_invalid_authoritative_files_fail_closed() {
  let root=source_fixture(true);let path=root.path().join("config/administration/native-tools.json");
  let original=std::fs::read(&path).unwrap();let schemas:Vec<Value>=serde_json::from_slice(&original).unwrap();
  let mut unknown=schemas.clone();unknown[0]["name"]=json!("unsupported_native_handler");
  let mut duplicate=schemas.clone();duplicate[1]["name"]=duplicate[0]["name"].clone();
  let mut wrong_type=schemas.clone();wrong_type[0]["strict"]=json!(1);
  let mut invalid_required=schemas.clone();invalid_required[0]["parameters"]["required"]=json!(["not_declared"]);
  for bytes in [serde_json::to_vec(&unknown).unwrap(),serde_json::to_vec(&duplicate).unwrap(),serde_json::to_vec(&wrong_type).unwrap(),serde_json::to_vec(&invalid_required).unwrap(),b"not-json".to_vec(),b"{}".to_vec(),vec![b' ';512*1024+1]] {
   std::fs::write(&path,&bytes).unwrap();
   assert!(native_schema_snapshot_at(Some(root.path())).is_err());
   assert!(crate::abilities::Catalog::load_snapshot(&root.path().join("plugins/abilities")).is_err());
   assert_eq!(std::fs::read(&path).unwrap(),bytes);
  }
  std::fs::remove_file(&path).unwrap();assert!(native_schema_snapshot_at(Some(root.path())).is_err());
  std::os::unix::fs::symlink("missing-target",&path).unwrap();assert!(native_schema_snapshot_at(Some(root.path())).is_err());
  std::fs::remove_file(&path).unwrap();std::fs::create_dir(&path).unwrap();assert!(native_schema_snapshot_at(Some(root.path())).is_err());
 }
 #[test]
 fn actual_native_catalog_retains_owner_action_and_nullable_normalization() {
  let root=source_fixture(true);let raw:Vec<Value>=serde_json::from_slice(&std::fs::read(root.path().join("config/administration/native-tools.json")).unwrap()).unwrap();
  let raw_build=raw.iter().find(|s|s["name"]=="build_work").unwrap();
  assert_eq!(raw_build["parameters"]["properties"]["sync_decision"]["type"],"string");
  let catalog=crate::abilities::Catalog::load_current(&root.path().join("plugins/abilities")).unwrap();let schemas=catalog.all_schemas();
  let build=schemas.iter().find(|s|s["name"]=="build_work").unwrap();
  assert_eq!(build["parameters"]["properties"]["sync_decision"]["type"],json!(["string","null"]));
  assert!(build["parameters"]["properties"]["sync_decision"]["enum"].as_array().unwrap().contains(&Value::Null));
  assert_eq!(build["parameters"]["properties"]["action"]["type"],"string");
  let protected=schemas.iter().find(|s|s["name"]=="prompt_slot_write").unwrap();
  let action=&protected["parameters"]["properties"]["_action"];
  assert_eq!(action["type"],json!(["object","null"]));assert_eq!(action["required"],json!(["id"]));assert_eq!(action["additionalProperties"],false);
  assert_eq!(action["properties"]["id"]["maxLength"],128);assert_eq!(action["properties"]["separate_from"]["type"],json!(["string","null"]));
 }
}
