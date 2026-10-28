//! Engine-owned engine discovery, activation and disposable process generations.
//! Catalog entries never grant tools or authority; only registered contracts run.
// @code-index {"version":1,"feature":"engine-components","aliases":["live engine modules","context service","component catalog"],"purpose":"Engine catalog, control fencing and disposable generation lifecycle. Config: config/engine-components.json seeds initial disabled defaults; data-root config/engine-components.json owns live revision/disabled. Registered engine descriptors own bounded execution limits and runtime requirements; pure result contracts never grant authority.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled"]},{"path":"plugins/engine/context-assembly/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]},{"path":"plugins/engine/background-dispatch/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]}],"opportunities":[]}}
use anyhow::{anyhow,Result};
use crate::{status,uuid};
use serde_json::{json,Value};
use std::{path::PathBuf,sync::{Arc,atomic::Ordering},time::Duration,io::Write};
use sha2::Digest;
use tokio_util::sync::CancellationToken;
use crate::{digest,defaults as live_assets};
use crate::context::{input as context_input,assembly as context_assembly,accounting as context_accounting,compaction as context_compaction};
use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex,OnceLock};
pub const CONTEXT:&str="context-assembly";
pub const BACKGROUND:&str="background-dispatch";
pub const ASSETS:&str="context-assets";
pub const CAPABILITY:&str="capability-context";
pub const PROJECTION:&str="provider-projection";
pub const WORKING_COMPONENTS:[&str;5]=[CONTEXT,BACKGROUND,ASSETS,CAPABILITY,PROJECTION];
// @code-index {"version":1,"feature":"context-component-registration","purpose":"Register bounded context preparation operations and reviewed build closures. Config: plugins/engine context-assets, capability-context and provider-projection descriptors own execution limits; host retains authority.","span_lines":30,"configuration":{"status":"configured","references":[{"path":"plugins/engine/context-assets/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]},{"path":"plugins/engine/capability-context/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]},{"path":"plugins/engine/provider-projection/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]}],"opportunities":[]},"aliases":[]}
/// Bind newly registered operations to exact reviewed build closures; declarations cannot choose commands.
fn context_registration(operation:&str)->Option<(&'static str,&'static str,Vec<&'static str>)> {
 let (id,service,implementation,contracts)=match operation {
  "assets.plan"=>(ASSETS,"assets-service","context-assets","assets-contracts"),
  "capability.prepare"=>(CAPABILITY,"capability-service","capability-context","capability-contracts"),
  "provider.project"=>(PROJECTION,"provider-service","provider-projection","provider-contracts"),_=>return None,
 };
 let names=match operation {
  "assets.plan"=>vec!["crates/assets-service","crates/context-assets","crates/assets-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],
  "capability.prepare"=>vec!["crates/capability-service","crates/capability-context","crates/capability-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],
  _=>vec!["crates/provider-service","crates/provider-projection","crates/provider-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],
 };
 let _=(service,implementation,contracts);
 Some((id,match operation {"assets.plan"=>"webagent-assets-service","capability.prepare"=>"webagent-capability-service",_=>"webagent-provider-service"},names))
}
/// Module services share the engine catalog; each registration fixes its package and build closure.
fn module_registration(operation:&str)->Option<(&'static str,&'static str,Vec<&'static str>)> {
 match operation {
  "module.toy"=>Some(("module-toy","webagent-toy-service",vec!["crates/module-toy","crates/module-service","crates/module-interface","crates/engine-service","Cargo.toml","Cargo.lock"])),
  _=>None,
 }
}
/// Engine operations and module services resolve through one registration lookup.
fn registration(operation:&str)->Option<(&'static str,&'static str,Vec<&'static str>)> {context_registration(operation).or_else(||module_registration(operation))}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {pub max_request_bytes:usize,pub max_response_bytes:usize,pub timeout_ms:u64,pub max_concurrency:usize}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {pub package:String,pub source_inputs:Vec<String>,pub toolchain:String,pub features:Vec<String>,pub runtime_assets:Vec<String>}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Descriptor {
 pub schema_version:u32,pub id:String,pub group:String,pub interface_version:u32,
 pub implementation_version:String,pub operation:String,pub modes:Vec<String>,
 pub runtime_requirements:Vec<String>,pub build:Build,pub executable:String,
 #[serde(default)]pub artifact_sha256:Option<String>,pub limits:Limits,
}
/// Keep catalog and control identities bounded and safe as organizational labels.
fn valid_id(id:&str)->bool {!id.is_empty()&&id.len()<=64&&id.bytes().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||c==b'-')}
impl Descriptor {
 /// Only reviewed fixed interfaces and confined build recipes may enter the engine catalog.
 pub fn validate(&self)->Result<()> {
  if self.schema_version!=1||!valid_id(&self.id)||self.interface_version!=if self.operation=="context.prepare"{2}else if self.operation=="background.plan"{webagent_background_contracts::INTERFACE_VERSION}else{1}||(!matches!(self.operation.as_str(),"context.prepare"|"background.plan"|"boot.plan")&&registration(&self.operation).is_none())
   ||!valid_id(&self.group)||module_registration(&self.operation).is_some()!=(self.group=="module")||self.implementation_version.is_empty()||self.implementation_version.len()>64
   ||self.modes!=["live","linked"]||self.runtime_requirements.len()>16||self.runtime_requirements.iter().any(|id|!valid_id(id))
   ||self.executable!=format!("artifact/{}",self.build.package)
   ||match self.operation.as_str() {
    "context.prepare"=>self.build.package!="webagent-context-service",
    "background.plan"=>self.id!=BACKGROUND||self.build.package!="webagent-background-service",
    "boot.plan"=>self.id!="boot-supervisor"||self.build.package!="webagent-boot-service",_=>registration(&self.operation).is_none_or(|(id,package,_)|self.id!=id||self.build.package!=package)
   }
   ||self.build.toolchain!="1.90.0"||self.build.source_inputs!=if self.operation=="context.prepare" {vec!["crates/context-service","crates/context-assembly","crates/context-contracts","Cargo.toml","Cargo.lock"]}else if self.operation=="boot.plan"{vec!["crates/boot-service","crates/boot-supervisor","crates/boot-contracts","crates/engine-service","Cargo.toml","Cargo.lock"]}else if self.operation=="background.plan"{vec!["crates/background-service","crates/background-dispatch","crates/background-contracts","crates/engine-service","Cargo.toml","Cargo.lock"]}else{registration(&self.operation).map(|(_,_,inputs)|inputs).unwrap_or_default()}
   ||!self.build.features.is_empty()||!self.build.runtime_assets.is_empty()
   ||self.limits.max_request_bytes==0||self.limits.max_request_bytes>16*1024*1024
   ||self.limits.max_response_bytes==0||self.limits.max_response_bytes>16*1024*1024
   ||self.limits.timeout_ms==0||self.limits.timeout_ms>30000||self.limits.max_concurrency!=1
   ||(matches!(self.operation.as_str(),"background.plan"|"boot.plan")||module_registration(&self.operation).is_some())&&(self.limits.max_request_bytes>65536||self.limits.max_response_bytes>65536||self.limits.timeout_ms>1000)
   ||self.artifact_sha256.as_ref().is_some_and(|s|s.len()!=64||!s.bytes().all(|c|c.is_ascii_hexdigit())) {
    anyhow::bail!("Engine contract rejected");
  }Ok(())
 }
}
#[derive(Clone)]
pub struct Catalog {pub revision:String,pub root:Option<PathBuf>,pub entries:BTreeMap<String,Descriptor>,pub withdrawn:std::collections::BTreeSet<String>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {id:String,descriptor_sha256:String,artifact_sha256:String}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {schema_version:u32,revision:String,components:Vec<Member>,withdrawn:Vec<String>}
/// Validate broker-owned membership, withdrawals and every descriptor/executable before activation.
pub fn load(root:&std::path::Path,revision:&str)->Result<Catalog>{
 let dir=root.join("plugins/engine");let metadata=dir.join("catalog.json");
 let info=std::fs::symlink_metadata(&metadata)?;
 if !info.is_file()||info.file_type().is_symlink()||info.len()>65536{anyhow::bail!("Engine membership identity");}
 let namespace:Namespace=serde_json::from_slice(&std::fs::read(&metadata)?)?;
 if namespace.schema_version!=1||uuid::Uuid::parse_str(&namespace.revision).is_err()
  ||revision!="candidate"&&namespace.revision!=revision||namespace.components.len()>256||namespace.withdrawn.len()>256
  ||namespace.components.windows(2).any(|w|w[0].id>=w[1].id)||namespace.withdrawn.windows(2).any(|w|w[0]>=w[1])
  ||namespace.withdrawn.iter().any(|id|!valid_id(id)){anyhow::bail!("Engine membership rejected");}
 let withdrawn=namespace.withdrawn.iter().cloned().collect::<std::collections::BTreeSet<_>>();
 let mut entries=BTreeMap::new();
 use sha2::Digest;
 for item in std::fs::read_dir(&dir)? {
  let item=item?;if item.file_name()=="catalog.json"{continue;}
  let meta=item.file_type()?;
  if !meta.is_dir()||meta.is_symlink(){anyhow::bail!("Invalid engine package");}
  let path=item.path().join("component.json");let meta=std::fs::symlink_metadata(&path)?;
  if !meta.is_file()||meta.file_type().is_symlink()||meta.len()>32768 {anyhow::bail!("Invalid engine descriptor");}
  let bytes=std::fs::read(&path)?;let descriptor:Descriptor=serde_json::from_slice(&bytes)?;descriptor.validate()?;
  if item.file_name().to_str()!=Some(descriptor.id.as_str())||entries.contains_key(&descriptor.id)||withdrawn.contains(&descriptor.id){anyhow::bail!("Duplicate engine component");}
  let member=namespace.components.iter().find(|member|member.id==descriptor.id).ok_or_else(||anyhow!("Engine membership incomplete"))?;
  let hash=descriptor.artifact_sha256.as_ref().ok_or_else(||anyhow!("Unsealed engine artifact"))?;
  if member.descriptor_sha256!=hex::encode(sha2::Sha256::digest(&bytes))||member.artifact_sha256!=*hash{anyhow::bail!("Engine membership hash mismatch");}
  let executable=item.path().join(&descriptor.executable);let artifact_dir=std::fs::symlink_metadata(item.path().join("artifact"))?;
  let meta=std::fs::symlink_metadata(&executable)?;
  use std::os::unix::fs::PermissionsExt;
  if !artifact_dir.is_dir()||artifact_dir.file_type().is_symlink()||!meta.is_file()||meta.file_type().is_symlink()
   ||meta.len()>128*1024*1024||meta.permissions().mode()&0o022!=0||meta.permissions().mode()&0o111==0 {
    anyhow::bail!("Invalid engine executable");
  }
  if hex::encode(sha2::Sha256::digest(std::fs::read(&executable)?))!=*hash {anyhow::bail!("Engine artifact mismatch");}
  entries.insert(descriptor.id.clone(),descriptor);
 }
 if entries.len()!=namespace.components.len(){anyhow::bail!("Engine membership incomplete");}
 validate_graph_with_withdrawals(&entries,&withdrawn)?;
 Ok(Catalog{revision:namespace.revision,root:Some(root.to_owned()),entries,withdrawn})
}
/// Immutable snapshots reject malformed revisions while deliberate absence is valid removal.
pub fn catalog()->Result<Arc<Catalog>>{
 let Some(root)=live_assets::published_root() else {
  let descriptor:Descriptor=serde_json::from_value(json!({"schema_version":1,"id":"context-assembly","group":"engine-loop","interface_version":2,"implementation_version":"0.2.1","operation":"context.prepare","modes":["live","linked"],"runtime_requirements":[],"build":{"package":"webagent-context-service","source_inputs":["crates/context-service","crates/context-assembly","crates/context-contracts","Cargo.toml","Cargo.lock"],"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/webagent-context-service","limits":{"max_request_bytes":16777216,"max_response_bytes":16777216,"timeout_ms":30000,"max_concurrency":1}}))?;
  descriptor.validate()?;
  let mut embedded=Catalog{revision:"embedded".into(),root:None,entries:BTreeMap::from([(CONTEXT.into(),descriptor),(BACKGROUND.into(),serde_json::from_value(json!({"schema_version":1,"id":"background-dispatch","group":"engine-loop","interface_version":2,"implementation_version":"0.2.0","operation":"background.plan","modes":["live","linked"],"runtime_requirements":[],"build":{"package":"webagent-background-service","source_inputs":["crates/background-service","crates/background-dispatch","crates/background-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/webagent-background-service","limits":{"max_request_bytes":65536,"max_response_bytes":65536,"timeout_ms":1000,"max_concurrency":1}}))?)]),withdrawn:Default::default()};
  for operation in ["assets.plan","capability.prepare","provider.project"] {
   let(id,package,inputs)=context_registration(operation).unwrap();
   let descriptor=serde_json::from_value(json!({"schema_version":1,"id":id,"group":"engine-loop","interface_version":1,"implementation_version":"0.1.0","operation":operation,"modes":["live","linked"],"runtime_requirements":[],"build":{"package":package,"source_inputs":inputs,"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":format!("artifact/{package}"),"limits":{"max_request_bytes":16777216,"max_response_bytes":16777216,"timeout_ms":30000,"max_concurrency":1}}))?;
   embedded.entries.insert(id.into(),descriptor);
  }
  return Ok(Arc::new(embedded));
 };
 static CACHE:OnceLock<StdMutex<(Option<PathBuf>,Option<Arc<Catalog>>) >>=OnceLock::new();
 let mut cache=CACHE.get_or_init(||StdMutex::new((None,None))).lock().unwrap_or_else(|e|e.into_inner());
 if cache.0.as_ref()==Some(&root) {return cache.1.clone().ok_or_else(||anyhow!("Engine catalog unavailable"));}
 let revision=root.parent().and_then(|p|p.file_name()).and_then(|n|n.to_str()).ok_or_else(||anyhow!("Engine revision unavailable"))?;
 match load(&root,revision) {
  Ok(next)=>{let next=Arc::new(next);cache.0=Some(root);cache.1=Some(next.clone());Ok(next)},
  Err(_)=>{tracing::error!("Engine catalog candidate rejected");cache.1.clone().ok_or_else(||anyhow!("Engine catalog unavailable"))}
 }
}
/// Runtime dependency graphs are independent of process hierarchy and compilation dependencies.
fn validate_graph(entries:&BTreeMap<String,Descriptor>)->Result<()> {validate_graph_with_withdrawals(entries,&Default::default())}
/// Known withdrawals are unavailable requirements; typos remain invalid installation candidates.
fn validate_graph_with_withdrawals(entries:&BTreeMap<String,Descriptor>,withdrawn:&std::collections::BTreeSet<String>)->Result<()> {
 // Traverse declared runtime requirements and reject cycles or unknown dependencies.
 fn visit(id:&str,entries:&BTreeMap<String,Descriptor>,active:&mut std::collections::BTreeSet<String>,done:&mut std::collections::BTreeSet<String>,withdrawn:&std::collections::BTreeSet<String>)->Result<()> {
  if withdrawn.contains(id){return Ok(());}
  if done.contains(id){return Ok(());}
  let component=entries.get(id).ok_or_else(||anyhow!("Missing engine requirement"))?;
  if !active.insert(id.to_owned()){anyhow::bail!("Cyclic engine requirements");}
  let mut unique=std::collections::BTreeSet::new();
  for dependency in &component.runtime_requirements {
   if !unique.insert(dependency){anyhow::bail!("Duplicate engine requirement");}
   visit(dependency,entries,active,done,withdrawn)?;
  }
  active.remove(id);done.insert(id.to_owned());Ok(())
 }
 let mut done=std::collections::BTreeSet::new();
 for id in entries.keys(){visit(id,entries,&mut std::collections::BTreeSet::new(),&mut done,withdrawn)?;}Ok(())
}
/// Resolve the required dependency closure; group labels never imply another process.
fn requirement_available(c:&Catalog,controls:&Controls,id:&str)->bool {
 let Some(descriptor)=c.entries.get(id)else{return false;};
 !controls.disabled.iter().any(|v|v==id)&&descriptor.runtime_requirements.iter().all(|dependency|requirement_available(c,controls,dependency))
}
#[derive(Default,Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Controls {pub revision:u64,pub disabled:Vec<String>}
/// Source defaults seed only absent runtime controls; saved disable tombstones survive releases.
pub fn initialize(root:&std::path::Path)->Result<()> {
 let path=root.join("config/engine-components.json");
 if path.exists(){controls(root)?;return Ok(());}
 let source=webagent_configuration::prompts::default_source(None)?;
 let defaults_path=source.map(|source|source.join("config/engine-components.json")).unwrap_or_else(||crate::defaults::fallback_directory().join("engine-components.json"));
 let defaults=match std::fs::read(defaults_path) {
  Ok(bytes)=>{if bytes.len()>32768{anyhow::bail!("Engine defaults bound");}serde_json::from_slice::<Controls>(&bytes)?},
  Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Controls::default(),Err(e)=>return Err(e.into())
 };
 if defaults.revision!=0||defaults.disabled.len()>256||defaults.disabled.iter().any(|id|!valid_id(id)){anyhow::bail!("Invalid engine control defaults");}
 let dir=root.join("config");std::fs::create_dir_all(&dir)?;
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(&defaults)?)?;file.as_file().sync_all()?;
 match file.persist_noclobber(path){Ok(_)=>{},Err(e)if e.error.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.error.into())};Ok(())
}
/// Runtime controls are distinct from immutable packages and contain no credentials.
pub fn controls(root:&std::path::Path)->Result<Controls>{
 let path=root.join("config/engine-components.json");
 match std::fs::read(path){Ok(bytes)=>{if bytes.len()>32768{anyhow::bail!("Engine controls too large");}let value:Controls=serde_json::from_slice(&bytes)?;if value.disabled.len()>256||value.disabled.iter().any(|s|!valid_id(s)){anyhow::bail!("Unknown engine control");}Ok(value)},
 Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(Controls::default()),Err(e)=>Err(e.into())}
}
/// Admission and dispatch both read current controls; there is no built-in fallback on removal.
static BACKGROUND_FAILURE_UNTIL:OnceLock<StdMutex<Option<std::time::Instant>>>=OnceLock::new();
static FAILURE_UNTIL:OnceLock<StdMutex<Option<std::time::Instant>>>=OnceLock::new();
/// New pure-engine failures are isolated by state root and registered component.
/// Expired entries are pruned and at most 256 live roots/components are retained.
static PURE_FAILURE_UNTIL:OnceLock<StdMutex<BTreeMap<(PathBuf,String),std::time::Instant>>>=OnceLock::new();
/// Keep only the reviewed execution IDs eligible for host-retained run pauses.
pub fn working_component(id:&str)->bool {WORKING_COMPONENTS.contains(&id)}
/// A malformed reply or failed IPC must not immediately restart the same work.
fn record_pure_failure(root:&std::path::Path,id:&str) {
 if !matches!(id,ASSETS|CAPABILITY|PROJECTION) {return;}
 let now=std::time::Instant::now();
 let mut failures=PURE_FAILURE_UNTIL.get_or_init(||StdMutex::new(BTreeMap::new())).lock().unwrap_or_else(|e|e.into_inner());
 failures.retain(|_,deadline|*deadline>now);
 if failures.len()>=256 {
  if let Some(key)=failures.iter().min_by_key(|(_,deadline)|**deadline).map(|(key,_)|key.clone()) {failures.remove(&key);}
 }
 failures.insert((root.to_owned(),id.into()),now+Duration::from_secs(5));
}
/// Read state-root pure-engine cooldowns alongside process-wide context/background fences.
fn component_cooling(root:&std::path::Path,id:&str)->bool {
 let now=std::time::Instant::now();
 if id==CONTEXT&&FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner()).is_some_and(|deadline|deadline>now){return true;}
 if id==BACKGROUND&&BACKGROUND_FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner()).is_some_and(|deadline|deadline>now){return true;}
 let mut failures=PURE_FAILURE_UNTIL.get_or_init(||StdMutex::new(BTreeMap::new())).lock().unwrap_or_else(|e|e.into_inner());
 failures.retain(|_,deadline|*deadline>now);
 failures.contains_key(&(root.to_owned(),id.into()))
}
/// Include transient failures throughout the validated runtime dependency closure.
fn runtime_available(c:&Catalog,controls:&Controls,root:&std::path::Path,id:&str)->bool {
 let Some(descriptor)=c.entries.get(id) else {return false;};
 !component_cooling(root,id) && !controls.disabled.iter().any(|disabled|disabled==id)
  && descriptor.runtime_requirements.iter().all(|dependency|runtime_available(c,controls,root,dependency))
}
/// Require every model-loop component, with no fallback on removal or cooldown.
pub fn available(root:&std::path::Path)->bool {
 WORKING_COMPONENTS.iter().all(|id|component_available(root,id))
}
/// Resolve one installed component, its live controls and transient dependency failures.
pub fn component_available(root:&std::path::Path,id:&str)->bool {
 match (controls(root),catalog()) {(Ok(controls),Ok(catalog))=>runtime_available(&catalog,&controls,root,id),_=>false}
}
/// Pure preparation is valid only while its pinned catalog and live controls survive.
fn require_component_snapshot(root:&std::path::Path,id:&str,revision:&str)->Result<()> {
 if catalog()?.revision!=revision || !component_available(root,id) {anyhow::bail!("Engine component changed during preparation");}
 Ok(())
}
/// Persist only validated administrator controls through an atomic owning-host write.
pub fn save_controls(root:&std::path::Path,current:&Controls)->Result<()> {
 let dir=root.join("config");std::fs::create_dir_all(&dir)?;
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(current)?)?;file.as_file().sync_all()?;
 file.persist(dir.join("engine-components.json")).map_err(anyhow::Error::from)?;Ok(())
}
/// Save a host-marked pause while preserving accepted input and run revision fencing.

/// Recognize safe host-retained pauses; manual controls and uncertain effects stay fenced.
pub fn retained_pause(run:&Value)->bool {
 status(run)=="paused" && run["component_pause"]["version"]==1
 && matches!(run["component_pause"]["component"].as_str(),Some(CONTEXT|BACKGROUND|ASSETS|CAPABILITY|PROJECTION))
 && run["uncertain"]!=true && run["pending"]["state"]!="executing"
 && run["pending"]["external_job"].is_null()
 && !run["batch"].as_array().into_iter().flatten().any(|item|item["state"]=="executing"
   || item["pending"]["state"]=="executing" || !item["pending"]["external_job"].is_null())
}
/// Resume only host-marked component pauses, with current participation and model authority.

/// Adoption attests the catalog held by the consumer, not an arbitrary filesystem pointer.
pub fn adoption(root:&std::path::Path,role:&str)->Result<()>{
 let c=catalog()?;
 if let Some(services)=SERVICES.get(){let mut services=services.lock().unwrap_or_else(|e|e.into_inner());services.retain(|hash,service|(controls(root).is_ok_and(|controls|c.entries.values().any(|d|requirement_available(&c,&controls,&d.id)&&serde_json::to_value(d).is_ok_and(|v|crate::digest(&v)==*hash))))||Arc::strong_count(service)>1);}
 let dir=root.join("publication");std::fs::create_dir_all(&dir)?;
 let value=json!({"revision":c.revision,"components":c.entries.keys().collect::<Vec<_>>(),"mode":if cfg!(feature="live-context"){"live"}else{"linked"}});
 let path=dir.join(format!("engine-{role}.json"));if std::fs::read(&path).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok())==Some(value.clone()){return Ok(());}
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(&value)?)?;file.as_file().sync_all()?;file.persist(path).map_err(anyhow::Error::from)?;Ok(())
}

