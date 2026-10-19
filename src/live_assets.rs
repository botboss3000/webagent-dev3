//! Immutable publication snapshots. Only the protected broker switches current.
use crate::*;
use std::sync::{Mutex as SyncMutex, OnceLock};
/// Permit published native descriptions while rejecting changes to compiled tool contracts.
pub(crate) fn native_schema_overrides(root:&std::path::Path)->Result<std::collections::BTreeMap<String,(String,String)>>{
    let mut overrides=std::collections::BTreeMap::new();
    if !root.ends_with("plugins/abilities"){return Ok(overrides);}
    let Some(source)=root.parent().and_then(std::path::Path::parent)else{return Ok(overrides);};
    let path=source.join("src/agent_loop/tools/administrator-schemas.json");
    if !path.exists(){return Ok(overrides);}
    let bytes=std::fs::read(path)?;
    if bytes.len()>512*1024{anyhow::bail!("Native schema asset exceeds limit");}
    let live:Vec<Value>=serde_json::from_slice(&bytes)?;
    let baseline:Vec<Value>=serde_json::from_str(include_str!("agent_loop/tools/administrator-schemas.json"))?;
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
    let ui=PathBuf::from(std::env::var_os("V3_UI_ROOT")?);
    if !ui.ends_with("current/v3/ui") { return None; }
    let assets=ui.parent()?.parent()?.parent()?;
    let selected=assets.join("current").canonicalize().ok()?;
    if selected.parent()!=Some(assets.join("releases").as_path())
        || uuid::Uuid::parse_str(selected.file_name()?.to_str()?).is_err() { return None; }
    Some(selected.join("v3"))
}
/// Main Chat guidance has no compiled or last-good fallback. An installation
/// JSON file, when present, is authoritative even when it omits these skills.
pub(crate) fn main_chat_prompts(root:&std::path::Path)->Result<Value> {
    let saved=root.join("config/prompts.json");
    let source=published_root().unwrap_or_else(||PathBuf::from(".")).join("config/prompts.json");
    fn section(path:&std::path::Path)->Result<Option<Value>> {
        let meta=std::fs::symlink_metadata(path)?;
        if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>4*1024*1024{anyhow::bail!("Invalid Main Chat prompts");}
        let value:Value=serde_json::from_slice(&std::fs::read(path)?)?;
        Ok(value.get("main_chat_skills").cloned())
    }
    if saved.exists(){if let Some(value)=section(&saved)?{return Ok(value);}}
    Ok(section(&source)?.unwrap_or(Value::Null))
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
/// Read defaults from the currently published source template; never mutate runtime overrides.
pub(crate) fn prompt_defaults() -> Result<Vec<u8>> {
    prompt_defaults_at(published_root().as_deref())
}
/// Merge a bounded partial source template onto shipped defaults before domain validation.
pub(crate) fn prompt_defaults_at(source:Option<&std::path::Path>)->Result<Vec<u8>> {
    let path=source.unwrap_or_else(||std::path::Path::new(".")).join("config/prompts.json");
    let metadata=std::fs::symlink_metadata(&path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len()>4*1024*1024 { anyhow::bail!("Invalid published prompts"); }
    let bytes=std::fs::read(path)?;
    let value:Value=serde_json::from_slice(&bytes)?;
    if !value.is_object(){anyhow::bail!("Invalid published prompts");}
    let mut defaults:Value=serde_json::from_slice(include_bytes!("../config/prompts.json"))?;
    fn merge(defaults:&mut Value,value:&Value){
        if let Some(object)=value.as_object(){
            if !defaults.is_object(){*defaults=json!({});}
            for (key,value)in object{merge(&mut defaults[key],value);}
        }else{*defaults=value.clone();}
    }
    // Legacy prompt-only files retain optional shipped defaults. Present values,
    // including invalid ones, replace defaults and remain subject to domain validation.
    merge(&mut defaults,&value);
    Ok(serde_json::to_vec(&defaults)?)
}
/// Seed no-clobber leaf hashes so source defaults can change without overwriting saved overrides.
pub(crate) fn initialize_prompt_baseline(root:&std::path::Path)->Result<()> {
    let path=root.join("config/.prompt-defaults-baseline.json");
    let defaults:Value=serde_json::from_slice(include_bytes!("../config/prompts.json"))?;
    fn hashes(value:&Value)->Value{
        match value.as_object(){
            Some(object)=>json!(object.iter().map(|(k,v)|(k.clone(),hashes(v))).collect::<std::collections::BTreeMap<_,_>>()),
            None=>json!(digest(value)),
        }
    }
    let value=json!({"schema_version":1,"defaults":hashes(&defaults)});
    let mut file=tempfile::NamedTempFile::new_in(root.join("config"))?;
    file.write_all(&serde_json::to_vec(&value)?)?;file.as_file().sync_all()?;
    match file.persist_noclobber(path) {
        Ok(_)=>{},
        Err(e) if e.error.kind()==std::io::ErrorKind::AlreadyExists=>{},
        Err(e)=>return Err(e.error.into()),
    }Ok(())
}
/// Resolve the current installation defaults and saved prompt overrides.
pub(crate) fn resolved_prompts(root:&std::path::Path)->Result<Value> {
    resolved_prompts_at(root,published_root().as_deref())
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
