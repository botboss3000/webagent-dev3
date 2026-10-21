//! Published/default JSON transport, saved prompt composition and no-clobber first-start provenance.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::{path::PathBuf,io::Write};
use crate::digest;
pub fn published_root_at(ui:&std::path::Path)->Result<PathBuf> {
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
pub fn default_source(source:Option<&std::path::Path>)->Result<Option<PathBuf>> {
    if let Some(source)=source {return Ok(Some(source.to_owned()));}
    std::env::var_os("V3_UI_ROOT").map(|ui|published_root_at(&PathBuf::from(ui))).transpose()
}
fn read_selected_template(source:Option<&std::path::Path>,fallback_directory:&std::path::Path,name:&str,limit:u64)->Result<Vec<u8>> {
    use std::io::Read;
    let path=match source {
        Some(root)=>root.join("config").join(name),
        None=>fallback_directory.join(name),
    };
    let info=std::fs::symlink_metadata(&path)?;
    if !info.is_file()||info.file_type().is_symlink()||info.len()>limit {anyhow::bail!("Invalid default configuration file");}
    let mut bytes=Vec::new();std::fs::File::open(path)?.take(limit+1).read_to_end(&mut bytes)?;
    if bytes.len()as u64>limit {anyhow::bail!("Default configuration exceeds limit");}
    let value:Value=serde_json::from_slice(&bytes).map_err(|_|anyhow!("Invalid default configuration JSON"))?;
    if !value.is_object(){anyhow::bail!("Default configuration must be an object");}
    Ok(bytes)
}
/// Read bounded installation prompt defaults from the selected source or explicit packaged directory.
pub fn prompt_defaults_at(source:Option<&std::path::Path>,fallback_directory:&std::path::Path)->Result<Vec<u8>> {
    let selected=default_source(source)?;
    read_selected_template(selected.as_deref(),fallback_directory,"prompts.json",4*1024*1024)
}
/// Read bounded installation retention defaults with the same source-selection rules.
pub fn retention_defaults_at(source:Option<&std::path::Path>,fallback_directory:&std::path::Path)->Result<Vec<u8>> {
    let selected=default_source(source)?;
    read_selected_template(selected.as_deref(),fallback_directory,"tool-output-retention.json",64*1024)
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
pub fn initialize_defaults_at(root:&std::path::Path,source:Option<&std::path::Path>,fallback_directory:&std::path::Path,validate:impl FnOnce(&Value,&[u8])->Result<()>)->Result<()> {
    let source=default_source(source)?;
    let prompts=read_selected_template(source.as_deref(),fallback_directory,"prompts.json",4*1024*1024)?;
    let retention=read_selected_template(source.as_deref(),fallback_directory,"tool-output-retention.json",64*1024)?;
    let defaults:Value=serde_json::from_slice(&prompts)?;
    validate(&defaults,&retention)?;
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
/// Resolve prompts for a pinned model-call source revision. Reads config/prompts.json and saved leaf provenance.
pub fn resolved_prompts_at(root:&std::path::Path,source:Option<&std::path::Path>,fallback_directory:&std::path::Path)->Result<Value> {
    let mut defaults:Value=serde_json::from_slice(&prompt_defaults_at(source,fallback_directory)?)?;
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
#[cfg(test)]mod tests {
 use super::*;
 fn source()->tempfile::TempDir {
  let root=tempfile::tempdir().unwrap();std::fs::create_dir(root.path().join("config")).unwrap();
  std::fs::write(root.path().join("config/prompts.json"),br#"{"base_instructions":"old base","jev_guidance":"old sibling"}"#).unwrap();
  std::fs::write(root.path().join("config/tool-output-retention.json"),b"{}").unwrap();root
 }
 #[test]fn saved_setting_does_not_hide_an_updated_sibling_prompt(){
  let mut published=json!({"session_naming":{"prompt":"new","attempts_per_job":3}});
  let saved=json!({"session_naming":{"prompt":"old","attempts_per_job":5}});
  let baseline=json!({"session_naming":{"prompt":digest(&json!("old")),"attempts_per_job":digest(&json!(3))}});
  merge_prompt_overrides(&mut published,&saved,&baseline);
  assert_eq!(published,json!({"session_naming":{"prompt":"new","attempts_per_job":5}}));
 }
 #[test]fn configured_source_is_authoritative_and_bounded() {
  use std::os::unix::fs::symlink;
  let source=source();let path=source.path().join("config/prompts.json");
  std::fs::write(&path,b"{}").unwrap();
  let value:Value=serde_json::from_slice(&prompt_defaults_at(Some(source.path()),source.path()).unwrap()).unwrap();
  assert!(value.get("base_instructions").is_none());
  std::fs::write(&path,b"[]").unwrap();assert!(prompt_defaults_at(Some(source.path()),source.path()).is_err());
  std::fs::write(&path,b"PRIVATE_PROMPT_MARKER").unwrap();
  let error=prompt_defaults_at(Some(source.path()),source.path()).unwrap_err().to_string();assert!(!error.contains("PRIVATE_PROMPT_MARKER"));
  std::fs::write(&path,vec![b' ';4*1024*1024+1]).unwrap();assert!(prompt_defaults_at(Some(source.path()),source.path()).is_err());
  std::fs::remove_file(&path).unwrap();assert!(prompt_defaults_at(Some(source.path()),source.path()).is_err());
  symlink(source.path().join("config/tool-output-retention.json"),&path).unwrap();assert!(prompt_defaults_at(Some(source.path()),source.path()).is_err());
  assert!(published_root_at(source.path()).is_err());
  assert!(published_root_at(&source.path().join("missing/current/v3/ui")).is_err());
 }
 #[test]fn existing_malformed_saved_bytes_and_baseline_are_never_replaced() {
  let source=source();let state=tempfile::tempdir().unwrap();let dir=state.path().join("config");std::fs::create_dir(&dir).unwrap();
  let saved=dir.join("prompts.json");let baseline=dir.join(".prompt-defaults-baseline.json");
  std::fs::write(&saved,b"PRIVATE_INVALID_SAVED_JSON").unwrap();std::fs::write(&baseline,b"preserved prior baseline").unwrap();
  initialize_defaults_at(state.path(),Some(source.path()),source.path(),|_,_|Ok(())).unwrap();
  assert_eq!(std::fs::read(&saved).unwrap(),b"PRIVATE_INVALID_SAVED_JSON");
  assert_eq!(std::fs::read(&baseline).unwrap(),b"preserved prior baseline");
  assert!(resolved_prompts_at(state.path(),Some(source.path()),source.path()).is_err());
 }
 #[test]fn missing_baseline_does_not_guess_away_saved_leaf_overrides() {
  let source=source();let state=tempfile::tempdir().unwrap();let dir=state.path().join("config");std::fs::create_dir(&dir).unwrap();
  let saved=br#"{"base_instructions":"saved base","jev_guidance":"saved sibling","extension":{"keep":true}}"#;
  std::fs::write(dir.join("prompts.json"),saved).unwrap();
  initialize_defaults_at(state.path(),Some(source.path()),source.path(),|_,_|Ok(())).unwrap();
  let baseline=dir.join(".prompt-defaults-baseline.json");let bytes=std::fs::read(&baseline).unwrap();
  let provenance:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(provenance["defaults"],json!({}));
  let path=source.path().join("config/prompts.json");let mut changed:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
  changed["base_instructions"]=json!("changed source base");changed["jev_guidance"]=json!("changed source sibling");
  std::fs::write(&path,serde_json::to_vec(&changed).unwrap()).unwrap();
  initialize_defaults_at(state.path(),Some(source.path()),source.path(),|_,_|Ok(())).unwrap();
  let resolved=resolved_prompts_at(state.path(),Some(source.path()),source.path()).unwrap();
  assert_eq!(resolved["base_instructions"],"saved base");assert_eq!(resolved["jev_guidance"],"saved sibling");assert_eq!(resolved["extension"]["keep"],true);
  assert_eq!(std::fs::read(dir.join("prompts.json")).unwrap(),saved);assert_eq!(std::fs::read(baseline).unwrap(),bytes);
 }
 #[test]fn validation_precedes_state_writes_and_seeding_uses_loaded_bytes(){
  let source=source();let state=tempfile::tempdir().unwrap();
  let prompts=std::fs::read(source.path().join("config/prompts.json")).unwrap();
  let retention=std::fs::read(source.path().join("config/tool-output-retention.json")).unwrap();
  let error=initialize_defaults_at(state.path(),Some(source.path()),source.path(),|_,_|anyhow::bail!("Owner rejected defaults")).unwrap_err();
  assert_eq!(error.to_string(),"Owner rejected defaults");assert!(!state.path().join("config").exists());
  initialize_defaults_at(state.path(),Some(source.path()),source.path(),|p,r|{
   assert_eq!(p["base_instructions"],"old base");assert_eq!(r,retention.as_slice());
   // A publication change after validation must not reload either seed or its provenance.
   std::fs::remove_file(source.path().join("config/prompts.json"))?;
   std::fs::remove_file(source.path().join("config/tool-output-retention.json"))?;Ok(())
  }).unwrap();
  assert_eq!(std::fs::read(state.path().join("config/prompts.json")).unwrap(),prompts);
  assert_eq!(std::fs::read(state.path().join("config/tool-output-retention.json")).unwrap(),retention);
  let baseline:Value=serde_json::from_slice(&std::fs::read(state.path().join("config/.prompt-defaults-baseline.json")).unwrap()).unwrap();
  assert_eq!(baseline["defaults"]["base_instructions"],digest(&json!("old base")));
 }
}