/// Report execution identity independently of installed live package metadata.
pub fn execution_identity(descriptor:&Descriptor)->(String,String) {
 #[cfg(feature="live-context")]
 { (descriptor.implementation_version.clone(),descriptor.artifact_sha256.clone().unwrap_or_default()) }
 #[cfg(feature="linked-context")]
 { let _=descriptor;static GENERATION:OnceLock<String>=OnceLock::new();
   let generation=GENERATION.get_or_init(||std::env::current_exe().ok().and_then(|path|std::fs::read(path).ok()).map(|bytes|hex::encode(sha2::Sha256::digest(bytes))).unwrap_or_else(||"linked-build-unavailable".into()));
   (if descriptor.id==BACKGROUND {webagent_background_dispatch::IMPLEMENTATION_VERSION}else if descriptor.id=="boot-supervisor"{webagent_boot_supervisor::IMPLEMENTATION_VERSION}else if descriptor.id==ASSETS {webagent_context_assets::IMPLEMENTATION_VERSION}else if descriptor.id==CAPABILITY {webagent_capability_context::IMPLEMENTATION_VERSION}else if descriptor.id==PROJECTION {webagent_provider_projection::IMPLEMENTATION_VERSION}else{webagent_context_assembly::IMPLEMENTATION_VERSION}.into(),generation.clone()) }
}
/// Compare public metadata cases in both optimized modes, requiring live reuse and drain.
async fn background_smoke()->Result<Vec<Value>> {
 use webagent_background_contracts::{Request,Task,State,Plan};
 #[cfg(feature="live-context")]
 let process=Service{connection:tokio::sync::Mutex::new(None),admission:Arc::new(tokio::sync::Semaphore::new(1)),
  executable:StdMutex::new(std::env::current_exe()?.with_file_name("webagent-background-service")),
  limits:Limits{max_request_bytes:65536,max_response_bytes:65536,timeout_ms:1000,max_concurrency:1}};
 #[cfg(feature="live-context")]let mut pid=None;
 let mut cases=Vec::new();
 for time in [100,101] {
  let input=Request{version:2,now:time,slots:2,rescan_interval_ms:15000,tasks:vec![Task{state:State::Queued,active:false,lease_until:0},Task{state:State::Running,active:false,lease_until:101},Task{state:State::Completed,active:false,lease_until:0}]};
  #[cfg(feature="linked-context")]let plan=webagent_background_dispatch::plan(&input)?;
  #[cfg(feature="live-context")]let plan:Plan={
   let reply=process.call("offline-background-smoke",&input,&CancellationToken::new()).await?;
   if reply["status"]!="success"{anyhow::bail!("Background smoke response");}
   let current=process.connection.lock().await.as_ref().and_then(|value|value.0.id()).ok_or_else(||anyhow!("Background smoke exited"))?;
   if pid.is_some_and(|previous|previous!=current){anyhow::bail!("Background smoke reuse");}pid=Some(current);
   serde_json::from_value(reply["plan"].clone())?
  };
  webagent_background_contracts::validate_plan(&input,&plan)?;
  let expected=Plan{version:2,dispatch:if time==100{vec![0]}else{vec![0,1]},followup:vec![2],retry_ms:if time==100{1000}else{15000}};
  if plan!=expected{anyhow::bail!("Background smoke policy mismatch");}
  cases.push(serde_json::to_value(plan)?);
 }
 #[cfg(feature="live-context")] {
  process.release("offline-background-smoke").await?;
  if let Some((mut child,writer,reader))=process.connection.lock().await.take(){drop(writer);drop(reader);let outcome=tokio::time::timeout(Duration::from_secs(1),child.wait()).await.map_err(|_|anyhow!("Background smoke drain"))??;if !outcome.success(){anyhow::bail!("Background smoke exit");}}
 }
 Ok(cases)
}
/// Assert idle/recovery metadata in both optimized modes and live process reuse/drain.
async fn boot_smoke()->Result<Vec<Value>> {
 use webagent_boot_contracts::{Request,Service as Input,State,Plan,Action,Operation};
 #[cfg(feature="live-context")]
 let process=Service{connection:tokio::sync::Mutex::new(None),admission:Arc::new(tokio::sync::Semaphore::new(1)),
 executable:StdMutex::new(std::env::current_exe()?.with_file_name("webagent-boot-service")),
 limits:Limits{max_request_bytes:65536,max_response_bytes:65536,timeout_ms:1000,max_concurrency:1}};
 #[cfg(feature="live-context")]let mut pid=None;
 let mut result=Vec::new();
 for now in [10,30] {
  let input=Request{version:1,now_ms:now,services:vec![
   Input{state:State::Running,unhealthy:false,eligible:true,dependency_ready:true,safe_after_ms:0},
   Input{state:State::Stopped,unhealthy:false,eligible:true,dependency_ready:true,safe_after_ms:30},
   Input{state:State::Running,unhealthy:true,eligible:false,dependency_ready:true,safe_after_ms:0}]};
  #[cfg(feature="linked-context")]let plan=webagent_boot_supervisor::plan(&input)?;
  #[cfg(feature="live-context")]let plan:Plan={
   let reply=process.call("offline-boot-smoke",&input,&CancellationToken::new()).await?;
   if reply["status"]!="success"{anyhow::bail!("Boot smoke response");}
   let current=process.connection.lock().await.as_ref().and_then(|v|v.0.id()).ok_or_else(||anyhow!("Boot smoke exited"))?;
   if pid.is_some_and(|previous|previous!=current){anyhow::bail!("Boot service reuse");}pid=Some(current);
   serde_json::from_value(reply["plan"].clone())?
  };
  webagent_boot_contracts::validate_plan(&input,&plan)?;
  let expected=Plan{version:1,actions:if now==30{vec![Action{index:1,operation:Operation::Start}]}else{vec![]},next_deadline_ms:if now==10{Some(30)}else{None}};
  if plan!=expected{anyhow::bail!("Boot smoke policy");}
  result.push(serde_json::to_value(plan)?);
 }
 #[cfg(feature="live-context")] {
  process.release("offline-boot-smoke").await?;
  if let Some((mut child,writer,reader))=process.connection.lock().await.take(){
   drop(writer);drop(reader);
   if !tokio::time::timeout(Duration::from_secs(1),child.wait()).await.map_err(|_|anyhow!("Boot drain"))??.success(){anyhow::bail!("Boot drain exit");}
  }
 }
 Ok(result)
}
/// Construct the actual sibling service built for the offline optimized proof.
#[cfg(feature="live-context")]
fn smoke_service(package:&str)->Result<Service> {
 let executable=std::env::current_exe()?.with_file_name(package);
 let info=std::fs::symlink_metadata(&executable)?;
 if !info.is_file()||info.file_type().is_symlink(){anyhow::bail!("Offline service artifact missing");}
 Ok(Service{connection:tokio::sync::Mutex::new(None),admission:Arc::new(tokio::sync::Semaphore::new(1)),
  executable:StdMutex::new(executable),limits:Limits{max_request_bytes:16777216,max_response_bytes:16777216,timeout_ms:30000,max_concurrency:1}})
}
/// Read proof identity from captured descriptors without linking replaceable implementations.
#[cfg(any(feature="live-context",test))]
fn smoke_implementation(root:&std::path::Path,id:&str)->Result<String> {
 if !matches!(id,CONTEXT|ASSETS|CAPABILITY|PROJECTION){anyhow::bail!("Offline component identity");}
 let path=root.join("plugins/engine").join(id).join("component.json");
 let info=std::fs::symlink_metadata(&path)?;
 if !info.is_file()||info.file_type().is_symlink()||info.len()>65536{anyhow::bail!("Offline descriptor identity");}
 let descriptor:Descriptor=serde_json::from_slice(&std::fs::read(path)?)?;
 descriptor.validate()?;
 if descriptor.id!=id{anyhow::bail!("Offline descriptor component");}
 Ok(descriptor.implementation_version)
}
/// Validate two calls on the same process rather than accepting a respawn as reuse.
#[cfg(feature="live-context")]
async fn smoke_reply<T:serde::Serialize,R:serde::de::DeserializeOwned>(process:&Service,pid:&mut Option<u32>,key:&str,input:&T,
 field:&str,implementation:&str,validate:impl Fn(&T,&R)->Result<()>)->Result<R> {
 let response=process.call(key,input,&CancellationToken::new()).await?;
 let result=checked_pure_response(input,&response,field,implementation,validate)?;
 let current=process.connection.lock().await.as_ref().and_then(|value|value.0.id()).ok_or_else(||anyhow!("Offline service exited"))?;
 if pid.is_some_and(|previous|previous!=current){anyhow::bail!("Offline service reuse");}*pid=Some(current);
 Ok(result)
}
/// Release and close inherited pipes, requiring successful bounded child shutdown.
#[cfg(feature="live-context")]
async fn smoke_drain(process:&Service,key:&str)->Result<()> {
 process.release(key).await?;
 if let Some((mut child,writer,reader))=process.connection.lock().await.take() {
  drop(writer);drop(reader);
  if !tokio::time::timeout(Duration::from_secs(1),child.wait()).await.map_err(|_|anyhow!("Offline service drain"))??.success() {
   anyhow::bail!("Offline service drain exit");
  }
 }
 Ok(())
}
/// Prove permanent append/prefix boundaries and temporary suffixes with metadata only.
async fn assets_smoke()->Result<Vec<Value>> {
 use webagent_assets_contracts::{Request,Plan,State,Placement};
 #[cfg(feature="live-context")]let process=smoke_service("webagent-assets-service")?;
 #[cfg(feature="live-context")]let mut pid=None;
 let mut cases=Vec::new();
 for append in [true,false] {
  let history=vec![json!({"role":"user","content":"public inspection"}),
   json!({"type":"function_call","call_id":"smoke-call","name":"file_read","arguments":"{}"}),
   json!({"type":"function_call_output","call_id":"smoke-call","output":"public result"})];
  let state=State{history_basis:"smoke-epoch".into(),placements:vec![Placement{id:"main".into(),after_input:0,anchor:None}],
   schema_order:vec!["file_write".into()],..Default::default()};
  let input=Request::Place{version:1,state,history,definitions:vec!["main".into(),"verification".into()],
   temporary:vec!["current-mode".into()],schema_names:vec!["file_read".into(),"file_write".into()],history_basis:"smoke-epoch".into(),append};
  #[cfg(feature="linked-context")]let plan=webagent_context_assets::plan(&input)?;
  #[cfg(feature="live-context")]let plan:Plan=smoke_reply(&process,&mut pid,"offline-assets-smoke",&input,"plan",&smoke_implementation(std::path::Path::new("."),ASSETS)?,webagent_assets_contracts::validate_plan).await?;
  webagent_assets_contracts::validate_plan(&input,&plan)?;
  let at=if append{3}else{0};
  let expected=Plan::Place{placements:vec![Placement{id:"main".into(),after_input:0,anchor:None},
   Placement{id:"verification".into(),after_input:at,anchor:if append{Some("call:smoke-call".into())}else{None}},
   Placement{id:"current-mode".into(),after_input:3,anchor:Some("call:smoke-call".into())}],
   schema_order:vec!["file_write".into(),"file_read".into()],rebased:false,input_digest:webagent_assets_contracts::digest(&input)?};
  if plan!=expected{anyhow::bail!("Assets smoke placement mismatch");}
  cases.push(serde_json::to_value(plan)?);
 }
 #[cfg(feature="live-context")]smoke_drain(&process,"offline-assets-smoke").await?;
 Ok(cases)
}
/// Prove fresh prompts/selected text and exact authorized schema order across calls.
async fn capability_smoke()->Result<Vec<Value>> {
 use webagent_capability_contracts::{PrepareRequest,PreparedCapability,Guidance,GuidanceKind,Definition,Inclusion,InclusionReason};
 #[cfg(feature="live-context")]let process=smoke_service("webagent-capability-service")?;
 #[cfg(feature="live-context")]let mut pid=None;
 let mut cases=Vec::new();
 for (index,label) in ["first","current"].into_iter().enumerate() {
  let input=PrepareRequest{version:1,source_fingerprint:if index==0{"a".repeat(64)}else{"b".repeat(64)},
   base_prompt:format!("{label} live main é\n"),live_slots:format!("{label} live slot\n"),
   guidance:vec![Guidance{id:"ability:base".into(),kind:GuidanceKind::Ability,body:format!("{label} base guidance"),selected:false,natural:true},
    Guidance{id:"selectable:codebase_admin:verification".into(),kind:GuidanceKind::Selectable,
     body:format!("{label} selected guidance"),selected:true,natural:false}],
   permitted_schemas:vec!["file_read".into(),"file_write".into()],prior_schema_order:vec!["file_write".into()],menu:format!("{label} temporary menu")};
  #[cfg(feature="linked-context")]let result=webagent_capability_context::prepare(&input)?;
  #[cfg(feature="live-context")]let result:PreparedCapability=smoke_reply(&process,&mut pid,"offline-capability-smoke",&input,"capability",&smoke_implementation(std::path::Path::new("."),CAPABILITY)?,webagent_capability_contracts::validate_result).await?;
  webagent_capability_contracts::validate_result(&input,&result)?;
  let expected=PreparedCapability{version:1,source_fingerprint:input.source_fingerprint.clone(),main:format!("{}{}",input.base_prompt,input.live_slots),
   definitions:input.guidance.iter().map(|item|Definition{id:item.id.clone(),kind:item.kind,body:item.body.clone()}).collect(),
   schema_order:vec!["file_write".into(),"file_read".into()],temporary_menu:input.menu.clone(),
   inclusion:input.guidance.iter().map(|item|Inclusion{id:item.id.clone(),kind:item.kind,included:true,
    reason:if item.natural{InclusionReason::Natural}else{InclusionReason::Selected},content_bytes:item.body.len()}).collect()};
  if result!=expected{anyhow::bail!("Capability smoke source mismatch");}
  cases.push(serde_json::to_value(result)?);
 }
 #[cfg(feature="live-context")]smoke_drain(&process,"offline-capability-smoke").await?;
 Ok(cases)
}
/// Prove subscription and DeepSeek reference plans without endpoints or credentials.
async fn provider_smoke()->Result<Vec<Value>> {
 use webagent_provider_contracts::{RequestV1,ResultV1,ProviderIdV1,ProjectionV1,SourceGroupV1,TransportV1};
 #[cfg(feature="live-context")]let process=smoke_service("webagent-provider-service")?;
 #[cfg(feature="live-context")]let mut pid=None;
 let mut cases=Vec::new();
 for provider in [ProviderIdV1::Deepseek,ProviderIdV1::OpenaiSubscription] {
  let input=RequestV1::Project{version:1,provider,model:"public-smoke-model".into(),instructions:"current live main".into(),
   input:vec![json!({"role":"user","content":"inspect public fixture"}),
    json!({"type":"reasoning","id":"rs-smoke","encrypted_content":"public-opaque-fixture","summary":[]}),
    json!({"role":"assistant","content":"checking"}),
    json!({"type":"function_call","call_id":"smoke-call","name":"file_read","arguments":"{}","deepseek_reasoning":"public fixture reasoning"}),
    json!({"type":"function_call_output","call_id":"smoke-call","output":"public fixture result"}),
    json!({"role":"developer","content":"current live guidance"})],tools:vec![json!({"name":"z_read"}),json!({"name":"a_read"})]};
  #[cfg(feature="linked-context")]let result=webagent_provider_projection::project(&input)?;
  #[cfg(feature="live-context")]let result:ResultV1=smoke_reply(&process,&mut pid,"offline-provider-smoke",&input,"projection",&smoke_implementation(std::path::Path::new("."),PROJECTION)?,webagent_provider_contracts::validate_result).await?;
  webagent_provider_contracts::validate_result(&input,&result)?;
  let subscription=provider==ProviderIdV1::OpenaiSubscription;
  let expected=ResultV1::Projected{version:1,projection:ProjectionV1{provider,
   transport:if subscription{TransportV1::Responses}else{TransportV1::ChatCompletions},
   groups:if subscription{(0..6).map(|source_index|SourceGroupV1::ResponseItem{source_index}).collect()}
    else{vec![SourceGroupV1::Message{source_index:0},SourceGroupV1::Assistant{message:Some(2),calls:vec![3]},
     SourceGroupV1::ToolResult{source_index:4},SourceGroupV1::Message{source_index:5}]},
   omitted:if subscription{vec![]}else{vec![1]},tools:vec![0,1]}};
  if result!=expected{anyhow::bail!("Provider smoke references mismatch");}
  // Materialization remains a host-contract operation over its exact original bytes.
  webagent_provider_contracts::materialize(&input,&result)?;
  cases.push(serde_json::to_value(result)?);
 }
 #[cfg(feature="live-context")]smoke_drain(&process,"offline-provider-smoke").await?;
 Ok(cases)
}
/// Exercise only public deterministic inputs offline; this CLI never opens application state or credentials.
pub async fn context_smoke()->Result<Value> {
 use webagent_context_contracts::protocol::{PrepareRequest,PreparedResult};
 let started=std::time::Instant::now();let config=context_compaction::Config::configured()?;
 let selection=crate::provider::ProviderSelection::parse(&json!({"provider":"deepseek","model":"smoke-model"}),"unused")?;
 let schemas=vec![json!({"name":"load_ability","parameters":{"type":"object","properties":{}}})];
 let contributions=vec![context_input::Contribution{id:"mode".into(),source:"smoke".into(),lifetime:context_input::Lifetime::Call,content:"overlay".into(),turn:1,call:2}];
 #[cfg(feature="live-context")]
 let process={
  let executable=std::env::current_exe()?.with_file_name("webagent-context-service");
  let info=std::fs::symlink_metadata(&executable)?;
  if !info.is_file()||info.file_type().is_symlink(){anyhow::bail!("Offline service artifact missing");}
  Service{connection:tokio::sync::Mutex::new(None),admission:Arc::new(tokio::sync::Semaphore::new(1)),executable:StdMutex::new(executable),limits:Limits{max_request_bytes:16777216,max_response_bytes:16777216,timeout_ms:30000,max_concurrency:1}}
 };
 #[cfg(feature="live-context")]let mut pid=None;
 let mut cases=Vec::new();
 for text in ["public transcript é🌍","replacement public transcript"] {
  let history=vec![json!({"role":"user","content":text})];
  let request=context_input::Request{history:&history,base:"smoke base é🌍",guidance:" guidance",schemas:&schemas,contributions:&contributions,identity:context_input::CallIdentity{turn:1,call:2},selection:&selection,max_bytes:65536,compaction:&config};
  let owned=PrepareRequest{history:history.clone(),base:request.base.into(),guidance:request.guidance.into(),schemas:schemas.clone(),contributions:contributions.clone(),identity:request.identity,model:context_assembly::model_metadata(&selection),max_bytes:request.max_bytes,compaction:config.0.clone(),projection_extra_bytes:0,contributions_projected:false,system_input_budget:None,summary_instructions:config.summary_instructions.clone()};
  #[cfg(feature="linked-context")]
  let prepared={
   let direct=context_assembly::prepare_incremental(request,&mut Default::default())?;
   let payload=match direct{context_assembly::Prepared::Ready(envelope)=>PreparedResult::Ready{instructions:envelope.instructions,report:envelope.report},context_assembly::Prepared::Compact(plan)=>PreparedResult::Compact{plan}};
   let decoded:PrepareRequest=serde_json::from_slice(&serde_json::to_vec(&owned)?)?;
   let transported=webagent_context_assembly::protocol::prepare_owned(&decoded,&mut Default::default())?;
   if serde_json::to_value(&payload)?!=serde_json::to_value(transported)?{anyhow::bail!("Linked adapter smoke mismatch");}payload
  };
  #[cfg(feature="live-context")]
  let prepared={
   let reply=process.call("offline-context-smoke",&owned,&CancellationToken::new()).await?;
   let current=process.connection.lock().await.as_ref().and_then(|value|value.0.id()).ok_or_else(||anyhow!("Offline service exited"))?;
   if pid.is_some_and(|previous|previous!=current){anyhow::bail!("Offline service was not reused");}pid=Some(current);
   let direct=validated(request,&mut Default::default(),reply)?;
   match direct{context_assembly::Prepared::Ready(envelope)=>PreparedResult::Ready{instructions:envelope.instructions,report:envelope.report},context_assembly::Prepared::Compact(plan)=>PreparedResult::Compact{plan}}
  };
  if !matches!(prepared,PreparedResult::Ready{..}){anyhow::bail!("Offline smoke unexpectedly compacted");}
  cases.push(serde_json::to_value(prepared)?);
 }
 use webagent_context_contracts::tool_results as duplicate;
 let body="public identical output ".repeat(100);
 let observations=["first","second","third"].into_iter().map(|id|duplicate::Observation{
  call_id:id.into(),tool:"file_read".into(),inputs:json!({"path":"public.rs"}),scope:json!({"session":"offline"}),
  output:body.clone(),success:true,included:true}).collect();
 let duplicate_request=duplicate::Request{version:1,observations,state:Default::default(),enabled:true,
  template:Some("Executed {current_call_id}; complete content equals {reference_call_id} ({tool}).".into()),rebase:false};
 #[cfg(feature="linked-context")]let duplicate_plan=webagent_context_assembly::tool_results::project(&duplicate_request)?;
 #[cfg(feature="live-context")]let duplicate_plan:duplicate::Plan={
  let response=process.call("offline-context-smoke",&json!({"tool_result_projection":duplicate_request}),&CancellationToken::new()).await?;
  checked_pure_response(&duplicate_request,&response,"tool_result_projection",&smoke_implementation(std::path::Path::new("."),CONTEXT)?,duplicate::validate_plan)?
 };
 duplicate::validate_plan(&duplicate_request,&duplicate_plan)?;
 if duplicate_plan.replacements.len()!=2||duplicate_plan.state.assignments["third"].primary!="first"{anyhow::bail!("Duplicate projection smoke mismatch");}
 // Keep the two-case release proof envelope; compare the new operation inside its first case.
 cases[0]["tool_result_projection"]=serde_json::to_value(duplicate_plan)?;
 #[cfg(feature="live-context")]
 {
  process.release("offline-context-smoke").await?;
  if let Some((mut child,writer,reader))=process.connection.lock().await.take(){drop(writer);drop(reader);tokio::time::timeout(Duration::from_secs(1),child.wait()).await.map_err(|_|anyhow!("Offline service drain timeout"))??;}
 }
 let mut result=json!({"mode":if cfg!(feature="live-context"){"live"}else{"linked"},"equivalence":true,"prepared":cases,"background_prepared":background_smoke().await?,"boot_prepared":boot_smoke().await?,"assets_prepared":assets_smoke().await?,"capability_prepared":capability_smoke().await?,"provider_prepared":provider_smoke().await?,"elapsed_us":started.elapsed().as_micros(),"interface_version":2});
 #[cfg(feature="linked-context")] {result["implementation_version"]=json!(webagent_context_assembly::IMPLEMENTATION_VERSION);result["service_started"]=json!(false);}
 #[cfg(feature="live-context")] {result["service_reused"]=json!(true);result["service_drained"]=json!(true);}
 Ok(result)
}
/// A generation owns one isolated reusable service and a bounded request gate.
struct Service {
 connection:tokio::sync::Mutex<Option<(tokio::process::Child,tokio::process::ChildStdin,tokio::io::BufReader<tokio::process::ChildStdout>)>>,
 admission:Arc<tokio::sync::Semaphore>,executable:StdMutex<PathBuf>,limits:Limits,
}
type Services=BTreeMap<String,Arc<Service>>;
static SERVICES:OnceLock<StdMutex<Services>>=OnceLock::new();
/// Retain admitted generation references; retire unused old processes on catalog changes.
fn service(c:&Catalog,d:&Descriptor)->Result<Arc<Service>>{
 let key=crate::digest(&serde_json::to_value(d)?);
 let mut map=SERVICES.get_or_init(||StdMutex::new(BTreeMap::new())).lock().unwrap_or_else(|e|e.into_inner());
 map.retain(|hash,value|hash==&key||Arc::strong_count(value)>1||c.entries.values().any(|entry|serde_json::to_value(entry).is_ok_and(|v|crate::digest(&v)==*hash)));
 let executable=if let Some(root)=&c.root {root.join("plugins/engine").join(&d.id).join(&d.executable)}
 else{PathBuf::from("/usr/local/lib/webagent/engine").join(&d.id).join(&d.build.package)};
 if let Some(value)=map.get(&key){*value.executable.lock().unwrap_or_else(|e|e.into_inner())=executable;return Ok(value.clone());}
 let value=Arc::new(Service{connection:tokio::sync::Mutex::new(None),admission:Arc::new(tokio::sync::Semaphore::new(16)),executable:StdMutex::new(executable),limits:d.limits.clone()});
 map.insert(key,value.clone());Ok(value)
}
impl Service {
 /// Release accounting without launching an idle service. Late user cancellation never revives it.
 async fn release(&self,run_key:&str)->Result<()> {
  use tokio::io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt};
  let deadline=tokio::time::Instant::now()+Duration::from_secs(1);
  let mut guard=tokio::time::timeout_at(deadline,self.connection.lock()).await.map_err(|_|anyhow!("Engine cleanup queue timeout"))?;
  let Some((_,writer,reader))=guard.as_mut()else{return Ok(());};
  let id=uuid();let payload=serde_json::to_vec(&json!({"version":1,"id":id,"run_key":run_key,"operation":"release"}))?;
  let action=async {
   writer.write_all(&payload).await?;writer.write_all(b"\n").await?;writer.flush().await?;
   let mut bytes=Vec::new();reader.take(1025).read_until(b'\n',&mut bytes).await?;
   if bytes.len()>1024||bytes.last()!=Some(&b'\n'){anyhow::bail!("Engine cleanup response limit");}
   let response:Value=serde_json::from_slice(&bytes)?;
   if response["version"]!=1||response["id"]!=id||response["result"]["status"]!="released"{anyhow::bail!("Engine cleanup identity");}
   Ok::<_,anyhow::Error>(())
  };
  let result=tokio::time::timeout_at(deadline,action).await.unwrap_or_else(|_|Err(anyhow!("Engine cleanup timeout")));
  if result.is_err(){*guard=None;}result
 }
 /// Private inherited pipes authenticate the peer; requests cannot select sockets or executables.
 async fn call<T:serde::Serialize>(&self,run_key:&str,input:&T,cancel:&tokio_util::sync::CancellationToken)->Result<Value>{
  use tokio::io::{AsyncBufReadExt,AsyncReadExt,AsyncWriteExt};
  let deadline=tokio::time::Instant::now()+Duration::from_millis(self.limits.timeout_ms);
  let _permit=self.admission.clone().try_acquire_owned().map_err(|_|anyhow!("Engine service saturated"))?;
  let id=uuid();let payload=serde_json::to_vec(&json!({"version":1,"id":id,"run_key":run_key,"input":input}))?;
  if payload.len()+1>self.limits.max_request_bytes{anyhow::bail!("Engine request limit");}
  let mut connection=tokio::select!{_=cancel.cancelled()=>return Err(anyhow!("Engine call cancelled")),guard=tokio::time::timeout_at(deadline,self.connection.lock())=>guard.map_err(|_|anyhow!("Engine queue timeout"))?};
  let action=async {
   if connection.is_none(){
    let executable=self.executable.lock().unwrap_or_else(|e|e.into_inner()).clone();
    let sandbox=crate::sandbox::prepare(&executable)?;
    let mut command=tokio::process::Command::new(&executable);
    unsafe {command.pre_exec(move || sandbox.apply());}
    let mut child=command.env_clear().current_dir("/")
     .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn()?;
    let input=child.stdin.take().ok_or_else(||anyhow!("Engine pipe unavailable"))?;
    let output=child.stdout.take().ok_or_else(||anyhow!("Engine pipe unavailable"))?;
    *connection=Some((child,input,tokio::io::BufReader::new(output)));
   }
   let (_,writer,reader)=connection.as_mut().unwrap();
   writer.write_all(&payload).await?;writer.write_all(b"\n").await?;writer.flush().await?;
   let mut bytes=Vec::new();reader.take((self.limits.max_response_bytes+1)as u64).read_until(b'\n',&mut bytes).await?;
   if bytes.len()>self.limits.max_response_bytes||bytes.last()!=Some(&b'\n'){anyhow::bail!("Engine response limit");}
   let response:Value=serde_json::from_slice(&bytes)?;
   if response["version"]!=1||response["id"]!=id{anyhow::bail!("Engine response identity");}
   Ok::<_,anyhow::Error>(response["result"].clone())
  };
  let result=tokio::select!{_=cancel.cancelled()=>Err(anyhow!("Engine call cancelled")),result=tokio::time::timeout_at(deadline,action)=>result.unwrap_or_else(|_|Err(anyhow!("Engine call timeout")))};
  if result.is_err(){*connection=None;}result
 }
}
/// Decode a closed response envelope before any typed contract result is accepted.
fn decode_pure_response<R:serde::de::DeserializeOwned>(response:&Value,field:&str,implementation:&str)->Result<R> {
 let object=response.as_object().ok_or_else(||anyhow!("Engine response shape"))?;
 if object.len()!=3 || response["status"]!="success" || response["implementation_version"]!=implementation
  || !object.contains_key(field) {anyhow::bail!("Context component response identity");}
 Ok(serde_json::from_value(response[field].clone())?)
}
/// Bind the typed response to its complete host-owned input before exposing it.
fn checked_pure_response<T,R:serde::de::DeserializeOwned>(input:&T,response:&Value,field:&str,implementation:&str,
 validate:impl Fn(&T,&R)->Result<()>)->Result<R> {
 let result=decode_pure_response(response,field,implementation)?;
 validate(input,&result)?;Ok(result)
}
/// Pin one installed pure generation and validate its exact typed correspondence.
/// A bad service is retired and cooled down before paused work may be restored.
#[cfg(feature="live-context")]
async fn pure_call<T:serde::Serialize,R:serde::de::DeserializeOwned>(root:&std::path::Path,id:&str,input:&T,
 cancel:&CancellationToken,field:&str,validate:impl Fn(&T,&R)->Result<()>)->Result<R> {
 if !component_available(root,id){anyhow::bail!("Required context component unavailable");}
 let catalog=catalog()?;let descriptor=catalog.entries.get(id).ok_or_else(||anyhow!("Required context component missing"))?;
 let service=service(&catalog,descriptor)?;
 let result=async {
  let response=service.call(id,input,cancel).await?;
  checked_pure_response(input,&response,field,&descriptor.implementation_version,validate)
 }.await;
 if result.is_err() {
  *service.connection.lock().await=None;
  if !cancel.is_cancelled() {record_pure_failure(root,id);}
 }
 let result=result?;
 if cancel.is_cancelled(){anyhow::bail!("Engine call cancelled");}
 require_component_snapshot(root,id,&catalog.revision)?;
 Ok(result)
}
/// Compute a bounded metadata plan; callers validate it before transactional persistence.
pub async fn assets_plan(root:&std::path::Path,input:&webagent_assets_contracts::Request,cancel:&CancellationToken)->Result<webagent_assets_contracts::Plan> {
 webagent_assets_contracts::validate_request(input)?;
 if !component_available(root,ASSETS){anyhow::bail!("Context assets component unavailable");}
 let revision=catalog()?.revision.clone();
 #[cfg(feature="linked-context")] let plan=webagent_context_assets::plan(input)?;
 #[cfg(feature="live-context")] let plan=pure_call(root,ASSETS,input,cancel,"plan",webagent_assets_contracts::validate_plan).await?;
 webagent_assets_contracts::validate_plan(input,&plan)?;
 if cancel.is_cancelled(){anyhow::bail!("Engine call cancelled");}
 require_component_snapshot(root,ASSETS,&revision)?;Ok(plan)
}
/// Compose current host-resolved sources; returned bodies must correspond exactly.
pub async fn capability_prepare(root:&std::path::Path,input:&webagent_capability_contracts::PrepareRequest,cancel:&CancellationToken)->Result<webagent_capability_contracts::PreparedCapability> {
 webagent_capability_contracts::validate_request(input)?;
 if !component_available(root,CAPABILITY){anyhow::bail!("Capability component unavailable");}
 let revision=catalog()?.revision.clone();
 #[cfg(feature="linked-context")] let result=webagent_capability_context::prepare(input)?;
 #[cfg(feature="live-context")] let result=pure_call(root,CAPABILITY,input,cancel,"capability",webagent_capability_contracts::validate_result).await?;
 webagent_capability_contracts::validate_result(input,&result)?;
 if cancel.is_cancelled(){anyhow::bail!("Engine call cancelled");}
 require_component_snapshot(root,CAPABILITY,&revision)?;Ok(result)
}
/// Prepare references-only provider projection before credentials and dispatch fencing.
pub async fn provider_project(root:&std::path::Path,input:&webagent_provider_contracts::RequestV1,cancel:&CancellationToken)->Result<webagent_provider_contracts::ResultV1> {
 webagent_provider_contracts::validate_request(input)?;
 if !component_available(root,PROJECTION){anyhow::bail!("Provider projection component unavailable");}
 let revision=catalog()?.revision.clone();
 #[cfg(feature="linked-context")] let result=webagent_provider_projection::project(input)?;
 #[cfg(feature="live-context")] let result=pure_call(root,PROJECTION,input,cancel,"projection",webagent_provider_contracts::validate_result).await?;
 webagent_provider_contracts::validate_result(input,&result)?;
 if cancel.is_cancelled(){anyhow::bail!("Engine call cancelled");}
 require_component_snapshot(root,PROJECTION,&revision)?;Ok(result)
}

