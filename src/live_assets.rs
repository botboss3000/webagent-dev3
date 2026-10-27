//! Immutable publication snapshots. Only the protected broker switches current.
use crate::*;
use std::sync::OnceLock;
/// Permit published native descriptions while rejecting changes to compiled tool contracts.
/// Load all native Administrator schema metadata from an authoritative external file.
/// Supported execution identities and runtime argument/authority checks remain fixed Rust code.
pub(crate) fn native_schema_snapshot_at(source:Option<&std::path::Path>)->Result<Vec<Value>> {
 use std::io::Read;
 // A pinned candidate supplies its own file; otherwise the live file (`live-files.json`).
 let bytes=match source {
  Some(root)=>{
   let path=root.join(NATIVE_TOOLS);
   let meta=std::fs::symlink_metadata(&path)?;
   if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>512*1024 {anyhow::bail!("Invalid native schema file");}
   let mut bytes=Vec::new();std::fs::File::open(path)?.take(512*1024+1).read_to_end(&mut bytes)?;
   if bytes.len()>512*1024 {anyhow::bail!("Native schemas exceed limit");}
   bytes
  },
  None=>webagent_configuration::live::read(NATIVE_TOOLS,512*1024,|bytes|native_schemas(bytes).map(|_|()))?,
 };
 native_schemas(&bytes)
}
/// Checkout path of the Administrator native tool schemas, beside their handlers.
pub(crate) const NATIVE_TOOLS:&str="src/engine/tools/native-tools.json";
fn native_schemas(bytes:&[u8])->Result<Vec<Value>> {
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
 // Installations (a configured publication) read the live file; validation and fixture
 // catalogs without one use their own source's file.
 let _=publication;
 let selected=selected.filter(|source|std::env::var_os("V3_UI_ROOT").is_none()&&source.join(NATIVE_TOOLS).exists());
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
/// The live prompts, assembled from their Markdown files.
pub(crate) fn resolved_prompts(root:&std::path::Path)->Result<Value> {
    resolved_prompts_at(root,None)
}
/// The prompts of a pinned candidate `source`, or the live files.
pub(crate) fn resolved_prompts_at(root:&std::path::Path,source:Option<&std::path::Path>)->Result<Value> {
 webagent_configuration::prompts::resolved_prompts_at(root,source,&fallback_directory())
}
/// Cache only validated immutable revisions; selected source errors remain errors.
pub(crate) fn catalog(fallback:&Arc<abilities::Catalog>) -> Result<Arc<abilities::Catalog>> {
    static CACHE:OnceLock<webagent_abilities::CatalogCache<abilities::NativeHost>>=OnceLock::new();
    CACHE.get_or_init(webagent_abilities::CatalogCache::default).current(fallback)
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
  std::fs::create_dir_all(root.path().join(NATIVE_TOOLS).parent().unwrap()).unwrap();
  if with_abilities_directory {std::fs::create_dir_all(root.path().join("plugins/abilities")).unwrap();}
  // Checked-in public source bytes only; no copied resolver, catalog or handler authority.
  std::fs::copy(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(NATIVE_TOOLS),root.path().join(NATIVE_TOOLS)).unwrap();
  root
 }
 pub(crate) fn change_parameter(root:&std::path::Path) {
  let path=root.join(NATIVE_TOOLS);
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
  let root=source_fixture(true);let path=root.path().join("src/engine/tools/native-tools.json");
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
  let root=source_fixture(true);let raw:Vec<Value>=serde_json::from_slice(&std::fs::read(root.path().join("src/engine/tools/native-tools.json")).unwrap()).unwrap();
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
