//! Immutable publication snapshots. Only the protected broker switches current.
use crate::*;
use std::sync::OnceLock;
/// Administrator tool schemas from the tool folders under a catalog root
/// (`plugins/abilities`), normalized for the model. A missing or invalid folder leaves
/// only its own tool out; release validation refuses any such problem.
pub(crate) fn native_schemas_for_catalog(root:&std::path::Path,_publication:bool)->Result<Vec<Value>> {
 let loaded=webagent_administration::schemas::load(root)?;
 Ok(crate::tools::normalize_native_schemas(loaded.schemas))
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
 /// A source root whose Administrator tool folders are copies of the checked-in ones,
 /// all under the retired `v3_build` folder the catalog does not list as an ability.
 pub(crate) fn source_fixture()->tempfile::TempDir {
  let root=tempfile::tempdir().unwrap();
  let checkout=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities/Administrator");
  let target=root.path().join("plugins/abilities/Administrator/v3_build/tools");
  for ability in std::fs::read_dir(&checkout).unwrap().flatten() {
   let Ok(tools)=std::fs::read_dir(ability.path().join("tools")) else {continue};
   for tool in tools.flatten().filter(|tool|tool.path().join("tool.json").is_file()) {
    let folder=target.join(tool.file_name());
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::copy(tool.path().join("tool.json"),folder.join("tool.json")).unwrap();
   }
  }
  root
 }
 fn definition(root:&std::path::Path,name:&str)->PathBuf {
  root.join("plugins/abilities/Administrator/v3_build/tools").join(name).join("tool.json")
 }
 pub(crate) fn change_parameter(root:&std::path::Path) {
  let path=definition(root,"code_index_scan");
  let mut tool:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  assert_ne!(tool["parameters"]["properties"]["limit"]["maximum"],123);
  tool["parameters"]["properties"]["limit"]["maximum"]=json!(123);
  std::fs::write(&path,serde_json::to_vec(&tool).unwrap()).unwrap();
 }
 #[test]
 fn parameter_change_versions_the_catalog_and_rejects_the_stale_snapshot() {
  let root=source_fixture();let plugins=root.path().join("plugins/abilities");
  let catalog=crate::abilities::Catalog::load_current(&plugins).unwrap();
  assert!(catalog.native_snapshot_current().unwrap());
  change_parameter(root.path());
  let fresh=crate::abilities::Catalog::load_current(&plugins).unwrap();
  assert_ne!(catalog.version(),fresh.version(),"Administrator parameters identify the catalog");
  assert!(!catalog.native_snapshot_current().unwrap());assert!(fresh.native_snapshot_current().unwrap());
  let schema=fresh.all_schemas().into_iter().find(|s|s["name"]=="code_index_scan").unwrap();
  assert_eq!(schema["parameters"]["properties"]["limit"]["maximum"],123);
 }
 #[test]
 fn an_invalid_folder_withdraws_only_its_tool() {
  let root=source_fixture();let plugins=root.path().join("plugins/abilities");
  let path=definition(root.path(),"code_index_scan");
  for bytes in [b"not-json".to_vec(),b"{}".to_vec(),serde_json::to_vec(&json!({"name":"other"})).unwrap()] {
   std::fs::write(&path,&bytes).unwrap();
   let catalog=crate::abilities::Catalog::load_current(&plugins).unwrap();
   let names:Vec<Value>=catalog.all_schemas().into_iter().map(|s|s["name"].clone()).collect();
   assert!(!names.contains(&json!("code_index_scan")));
   assert!(names.contains(&json!("code_index_lookup")));
   let loaded=webagent_administration::schemas::load(&plugins).unwrap();
   assert!(!loaded.problems.is_empty(),"release validation sees the problem");
  }
 }
 #[test]
 fn actual_native_catalog_retains_owner_action_and_nullable_normalization() {
  let root=source_fixture();
  let raw:Value=serde_json::from_slice(&std::fs::read(definition(root.path(),"build_work")).unwrap()).unwrap();
  assert_eq!(raw["parameters"]["properties"]["sync_decision"]["type"],"string");
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