/// Invoke the registered planner and verify its safety contract before host-owned effects.
pub async fn background_plan(root:&std::path::Path,stop:&CancellationToken,input:&webagent_background_contracts::Request)->Result<webagent_background_contracts::Plan> {
 webagent_background_contracts::validate_request(input)?;
 if !component_available(root,BACKGROUND){anyhow::bail!("Background component unavailable");}
 #[cfg(feature="linked-context")]
 let plan=webagent_background_dispatch::plan(input)?;
 #[cfg(feature="live-context")]
 let plan={
  let catalog=catalog()?;let descriptor=catalog.entries.get(BACKGROUND).ok_or_else(||anyhow!("Background component unavailable"))?;
  let service=service(&catalog,descriptor)?;
  let result=async {
   let response=service.call("background-dispatch",input,stop).await?;
   if response["status"]!="success"||response["implementation_version"]!=descriptor.implementation_version{anyhow::bail!("Background component response identity");}
   let plan:webagent_background_contracts::Plan=serde_json::from_value(response["plan"].clone())?;
   webagent_background_contracts::validate_plan(input,&plan)?;
   Ok::<_,anyhow::Error>(plan)
  }.await;
  match result {Ok(plan)=>plan,Err(error)=>{
   *service.connection.lock().await=None;
   *BACKGROUND_FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+Duration::from_secs(5));
   return Err(error);
  }}
 };
 webagent_background_contracts::validate_plan(input,&plan)?;
 if !component_available(root,BACKGROUND){anyhow::bail!("Background component withdrawn");}
 Ok(plan)
}

