//! Immutable publication snapshots. Only the protected broker switches current.
use crate::*;
use std::sync::{Mutex as SyncMutex, OnceLock};
/// Permit published native descriptions while rejecting changes to compiled tool contracts.
pub(crate) fn native_schema_overrides(root:&std::path::Path)->Result<std::collections::BTreeMap<String,(String,String)>>{
    let mut overrides=std::collections::BTreeMap::new();
    if !root.ends_with("plugins/abilities"){return Ok(overrides);}
    let Some(source)=root.parent().and_then(std::path::Path::parent)else{return Ok(overrides);};
    let path=source.join("src/engine/tools/administrator-schemas.json");
    if !path.exists(){return Ok(overrides);}
    let bytes=std::fs::read(path)?;
    if bytes.len()>512*1024{anyhow::bail!("Native schema asset exceeds limit");}
    let live:Vec<Value>=serde_json::from_slice(&bytes)?;
    let baseline:Vec<Value>=serde_json::from_str(include_str!("engine/tools/administrator-schemas.json"))?;
    if live.len()!=baseline.len(){anyhow::bail!("Native schema interface change requires engine release");}
    for (current,compiled)in live.iter().zip(baseline.iter()){
        let mut a=current.clone();let mut b=compiled.clone();
        let description=a.as_object_mut().ok_or_else(||anyhow!("Invalid native schema"))?.remove("description").ok_or_else(||anyhow!("Missing native description"))?;
        let original=b.as_object_mut().ok_or_else(||anyhow!("Invalid native schema"))?.remove("description").ok_or_else(||anyhow!("Missing native description"))?;
        if a!=b{anyhow::bail!("Native schema interface change requires engine release");}
        let description=description.as_str().filter(|s|!s.is_empty()&&s.len()<=8192).ok_or_else(||anyhow!("Invalid native description"))?;
        let original=original.as_str().ok_or_else(||anyhow!("Invalid compiled description"))?;
        if description!=original{overrides.insert(current["name"].as_str().ok_or_else(||anyhow!("Missing native name"))?.to_owned(),(original.to_owned(),description.to_owned()));}
    }Ok(overrides)
}
/// Resolve only a broker-owned UUID release beneath the configured current asset pointer.
pub(crate) fn published_root() -> Option<PathBuf> {
    published_root_at(&PathBuf::from(std::env::var_os("V3_UI_ROOT")?)).ok()
}
fn published_root_at(ui:&std::path::Path)->Result<PathBuf> {
    if !ui.ends_with("current/v3/ui") { anyhow::bail!("Invalid published configuration root"); }
    let assets=ui.parent().and_then(std::path::Path::parent).and_then(std::path::Path::parent)
        .ok_or_else(||anyhow!("Invalid published configuration root"))?;
    let selected=assets.join("current").canonicalize()?;
    if selected.parent()!=Some(assets.join("releases").as_path())
        || selected.file_name().and_then(|s|s.to_str()).is_none_or(|s|uuid::Uuid::parse_str(s).is_err()) {
        anyhow::bail!("Invalid published configuration revision");
    }
    Ok(selected.join("v3"))
}
/// Explicit call snapshots and configured publication are authoritative; only an unconfigured installation uses packaged defaults.
fn default_source(source:Option<&std::path::Path>)->Result<Option<PathBuf>> {
    if let Some(source)=source {return Ok(Some(source.to_owned()));}
    std::env::var_os("V3_UI_ROOT").map(|ui|published_root_at(&PathBuf::from(ui))).transpose()
}
fn read_default_template(source:Option<&std::path::Path>,name:&str,limit:u64)->Result<Vec<u8>> {
    use std::io::Read;
    let path=match default_source(source)? {
        Some(root)=>root.join("config").join(name),
        None=>if cfg!(test) {PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("config").join(name)}
            else {PathBuf::from("/usr/local/share/webagent/defaults").join(name)},
    };
    let info=std::fs::symlink_metadata(&path)?;
    if !info.is_file()||info.file_type().is_symlink()||info.len()>limit {anyhow::bail!("Invalid default configuration file");}
    let mut bytes=Vec::new();std::fs::File::open(path)?.take(limit+1).read_to_end(&mut bytes)?;
    if bytes.len()as u64>limit {anyhow::bail!("Default configuration exceeds limit");}
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|anyhow!("Invalid default configuration JSON"))?;
    if !value.is_object(){anyhow::bail!("Default configuration must be an object");}
    Ok(bytes)
}
/// Main Chat guidance has no compiled or last-good fallback. An installation
/// JSON file, when present, is authoritative even when it omits these skills.
pub(crate) fn main_chat_prompts(root:&std::path::Path)->Result<Value> {
    let saved=root.join("config/prompts.json");
    let source=default_source(None)?;
    fn section(path:&std::path::Path)->Result<Option<Value>> {
        let meta=std::fs::symlink_metadata(path)?;
        if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>4*1024*1024{anyhow::bail!("Invalid Main Chat prompts");}
        let value:Value=serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(value.get("main_chat_skills").cloned())
    }
    if saved.exists(){if let Some(value)=section(&saved)?{return Ok(value);}}
    let defaults:Value=serde_json::from_slice(&prompt_defaults_at(source.as_deref())?)?;
    Ok(defaults.get("main_chat_skills").cloned().unwrap_or(Value::Null))
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
pub(crate) fn prompt_defaults_at(source:Option<&std::path::Path>)->Result<Vec<u8>> {
    read_default_template(source,"prompts.json",4*1024*1024)
}
pub(crate) fn retention_defaults()->Result<Vec<u8>> {
    read_default_template(None,"tool-output-retention.json",64*1024)
}
fn seed_file(path:&std::path::Path,bytes:&[u8])->Result<bool> {
    let directory=path.parent().ok_or_else(||anyhow!("Missing configuration directory"))?;
    let mut file=tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(bytes)?;file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_)=>{std::fs::File::open(directory)?.sync_all()?;Ok(true)},
        Err(error)if error.error.kind()==std::io::ErrorKind::AlreadyExists=>Ok(false),
        Err(error)=>Err(error.error.into()),
    }
}
/// Publish first-start files and provenance from one pinned snapshot, without replacing saved configuration.
pub(crate) fn initialize_defaults(root:&std::path::Path)->Result<()> {initialize_defaults_at(root,None)}
fn initialize_defaults_at(root:&std::path::Path,source:Option<&std::path::Path>)->Result<()> {
    let source=default_source(source)?;
    let prompts=prompt_defaults_at(source.as_deref())?;
    let retention=read_default_template(source.as_deref(),"tool-output-retention.json",64*1024)?;
    let defaults:Value=serde_json::from_slice(&prompts)?;
    crate::api::live_configuration::validate_source_prompts(&defaults)?;
    crate::tool_output::Policy::parse(&retention)?;
    let directory=root.join("config");std::fs::create_dir_all(&directory)?;
    let lock=std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(directory.join(".prompts.lock"))?;
    fs2::FileExt::lock_exclusive(&lock)?;
    let seeded=seed_file(&directory.join("prompts.json"),&prompts)?;
    fn hashes(value:&Value)->Value {
        match value.as_object() {
            Some(object)=>json!(object.iter().map(|(k,v)|(k.clone(),hashes(v))).collect::<std::collections::BTreeMap<_,_>>()),
            None=>json!(digest(value)),
        }
    }
    // Unknown provenance on an existing installation must preserve every saved leaf as an override.
    let baseline=json!({"schema_version":1,"defaults":if seeded {hashes(&defaults)}else{json!({})}});
    seed_file(&directory.join(".prompt-defaults-baseline.json"),&serde_json::to_vec(&baseline)?)?;
    seed_file(&directory.join("tool-output-retention.json"),&retention)?;
    Ok(())
}
/// Resolve the current installation defaults and saved prompt overrides.
pub(crate) fn resolved_prompts(root:&std::path::Path)->Result<Value> {
    resolved_prompts_at(root,None)
}
/// Resolve prompts for a pinned model-call source revision. Reads config/prompts.json and saved leaf provenance.
pub(crate) fn resolved_prompts_at(root:&std::path::Path,source:Option<&std::path::Path>)->Result<Value> {
    let mut defaults:Value=serde_json::from_slice(&prompt_defaults_at(source)?)?;
    let live_path=root.join("config/prompts.json");
    let live:Value=match std::fs::read(live_path) {
        Ok(bytes)=>serde_json::from_slice(&bytes)?,
        Err(e) if e.kind()==std::io::ErrorKind::NotFound=>return Ok(defaults),
        Err(e)=>return Err(e.into()),
    };
    let baseline:Value=std::fs::read(root.join("config/.prompt-defaults-baseline.json")).ok()
        .and_then(|b|serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
    if !live.is_object(){anyhow::bail!("Invalid live prompts");}
    let baseline=baseline.get("defaults").unwrap_or(&baseline);
    merge_prompt_overrides(&mut defaults,&live,baseline);
    Ok(defaults)
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
type CatalogCache=(HashMap<PathBuf,Arc<abilities::Catalog>>,Option<Arc<abilities::Catalog>>);
/// Cache complete immutable catalogs by revision and retain the last valid catalog on rejection.
pub(crate) fn catalog(fallback:&Arc<abilities::Catalog>) -> Arc<abilities::Catalog> {
    let Some(root)=published_root().map(|root|root.join("plugins/abilities")) else{return fallback.clone();};
    static CACHE:OnceLock<SyncMutex<CatalogCache>>=OnceLock::new();
    let mut cache=CACHE.get_or_init(||SyncMutex::new((HashMap::new(),None))).lock().unwrap_or_else(|e|e.into_inner());
    if let Some(value)=cache.0.get(&root){return value.clone();}
    match abilities::Catalog::load_snapshot(&root) {
        Ok(value)=>{
            let value=Arc::new(value);
            if cache.0.len()>=8 {cache.0.clear();}
            cache.0.insert(root,value.clone());cache.1=Some(value.clone());value
        },
        Err(_)=>{tracing::error!("Published ability snapshot rejected");cache.1.clone().unwrap_or_else(||fallback.clone())}
    }
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