/// Cleanup every admitted generation when an executing run yields or terminates.
pub async fn release_run(run_key:&str) {
 let services=SERVICES.get().map(|map|map.lock().unwrap_or_else(|e|e.into_inner()).values().cloned().collect::<Vec<_>>()).unwrap_or_default();
 for service in services {let _=service.release(run_key).await;}
}
/// Independently enforce the contract; service reports cannot authorize oversized requests or unsafe folding.
fn validated<'a>(request:context_input::Request<'a>,state:&mut context_accounting::ContextAccounting,result:Value)->Result<context_assembly::Prepared<'a>>{
 if result["status"]!="success"{anyhow::bail!("Engine response validation");}
 let prepared:webagent_context_contracts::protocol::PreparedResult=serde_json::from_value(result["prepared"].clone())?;
 state.sync(request.history)?;
 let selected=context_input::select(request.contributions,request.identity)?;
 let mut expected=format!("{}{}",request.base,request.guidance);
 for item in &selected {if item.disposition=="included"&&!state.contributions_projected{expected.push('\n');expected.push_str(&item.item.content);}}
 let instruction_bytes=context_accounting::json_size(&expected)?;
 let schemas_bytes=context_accounting::json_size(request.schemas)?;
 let framing=b"{\"instructions\":,\"tools\":,\"input\":}".len();
 let overhead=framing+instruction_bytes+schemas_bytes+state.projection_extra_bytes;
 let history_budget=request.max_bytes.checked_sub(overhead).filter(|n|*n>request.compaction.summary_reserve_bytes)
  .ok_or_else(||anyhow!("Engine budget validation"))?;
 match prepared {
  webagent_context_contracts::protocol::PreparedResult::Ready{instructions,report}=>{
   let model=context_assembly::model_metadata(request.selection);
   let overlays=selected.iter().map(|item|json!({"id":item.item.id,"source":item.item.source,"lifetime":item.item.lifetime,"disposition":item.disposition,"content_bytes":item.item.content.len()})).collect::<Vec<_>>();
   let schemas=request.schemas.iter().map(|schema|Ok(json!({"name":schema["name"].as_str().unwrap_or(""),"bytes":context_accounting::json_size(schema)?,"bootstrap":matches!(schema["name"].as_str(),Some("list_abilities"|"load_ability"))}))).collect::<Result<Vec<Value>>>()?;
   if report.version!=1||report.scope!="logical_call_envelope"||report.accounting!="serialized_json_bytes_not_provider_tokens"
    ||report.base_content_bytes!=request.base.len()||report.guidance_content_bytes!=request.guidance.len()
    ||serde_json::to_value(&report.overlays)?!=json!(overlays)||serde_json::to_value(&report.schemas)?!=json!(schemas)
    ||instructions!=expected||instructions.is_empty()||report.envelope_bytes!=framing+instruction_bytes+schemas_bytes+state.bytes()+state.projection_extra_bytes
    ||report.envelope_bytes>request.max_bytes||report.max_bytes!=request.max_bytes||report.history_budget_bytes!=history_budget
    ||report.summary_reserve_bytes!=request.compaction.summary_reserve_bytes||report.instruction_json_bytes!=instruction_bytes
    ||report.schemas_json_bytes!=schemas_bytes||report.history_json_bytes!=state.bytes()+state.projection_extra_bytes||report.projection_extra_bytes!=state.projection_extra_bytes||report.framing_bytes!=framing
    ||report.turn!=request.identity.turn||report.call!=request.identity.call||report.provider!=model.provider||report.model!=model.model||report.transport!=model.transport
    ||serde_json::to_value(&report.history)?!=serde_json::to_value(state.report())? {
     anyhow::bail!("Engine envelope validation");
   }
   Ok(context_assembly::Prepared::Ready(context_assembly::Envelope{input:request.history,instructions,schemas:request.schemas,report}))
  },
  webagent_context_contracts::protocol::PreparedResult::Compact{plan}=>{
   webagent_context_contracts::context_compaction::validate_external_plan(request.history,&plan,&request.compaction.0,history_budget)?;
   Ok(context_assembly::Prepared::Compact(plan))
  }
 }
}
/// Plan duplicate references through the existing context component and validate originals in the host.
pub async fn tool_results_plan(root:&std::path::Path,run:&Value,input:&webagent_context_contracts::tool_results::Request,
 cancel:&CancellationToken)->Result<webagent_context_contracts::tool_results::Plan> {
 if !available(root){anyhow::bail!("Context component unavailable for result projection");}
 let catalog=catalog()?;let revision=catalog.revision.clone();
 #[cfg(feature="linked-context")]
 let plan=webagent_context_assembly::tool_results::project(input)?;
 #[cfg(feature="live-context")]
 let plan={
  let descriptor=catalog.entries.get(CONTEXT).ok_or_else(||anyhow!("Context component missing"))?;
  let service=service(&catalog,descriptor)?;
  let run_key=format!("{}:{}",run["id"].as_str().unwrap_or(""),run["generation"].as_str().unwrap_or(""));
  let result=async {
   let response=service.call(&run_key,&json!({"tool_result_projection":input}),cancel).await?;
   checked_pure_response(input,&response,"tool_result_projection",&descriptor.implementation_version,
    webagent_context_contracts::tool_results::validate_plan)
  }.await;
  match result {Ok(plan)=>plan,Err(error)=>{
   *service.connection.lock().await=None;
   *FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+Duration::from_secs(5));
   return Err(error);
  }}
 };
 let _=run;
 webagent_context_contracts::tool_results::validate_plan(input,&plan)?;
 if cancel.is_cancelled(){anyhow::bail!("Context projection cancelled");}
 require_component_snapshot(root,CONTEXT,&revision)?;Ok(plan)
}

/// Pin the validated generation before IO and keep borrowed durable inputs at the host.
pub async fn prepare<'a>(root:&std::path::Path,run:&Value,request:context_input::Request<'a>,state:&mut context_accounting::ContextAccounting,cancel:&tokio_util::sync::CancellationToken)->Result<context_assembly::Prepared<'a>> {
 if !available(root){anyhow::bail!("Engine component unavailable");}
 #[cfg(feature="linked-context")] {return context_assembly::prepare_incremental(request,state);}
 #[cfg(feature="live-context")] {
 let catalog=catalog()?;let descriptor=catalog.entries.get(CONTEXT).ok_or_else(||anyhow!("Engine component unavailable"))?;
 let input=webagent_context_contracts::protocol::PrepareRequest{
  history:request.history.to_vec(),base:request.base.into(),guidance:request.guidance.into(),schemas:request.schemas.to_vec(),contributions:request.contributions.to_vec(),identity:request.identity,
  model:context_assembly::model_metadata(request.selection),max_bytes:request.max_bytes,compaction:request.compaction.0.clone(),
  projection_extra_bytes:state.projection_extra_bytes,contributions_projected:state.contributions_projected,system_input_budget:request.compaction.system_input_budget,summary_instructions:request.compaction.summary_instructions.clone(),
 };
 let run_key=format!("{}:{}",run["id"].as_str().unwrap_or(""),run["generation"].as_str().unwrap_or(""));
 let service=service(&catalog,descriptor)?;
 let response=match service.call(&run_key,&input,cancel).await {
  Ok(response)=>response,
  Err(error)=>{
   *FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+Duration::from_secs(5));
   return Err(error);
  }
 };
 if cancel.is_cancelled(){anyhow::bail!("Engine call cancelled");}
 if response["status"]=="failure" {
  // Map fixed failure identities without exposing process-supplied reason strings.
  let pair=match (response["code"].as_str(),response["stage"].as_str()) {
   (Some("invalid_contribution"),Some("selection"))=>("invalid_contribution","selection"),
   (Some("instruction_limit"),Some("instructions"))=>("instruction_limit","instructions"),
   (Some("overhead_exceeds_budget"),Some("budget"))=>("overhead_exceeds_budget","budget"),
   (Some("summary_reserve_unavailable"),Some("budget"))=>("summary_reserve_unavailable","budget"),
   (Some("trigger_overhead_exceeds_budget"),Some("compaction_plan"))=>("trigger_overhead_exceeds_budget","compaction_plan"),
   (Some("invalid_measurements"),Some("compaction_plan"))=>("invalid_measurements","compaction_plan"),
   (Some("no_safe_boundary"),Some("compaction_plan"))=>("no_safe_boundary","compaction_plan"),
   _=>("assembly_internal","assembly"),
  };
  return Err(context_assembly::Failure::new(pair.0,pair.1).into());
 }
 let result=validated(request,state,response);
 if result.is_err(){
  *service.connection.lock().await=None;
  *FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+Duration::from_secs(5));
 }
 result
 }
}

#[cfg(test)]
mod tests {
 use crate::provider;
 use super::*;
 /// Provide the registered context descriptor for bounded component-contract fixtures.
 fn descriptor()->Descriptor {catalog().unwrap().entries[CONTEXT].clone()}
 #[cfg(feature="linked-context")]
 #[test]
 /// Keep linked execution identity tied to compiled bytes rather than published labels.
 fn linked_identity_is_build_owned() {
  let mut declaration=descriptor();declaration.implementation_version="future-live-version".into();declaration.artifact_sha256=Some("a".repeat(64));
  let (version,generation)=execution_identity(&declaration);
  assert_eq!(version,webagent_context_assembly::IMPLEMENTATION_VERSION);assert_ne!(generation,declaration.artifact_sha256.unwrap());
 }
 #[test]
 /// A registered module service enters the catalog only with its own group, package and limits.
 #[test]
 fn module_toy_descriptor_validates_only_as_a_module() {
  let path=PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../plugins/engine/module-toy/component.json");
  let value:Value=serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
  serde_json::from_value::<Descriptor>(value.clone()).unwrap().validate().unwrap();
  for (pointer,bad) in [("/group",json!("engine-loop")),("/build/package",json!("webagent-context-service")),("/limits/timeout_ms",json!(30000)),("/id",json!("context-assembly"))] {
   let mut changed=value.clone();*changed.pointer_mut(pointer).unwrap()=bad;
   assert!(serde_json::from_value::<Descriptor>(changed).unwrap().validate().is_err(),"{pointer}");
  }
 }
 /// Reuse an identical generation while refreshing its validated respawn location.
 fn unchanged_service_refreshes_respawn_path() {
  let declaration=descriptor();let mut catalog=Catalog{revision:"old".into(),root:Some(PathBuf::from("/old")),entries:BTreeMap::new(),withdrawn:Default::default()};
  let first=service(&catalog,&declaration).unwrap();catalog.root=Some(PathBuf::from("/new"));let next=service(&catalog,&declaration).unwrap();
  assert!(Arc::ptr_eq(&first,&next));assert!(next.executable.lock().unwrap().starts_with("/new"));
 }
 /// Exercise the official workspace binary through host confinement and generation retirement.
 #[tokio::test]
 async fn real_service_runs_inside_host_boundary_and_disable_reaps_without_prepare() {
  use std::os::unix::fs::PermissionsExt;
  let target=std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join("target"));
  let binary=target.join("debug/webagent-context-service");assert!(binary.is_file(),"workspace service binary required");
  let root=tempfile::tempdir().unwrap();let folder=root.path().join("plugins/engine/context-assembly/artifact");std::fs::create_dir_all(&folder).unwrap();
  let executable=folder.join("webagent-context-service");std::fs::copy(binary,&executable).unwrap();std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o555)).unwrap();
  let mut declaration=descriptor();declaration.implementation_version="host-boundary-test".into();
  let catalog=Catalog{revision:"fixture".into(),root:Some(root.path().to_owned()),entries:BTreeMap::from([(CONTEXT.into(),declaration.clone())]),withdrawn:Default::default()};
  let process=service(&catalog,&declaration).unwrap();
  let mut compaction=context_compaction::Config::configured().unwrap().0;compaction.trigger_tokens=10000;compaction.tail_tokens=1000;compaction.summary_reserve_bytes=100;
  let mut input=webagent_context_contracts::protocol::PrepareRequest{history:vec![json!({"role":"user","content":"short"})],base:"host instructions".into(),guidance:" guidance".into(),schemas:vec![],contributions:vec![],
   identity:context_input::CallIdentity{turn:1,call:1},model:webagent_context_contracts::context_input::ModelMetadata{provider:"deepseek".into(),model:"test".into(),transport:"chat_completions".into()},max_bytes:8192,compaction,projection_extra_bytes:0,contributions_projected:false,system_input_budget:None,summary_instructions:"summary instructions".into()};
  let first=process.call("host-boundary-run",&input,&CancellationToken::new()).await.unwrap();
  let pid=process.connection.lock().await.as_ref().unwrap().0.id().unwrap();
  assert_eq!(first["prepared"],serde_json::to_value(webagent_context_assembly::protocol::prepare_owned(&input,&mut Default::default()).unwrap()).unwrap());
  input.history[0]["content"]=json!("changed é🌍 history");
  let second=process.call("host-boundary-run",&input,&CancellationToken::new()).await.unwrap();
  assert_eq!(second["prepared"],serde_json::to_value(webagent_context_assembly::protocol::prepare_owned(&input,&mut Default::default()).unwrap()).unwrap());
  assert_eq!(process.connection.lock().await.as_ref().unwrap().0.id(),Some(pid));process.release("host-boundary-run").await.unwrap();
  drop(process);save_controls(root.path(),&Controls{revision:1,disabled:vec![CONTEXT.into()]}).unwrap();adoption(root.path(),"loop").unwrap();
  tokio::time::timeout(Duration::from_secs(2),async{while PathBuf::from(format!("/proc/{pid}")).exists(){tokio::time::sleep(Duration::from_millis(10)).await;}}).await.unwrap();
 }
 #[test]
 /// Validate explicit runtime dependencies, cycle rejection and registered operation boundaries.
 fn registered_contracts_and_runtime_graph_are_independent_of_groups(){
  let a=descriptor();a.validate().unwrap();let mut b=a.clone();b.id="second-context".into();b.group="module-host".into();b.runtime_requirements=vec![CONTEXT.into()];
  b.validate().unwrap();let mut graph=BTreeMap::from([(a.id.clone(),a),(b.id.clone(),b)]);
  validate_graph(&graph).unwrap();
  let catalog=Catalog{revision:"fixture".into(),root:None,entries:graph.clone(),withdrawn:Default::default()};
  assert!(requirement_available(&catalog,&Controls::default(),"second-context"));
  assert!(!requirement_available(&catalog,&Controls{revision:1,disabled:vec![CONTEXT.into()]},"second-context"));
  graph.get_mut(CONTEXT).unwrap().runtime_requirements=vec!["second-context".into()];assert!(validate_graph(&graph).is_err());
  graph.get_mut(CONTEXT).unwrap().runtime_requirements=vec!["missing".into()];assert!(validate_graph(&graph).is_err());
  let mut unknown=descriptor();unknown.operation="arbitrary.host.operation".into();assert!(unknown.validate().is_err());
 }
 #[test]
 /// Reject altered sealed artifacts while accepting an explicit catalog withdrawal.
 fn sealed_artifacts_and_deliberate_removal_are_distinct(){
  use sha2::Digest;use std::os::unix::fs::PermissionsExt;
  let root=tempfile::tempdir().unwrap();let folder=root.path().join("plugins/engine/context-assembly");
  std::fs::create_dir_all(folder.join("artifact")).unwrap();
  let mut declaration=descriptor();let binary=folder.join(&declaration.executable);std::fs::write(&binary,b"immutable test artifact").unwrap();std::fs::set_permissions(&binary,std::fs::Permissions::from_mode(0o555)).unwrap();
  declaration.artifact_sha256=Some(hex::encode(sha2::Sha256::digest(b"immutable test artifact")));
  let bytes=serde_json::to_vec(&declaration).unwrap();
  std::fs::write(folder.join("component.json"),&bytes).unwrap();
  let member=json!({"id":CONTEXT,"descriptor_sha256":hex::encode(sha2::Sha256::digest(&bytes)),"artifact_sha256":declaration.artifact_sha256});
  let meta=json!({"schema_version":1,"revision":uuid(),"components":[member],"withdrawn":[]});
  std::fs::write(root.path().join("plugins/engine/catalog.json"),serde_json::to_vec(&meta).unwrap()).unwrap();
  assert_eq!(load(root.path(),"candidate").unwrap().entries.len(),1);
  std::fs::set_permissions(&binary,std::fs::Permissions::from_mode(0o755)).unwrap();
  std::fs::write(&binary,b"tampered").unwrap();assert!(load(root.path(),"candidate").is_err());
  std::fs::remove_dir_all(folder).unwrap();
  std::fs::write(root.path().join("plugins/engine/catalog.json"),serde_json::to_vec(&json!({"schema_version":1,"revision":uuid(),"components":[],"withdrawn":[CONTEXT]})).unwrap()).unwrap();
  assert!(load(root.path(),"candidate").unwrap().entries.is_empty());
 }
 #[test]
 /// Allow known withdrawals to withhold dependents without accepting undeclared dependency IDs.
 fn known_withdrawals_pause_dependents_but_unknown_requirements_are_rejected(){
  let mut a=descriptor();a.runtime_requirements=vec!["dependency-context".into()];
  let mut b=descriptor();b.id="dependency-context".into();
  let graph=BTreeMap::from([(a.id.clone(),a.clone()),(b.id.clone(),b)]);
  validate_graph(&graph).unwrap();
  let removed=BTreeMap::from([(a.id.clone(),a)]);
  let withdrawn=std::collections::BTreeSet::from(["dependency-context".into()]);
  validate_graph_with_withdrawals(&removed,&withdrawn).unwrap();
  assert!(!requirement_available(&Catalog{revision:"fixture".into(),root:None,entries:removed.clone(),withdrawn},&Controls::default(),CONTEXT));
  assert!(validate_graph_with_withdrawals(&removed,&Default::default()).is_err());
  assert!(requirement_available(&Catalog{revision:"fixture".into(),root:None,entries:graph,withdrawn:Default::default()},&Controls::default(),CONTEXT));
 }

 #[test]
 /// Bind context reports to host-owned identity, accounting and overlay metadata.
 fn forged_report_metadata_cannot_enter_a_run(){
  let selection=provider::ProviderSelection::parse(&json!({"provider":"deepseek","model":"deepseek-chat"}),"unused").unwrap();
  let mut config=context_compaction::Config::configured().unwrap();config.summary_reserve_bytes=100;
  let history=[json!({"role":"user","content":"private history"})];let schemas=[json!({"name":"file_read"})];
  let additions=[context_input::Contribution{id:"mode".into(),source:"native".into(),lifetime:context_input::Lifetime::Call,content:"trusted".into(),turn:1,call:1}];
  let request=context_input::Request{history:&history,base:"host",guidance:"",schemas:&schemas,contributions:&additions,
   identity:context_input::CallIdentity{turn:1,call:1},selection:&selection,max_bytes:8192,compaction:&config};
  let context_assembly::Prepared::Ready(envelope)=context_assembly::prepare(request).unwrap()else{panic!("ready fixture");};
  let result=json!({"status":"success","prepared":{"kind":"ready","instructions":envelope.instructions,"report":envelope.report}});
  assert!(validated(request,&mut Default::default(),result.clone()).is_ok());
  for pointer in ["/prepared/report/scope","/prepared/report/accounting","/prepared/report/overlays/0/id","/prepared/report/overlays/0/source","/prepared/report/schemas/0/name"] {
   let mut forged=result.clone();*forged.pointer_mut(pointer).unwrap()=json!("private history");
   assert!(validated(request,&mut Default::default(),forged).is_err(),"{pointer}");
  }
 }


 /// Each new registry ID has an isolated bounded retry fence, including expiry.
 #[test]
 fn pure_component_cooldown_is_selective_root_scoped_and_expires() {
  let first=tempfile::tempdir().unwrap();let second=tempfile::tempdir().unwrap();
  for id in [ASSETS,CAPABILITY,PROJECTION] {
   record_pure_failure(first.path(),id);
   assert!(component_cooling(first.path(),id));
   assert!(!component_available(first.path(),id));
   assert!(!component_cooling(second.path(),id));
   for other in [ASSETS,CAPABILITY,PROJECTION] {if other!=id {assert!(!component_cooling(first.path(),other));}}
   PURE_FAILURE_UNTIL.get().unwrap().lock().unwrap().insert((first.path().to_owned(),id.into()),std::time::Instant::now()-Duration::from_secs(1));
   assert!(!component_cooling(first.path(),id));
  }
 }
 /// A retained explicit component identity survives another component's disable.

 /// Closed envelopes reject forged status/version/fields for every new operation.
 #[test]
 fn new_pure_component_reply_envelopes_are_closed() {
  for (id,field) in [(ASSETS,"plan"),(CAPABILITY,"capability"),(PROJECTION,"projection")] {
   let valid=json!({"status":"success","implementation_version":"0.1.0",(field):{}});
   assert!(decode_pure_response::<Value>(&valid,field,"0.1.0").is_ok(),"{id}");
   for (key,value) in [("status",json!("failure")),("implementation_version",json!("forged")),("unknown",json!(true))] {
    let mut forged=valid.clone();forged[key]=value;
    assert!(decode_pure_response::<Value>(&forged,field,"0.1.0").is_err(),"{id} {key}");
   }
   let mut missing=valid;missing.as_object_mut().unwrap().remove(field);
   assert!(decode_pure_response::<Value>(&missing,field,"0.1.0").is_err(),"{id}");
  }
 }
 /// Typed correspondence prevents replies from changing placements, text or profiles.
 #[test]
 fn new_pure_component_results_remain_bound_to_host_inputs() {
  let assets=webagent_assets_contracts::Request::Place{version:1,state:Default::default(),history:vec![],
   definitions:vec![],temporary:vec![],schema_names:vec![],history_basis:"epoch".into(),append:true};
  let assets_plan=webagent_assets_contracts::Plan::Place{placements:vec![],schema_order:vec![],rebased:true,input_digest:webagent_assets_contracts::digest(&assets).unwrap()};
  let mut reply=json!({"status":"success","implementation_version":"0.1.0","plan":assets_plan});
  assert!(checked_pure_response(&assets,&reply,"plan","0.1.0",webagent_assets_contracts::validate_plan).is_ok());
  reply["plan"]["input_digest"]=json!("forged");
  assert!(checked_pure_response(&assets,&reply,"plan","0.1.0",webagent_assets_contracts::validate_plan).is_err());
  let capability=webagent_capability_contracts::PrepareRequest{version:1,source_fingerprint:"a".repeat(64),base_prompt:"current main".into(),
   live_slots:String::new(),guidance:vec![],permitted_schemas:vec![],prior_schema_order:vec![],menu:String::new()};
  let prepared=webagent_capability_contracts::PreparedCapability{version:1,source_fingerprint:"a".repeat(64),main:"current main".into(),
   definitions:vec![],schema_order:vec![],temporary_menu:String::new(),inclusion:vec![]};
  let mut reply=json!({"status":"success","implementation_version":"0.1.0","capability":prepared});
  assert!(checked_pure_response(&capability,&reply,"capability","0.1.0",webagent_capability_contracts::validate_result).is_ok());
  reply["capability"]["main"]=json!("forged instructions");
  assert!(checked_pure_response(&capability,&reply,"capability","0.1.0",webagent_capability_contracts::validate_result).is_err());
  let provider=webagent_provider_contracts::RequestV1::Profile{version:1,provider:webagent_provider_contracts::ProviderIdV1::Deepseek,model:"smoke".into()};
  let profile=webagent_provider_contracts::ResultV1::Profile{version:1,profile:webagent_provider_contracts::profile_for(webagent_provider_contracts::ProviderIdV1::Deepseek)};
  let mut reply=json!({"status":"success","implementation_version":"0.1.0","projection":profile});
  assert!(checked_pure_response(&provider,&reply,"projection","0.1.0",webagent_provider_contracts::validate_result).is_ok());
  reply["projection"]["profile"]["instruction_role"]=json!("developer");
  assert!(checked_pure_response(&provider,&reply,"projection","0.1.0",webagent_provider_contracts::validate_result).is_err());
 }

}

/// Preserve the existing service-health recovery deadline independently of idle configuration.
pub fn background_failure_retry_ms()->Option<u64> {
 let guard=BACKGROUND_FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner());
 guard.and_then(|deadline|deadline.checked_duration_since(std::time::Instant::now()))
  .map(|remaining|remaining.as_millis().max(1).min(u64::MAX as u128)as u64)
}

/// A compatible implementation generation changes proof identity without linking its crate.
#[cfg(test)]
#[test]
fn offline_smoke_uses_frozen_descriptor_versions() {
 let root=tempfile::tempdir().unwrap();
 let path=root.path().join("plugins/engine").join(ASSETS).join("component.json");
 std::fs::create_dir_all(path.parent().unwrap()).unwrap();
 let mut descriptor:Value=serde_json::from_slice(&std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join("plugins/engine/context-assets/component.json")).unwrap()).unwrap();
 for version in ["0.1.1","0.2.7"] {
  descriptor["implementation_version"]=json!(version);
  std::fs::write(&path,serde_json::to_vec(&descriptor).unwrap()).unwrap();
  assert_eq!(smoke_implementation(root.path(),ASSETS).unwrap(),version);
 }
 let context_path=root.path().join("plugins/engine").join(CONTEXT).join("component.json");
 std::fs::create_dir_all(context_path.parent().unwrap()).unwrap();
 let context_descriptor=std::fs::read(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..").join("plugins/engine/context-assembly/component.json")).unwrap();
 let context_identity:Descriptor=serde_json::from_slice(&context_descriptor).unwrap();
 std::fs::write(&context_path,&context_descriptor).unwrap();
 assert_eq!(smoke_implementation(root.path(),CONTEXT).unwrap(),context_identity.implementation_version);
 descriptor["implementation_version"]=json!("");
 std::fs::write(&path,serde_json::to_vec(&descriptor).unwrap()).unwrap();
 assert!(smoke_implementation(root.path(),ASSETS).is_err());
 assert!(smoke_implementation(root.path(),"../unregistered").is_err());
}

/// Existing root integration fixture only; unavailable in ordinary linked/live production builds.
#[cfg(any(test,feature="test-support"))]
pub fn fixture_background_failure_for(duration:Duration) {
 *BACKGROUND_FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner())=Some(std::time::Instant::now()+duration);
}
