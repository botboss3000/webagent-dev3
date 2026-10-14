//! Host-owned engine discovery, activation and disposable process generations.
//! Catalog entries never grant tools or authority; only registered contracts run.
// @code-index {"version":1,"feature":"engine-components","aliases":["live engine modules","context service","component catalog"],"purpose":"Engine catalog, control fencing and generation lifecycle. Config: config/engine-components.json -> disabled (initial defaults only); data-root config/engine-components.json owns live revision/disabled. plugins/engine/context-assembly/component.json -> limits and runtime_requirements. plugins/engine/background-dispatch/component.json -> metadata-only planning limits and runtime_requirements.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled"]},{"path":"plugins/engine/context-assembly/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]},{"path":"plugins/engine/background-dispatch/component.json","pointers":["/limits/max_request_bytes","/limits/max_response_bytes","/limits/timeout_ms","/runtime_requirements"]}],"opportunities":[]}}
use crate::*;
use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;
use std::sync::{Mutex as StdMutex,OnceLock};
pub(crate) const CONTEXT:&str="context-assembly";
pub(crate) const BACKGROUND:&str="background-dispatch";
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Limits {pub max_request_bytes:usize,pub max_response_bytes:usize,pub timeout_ms:u64,pub max_concurrency:usize}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Build {pub package:String,pub source_inputs:Vec<String>,pub toolchain:String,pub features:Vec<String>,pub runtime_assets:Vec<String>}
#[derive(Clone,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Descriptor {
 pub schema_version:u32,pub id:String,pub group:String,pub interface_version:u32,
 pub implementation_version:String,pub operation:String,pub modes:Vec<String>,
 pub runtime_requirements:Vec<String>,pub build:Build,pub executable:String,
 #[serde(default)]pub artifact_sha256:Option<String>,pub limits:Limits,
}
/// Keep catalog and control identities bounded and safe as organizational labels.
fn valid_id(id:&str)->bool {!id.is_empty()&&id.len()<=64&&id.bytes().all(|c|c.is_ascii_lowercase()||c.is_ascii_digit()||c==b'-')}
impl Descriptor {
 /// Only reviewed fixed interfaces and confined build recipes may enter the engine catalog.
 pub(crate) fn validate(&self)->Result<()> {
  if self.schema_version!=1||!valid_id(&self.id)||self.interface_version!=1||!matches!(self.operation.as_str(),"context.prepare"|"background.plan"|"boot.plan")
   ||!valid_id(&self.group)||self.implementation_version.is_empty()||self.implementation_version.len()>64
   ||self.modes!=["live","linked"]||self.runtime_requirements.len()>16||self.runtime_requirements.iter().any(|id|!valid_id(id))
   ||self.executable!=format!("artifact/{}",self.build.package)
   ||match self.operation.as_str() {
    "context.prepare"=>self.build.package!="webagent-context-service",
    "background.plan"=>self.id!=BACKGROUND||self.build.package!="webagent-background-service",
    "boot.plan"=>self.id!="boot-supervisor"||self.build.package!="webagent-boot-service",_=>true
   }
   ||self.build.toolchain!="1.90.0"||self.build.source_inputs!=if self.operation=="context.prepare" {vec!["crates/context-service","crates/context-assembly","crates/context-contracts","Cargo.toml","Cargo.lock","config/context-compaction.json"]}else if self.operation=="boot.plan"{vec!["crates/boot-service","crates/boot-supervisor","crates/boot-contracts","crates/engine-service","Cargo.toml","Cargo.lock"]}else{vec!["crates/background-service","crates/background-dispatch","crates/background-contracts","crates/engine-service","Cargo.toml","Cargo.lock"]}
   ||!self.build.features.is_empty()||!self.build.runtime_assets.is_empty()
   ||self.limits.max_request_bytes==0||self.limits.max_request_bytes>16*1024*1024
   ||self.limits.max_response_bytes==0||self.limits.max_response_bytes>16*1024*1024
   ||self.limits.timeout_ms==0||self.limits.timeout_ms>30000||self.limits.max_concurrency!=1
   ||matches!(self.operation.as_str(),"background.plan"|"boot.plan")&&(self.limits.max_request_bytes>65536||self.limits.max_response_bytes>65536||self.limits.timeout_ms>1000)
   ||self.artifact_sha256.as_ref().is_some_and(|s|s.len()!=64||!s.bytes().all(|c|c.is_ascii_hexdigit())) {
    anyhow::bail!("Engine contract rejected");
  }Ok(())
 }
}
#[derive(Clone)]
pub(crate) struct Catalog {pub revision:String,pub root:Option<PathBuf>,pub entries:BTreeMap<String,Descriptor>,pub withdrawn:std::collections::BTreeSet<String>}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {id:String,descriptor_sha256:String,artifact_sha256:String}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Namespace {schema_version:u32,revision:String,components:Vec<Member>,withdrawn:Vec<String>}
/// Validate broker-owned membership, withdrawals and every descriptor/executable before activation.
pub(crate) fn load(root:&std::path::Path,revision:&str)->Result<Catalog>{
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
pub(crate) fn catalog()->Result<Arc<Catalog>>{
 let Some(root)=live_assets::published_root() else {
  let descriptor:Descriptor=serde_json::from_value(json!({"schema_version":1,"id":"context-assembly","group":"engine-loop","interface_version":1,"implementation_version":"0.1.0","operation":"context.prepare","modes":["live","linked"],"runtime_requirements":[],"build":{"package":"webagent-context-service","source_inputs":["crates/context-service","crates/context-assembly","crates/context-contracts","Cargo.toml","Cargo.lock","config/context-compaction.json"],"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/webagent-context-service","limits":{"max_request_bytes":16777216,"max_response_bytes":16777216,"timeout_ms":30000,"max_concurrency":1}}))?;
  descriptor.validate()?;
  return Ok(Arc::new(Catalog{revision:"embedded".into(),root:None,entries:BTreeMap::from([(CONTEXT.into(),descriptor),(BACKGROUND.into(),serde_json::from_value(json!({"schema_version":1,"id":"background-dispatch","group":"engine-loop","interface_version":1,"implementation_version":"0.1.0","operation":"background.plan","modes":["live","linked"],"runtime_requirements":[],"build":{"package":"webagent-background-service","source_inputs":["crates/background-service","crates/background-dispatch","crates/background-contracts","crates/engine-service","Cargo.toml","Cargo.lock"],"toolchain":"1.90.0","features":[],"runtime_assets":[]},"executable":"artifact/webagent-background-service","limits":{"max_request_bytes":65536,"max_response_bytes":65536,"timeout_ms":1000,"max_concurrency":1}}))?)]),withdrawn:Default::default()}));
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
pub(crate) struct Controls {pub revision:u64,pub disabled:Vec<String>}
/// Source defaults seed only absent runtime controls; saved disable tombstones survive releases.
pub(crate) fn initialize(root:&std::path::Path)->Result<()> {
 let path=root.join("config/engine-components.json");
 if path.exists(){controls(root)?;return Ok(());}
 let source=live_assets::published_root().unwrap_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")));
 let defaults=match std::fs::read(source.join("config/engine-components.json")) {
  Ok(bytes)=>{if bytes.len()>32768{anyhow::bail!("Engine defaults bound");}serde_json::from_slice::<Controls>(&bytes)?},
  Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Controls::default(),Err(e)=>return Err(e.into())
 };
 if defaults.revision!=0||defaults.disabled.len()>256||defaults.disabled.iter().any(|id|!valid_id(id)){anyhow::bail!("Invalid engine control defaults");}
 let dir=root.join("config");std::fs::create_dir_all(&dir)?;
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(&defaults)?)?;file.as_file().sync_all()?;
 match file.persist_noclobber(path){Ok(_)=>{},Err(e)if e.error.kind()==std::io::ErrorKind::AlreadyExists=>{},Err(e)=>return Err(e.error.into())};Ok(())
}
/// Runtime controls are distinct from immutable packages and contain no credentials.
pub(crate) fn controls(root:&std::path::Path)->Result<Controls>{
 let path=root.join("config/engine-components.json");
 match std::fs::read(path){Ok(bytes)=>{if bytes.len()>32768{anyhow::bail!("Engine controls too large");}let value:Controls=serde_json::from_slice(&bytes)?;if value.disabled.len()>256||value.disabled.iter().any(|s|!valid_id(s)){anyhow::bail!("Unknown engine control");}Ok(value)},
 Err(e)if e.kind()==std::io::ErrorKind::NotFound=>Ok(Controls::default()),Err(e)=>Err(e.into())}
}
/// Admission and dispatch both read current controls; there is no built-in fallback on removal.
static BACKGROUND_FAILURE_UNTIL:OnceLock<StdMutex<Option<std::time::Instant>>>=OnceLock::new();
static FAILURE_UNTIL:OnceLock<StdMutex<Option<std::time::Instant>>>=OnceLock::new();
pub(crate) fn available(root:&std::path::Path)->bool {
 if FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner()).is_some_and(|deadline|deadline>std::time::Instant::now()){return false;}
 component_available(root,CONTEXT)&&component_available(root,BACKGROUND)
}
/// Resolve one installed component and its runtime requirements against live controls.
pub(crate) fn component_available(root:&std::path::Path,id:&str)->bool {
 if id==CONTEXT&&FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner()).is_some_and(|deadline|deadline>std::time::Instant::now()){return false;}
 if id==BACKGROUND&&BACKGROUND_FAILURE_UNTIL.get_or_init(||StdMutex::new(None)).lock().unwrap_or_else(|e|e.into_inner()).is_some_and(|deadline|deadline>std::time::Instant::now()){return false;}
 match (controls(root),catalog()) {(Ok(controls),Ok(catalog))=>requirement_available(&catalog,&controls,id),_=>false}
}
/// Persist only validated administrator controls through an atomic owning-host write.
pub(crate) fn save_controls(root:&std::path::Path,current:&Controls)->Result<()> {
 let dir=root.join("config");std::fs::create_dir_all(&dir)?;
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(current)?)?;file.as_file().sync_all()?;
 file.persist(dir.join("engine-components.json")).map_err(anyhow::Error::from)?;Ok(())
}
/// Save a host-marked pause while preserving accepted input and run revision fencing.
pub(crate) async fn pause(app:&App,run:&mut Value)->Result<()> {
 let component=if component_available(&app.store.root,CONTEXT)&&!component_available(&app.store.root,BACKGROUND){BACKGROUND}else{CONTEXT};
 run["status"]=json!("paused");run["component_pause"]=json!({"component":component,"version":1});
 run["error"]=json!("Required engine component unavailable; work resumes when the component is restored");
 app.save(run).await
}
/// Recognize safe host-retained pauses; manual controls and uncertain effects stay fenced.
pub(crate) fn retained_pause(run:&Value)->bool {
 status(run)=="paused" && run["component_pause"]["version"]==1
 && matches!(run["component_pause"]["component"].as_str(),Some(CONTEXT|BACKGROUND))
 && run["uncertain"]!=true && run["pending"]["state"]!="executing"
 && run["pending"]["external_job"].is_null()
 && !run["batch"].as_array().into_iter().flatten().any(|item|item["state"]=="executing"
   || item["pending"]["state"]=="executing" || !item["pending"]["external_job"].is_null())
}
/// Resume only host-marked component pauses, with current participation and model authority.
pub(crate) async fn restore(app:&App,id:&str)->Result<()> {
 if !available(&app.store.root)||app.shutoff().await?||app.active.lock().await.contains_key(id){return Ok(());}
 let mut run=app.run(id).await?;
 if !retained_pause(&run){return Ok(());}
 if !app.store.participation_allows_run(&run).await?||!app.run_model_authorized(&run).await {
  run["status"]=json!("stopped");run["component_pause"]=Value::Null;run["error"]=json!("Access changed while engine component was unavailable");
 }else{
  run["status"]=json!("queued");run["component_pause"]=Value::Null;run["error"]=Value::Null;
 }
 app.save(&mut run).await?;crate::wake::notify(app).await;Ok(())
}
/// Adoption attests the catalog held by the consumer, not an arbitrary filesystem pointer.
pub(crate) fn adoption(root:&std::path::Path,role:&str)->Result<()>{
 let c=catalog()?;
 if let Some(services)=SERVICES.get(){let mut services=services.lock().unwrap_or_else(|e|e.into_inner());services.retain(|hash,service|(controls(root).is_ok_and(|controls|c.entries.values().any(|d|requirement_available(&c,&controls,&d.id)&&serde_json::to_value(d).is_ok_and(|v|crate::digest(&v)==*hash))))||Arc::strong_count(service)>1);}
 let dir=root.join("publication");std::fs::create_dir_all(&dir)?;
 let value=json!({"revision":c.revision,"components":c.entries.keys().collect::<Vec<_>>(),"mode":if cfg!(feature="live-context"){"live"}else{"linked"}});
 let path=dir.join(format!("engine-{role}.json"));if std::fs::read(&path).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok())==Some(value.clone()){return Ok(());}
 let mut file=tempfile::NamedTempFile::new_in(&dir)?;file.write_all(&serde_json::to_vec(&value)?)?;file.as_file().sync_all()?;file.persist(path).map_err(anyhow::Error::from)?;Ok(())
}

/// Report execution identity independently of installed live package metadata.
pub(crate) fn execution_identity(descriptor:&Descriptor)->(String,String) {
 #[cfg(feature="live-context")]
 { (descriptor.implementation_version.clone(),descriptor.artifact_sha256.clone().unwrap_or_default()) }
 #[cfg(feature="linked-context")]
 { let _=descriptor;static GENERATION:OnceLock<String>=OnceLock::new();
   let generation=GENERATION.get_or_init(||std::env::current_exe().ok().and_then(|path|std::fs::read(path).ok()).map(|bytes|hex::encode(sha2::Sha256::digest(bytes))).unwrap_or_else(||"linked-build-unavailable".into()));
   (if descriptor.id==BACKGROUND {webagent_background_dispatch::IMPLEMENTATION_VERSION}else if descriptor.id=="boot-supervisor"{webagent_boot_supervisor::IMPLEMENTATION_VERSION}else{webagent_context_assembly::IMPLEMENTATION_VERSION}.into(),generation.clone()) }
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
  let input=Request{version:1,now:time,slots:2,tasks:vec![Task{state:State::Queued,active:false,lease_until:0},Task{state:State::Running,active:false,lease_until:101},Task{state:State::Completed,active:false,lease_until:0}]};
  #[cfg(feature="linked-context")]let plan=webagent_background_dispatch::plan(&input)?;
  #[cfg(feature="live-context")]let plan:Plan={
   let reply=process.call("offline-background-smoke",&input,&CancellationToken::new()).await?;
   if reply["status"]!="success"{anyhow::bail!("Background smoke response");}
   let current=process.connection.lock().await.as_ref().and_then(|value|value.0.id()).ok_or_else(||anyhow!("Background smoke exited"))?;
   if pid.is_some_and(|previous|previous!=current){anyhow::bail!("Background smoke reuse");}pid=Some(current);
   serde_json::from_value(reply["plan"].clone())?
  };
  webagent_background_contracts::validate_plan(&input,&plan)?;
  let expected=Plan{version:1,dispatch:if time==100{vec![0]}else{vec![0,1]},followup:vec![2],retry_ms:if time==100{1000}else{5000}};
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
/// Exercise only public deterministic inputs offline; this CLI never opens application state or credentials.
pub(crate) async fn context_smoke()->Result<Value> {
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
  let owned=PrepareRequest{history:history.clone(),base:request.base.into(),guidance:request.guidance.into(),schemas:schemas.clone(),contributions:contributions.clone(),identity:request.identity,model:context_assembly::model_metadata(&selection),max_bytes:request.max_bytes,compaction:config.0.clone(),system_input_budget:None,summary_instructions:config.summary_instructions.clone()};
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
 #[cfg(feature="live-context")]
 {
  process.release("offline-context-smoke").await?;
  if let Some((mut child,writer,reader))=process.connection.lock().await.take(){drop(writer);drop(reader);tokio::time::timeout(Duration::from_secs(1),child.wait()).await.map_err(|_|anyhow!("Offline service drain timeout"))??;}
 }
 let mut result=json!({"mode":if cfg!(feature="live-context"){"live"}else{"linked"},"equivalence":true,"prepared":cases,"background_prepared":background_smoke().await?,"boot_prepared":boot_smoke().await?,"elapsed_us":started.elapsed().as_micros(),"interface_version":1});
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
    let sandbox=crate::engine_sandbox::prepare(&executable)?;
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
/// Invoke the registered planner and verify its safety contract before host-owned effects.
pub(crate) async fn background_plan(app:&App,input:&webagent_background_contracts::Request)->Result<webagent_background_contracts::Plan> {
 webagent_background_contracts::validate_request(input)?;
 if !component_available(&app.store.root,BACKGROUND){anyhow::bail!("Background component unavailable");}
 #[cfg(feature="linked-context")]
 let plan=webagent_background_dispatch::plan(input)?;
 #[cfg(feature="live-context")]
 let plan={
  let catalog=catalog()?;let descriptor=catalog.entries.get(BACKGROUND).ok_or_else(||anyhow!("Background component unavailable"))?;
  let service=service(&catalog,descriptor)?;
  let result=async {
   let response=service.call("background-dispatch",input,&app.stop).await?;
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
 if !component_available(&app.store.root,BACKGROUND){anyhow::bail!("Background component withdrawn");}
 Ok(plan)
}

/// Cleanup every admitted generation when an executing run yields or terminates.
pub(crate) async fn release_run(run_key:&str) {
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
 for item in &selected {if item.disposition=="included"{expected.push('\n');expected.push_str(&item.item.content);}}
 let instruction_bytes=context_accounting::json_size(&expected)?;
 let schemas_bytes=context_accounting::json_size(request.schemas)?;
 let framing=b"{\"instructions\":,\"tools\":,\"input\":}".len();
 let overhead=framing+instruction_bytes+schemas_bytes+2;
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
    ||instructions!=expected||instructions.is_empty()||report.envelope_bytes!=framing+instruction_bytes+schemas_bytes+state.bytes()
    ||report.envelope_bytes>request.max_bytes||report.max_bytes!=request.max_bytes||report.history_budget_bytes!=history_budget
    ||report.summary_reserve_bytes!=request.compaction.summary_reserve_bytes||report.instruction_json_bytes!=instruction_bytes
    ||report.schemas_json_bytes!=schemas_bytes||report.history_json_bytes!=state.bytes()||report.framing_bytes!=framing
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
/// Pin the validated generation before IO and keep borrowed durable inputs at the host.
pub(crate) async fn prepare<'a>(app:&App,run:&Value,request:context_input::Request<'a>,state:&mut context_accounting::ContextAccounting,cancel:&tokio_util::sync::CancellationToken)->Result<context_assembly::Prepared<'a>> {
 if !available(&app.store.root){anyhow::bail!("Engine component unavailable");}
 #[cfg(feature="linked-context")] {return context_assembly::prepare_incremental(request,state);}
 #[cfg(feature="live-context")] {
 let catalog=catalog()?;let descriptor=catalog.entries.get(CONTEXT).ok_or_else(||anyhow!("Engine component unavailable"))?;
 let input=webagent_context_contracts::protocol::PrepareRequest{
  history:request.history.to_vec(),base:request.base.into(),guidance:request.guidance.into(),schemas:request.schemas.to_vec(),contributions:request.contributions.to_vec(),identity:request.identity,
  model:context_assembly::model_metadata(request.selection),max_bytes:request.max_bytes,compaction:request.compaction.0.clone(),
  system_input_budget:request.compaction.system_input_budget,summary_instructions:request.compaction.summary_instructions.clone(),
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
  // Reproduce deterministic failures locally; never trust a process-supplied reason string.
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
 use super::*;
 fn descriptor()->Descriptor {catalog().unwrap().entries[CONTEXT].clone()}
 #[cfg(feature="linked-context")]
 #[test]
 fn linked_identity_is_build_owned() {
  let mut declaration=descriptor();declaration.implementation_version="future-live-version".into();declaration.artifact_sha256=Some("a".repeat(64));
  let (version,generation)=execution_identity(&declaration);
  assert_eq!(version,webagent_context_assembly::IMPLEMENTATION_VERSION);assert_ne!(generation,declaration.artifact_sha256.unwrap());
 }
 #[test]
 fn unchanged_service_refreshes_respawn_path() {
  let declaration=descriptor();let mut catalog=Catalog{revision:"old".into(),root:Some(PathBuf::from("/old")),entries:BTreeMap::new(),withdrawn:Default::default()};
  let first=service(&catalog,&declaration).unwrap();catalog.root=Some(PathBuf::from("/new"));let next=service(&catalog,&declaration).unwrap();
  assert!(Arc::ptr_eq(&first,&next));assert!(next.executable.lock().unwrap().starts_with("/new"));
 }
 /// Exercise the official workspace binary through host confinement and generation retirement.
 #[tokio::test]
 async fn real_service_runs_inside_host_boundary_and_disable_reaps_without_prepare() {
  use std::os::unix::fs::PermissionsExt;
  let target=std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(||PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target"));
  let binary=target.join("debug/webagent-context-service");assert!(binary.is_file(),"workspace service binary required");
  let root=tempfile::tempdir().unwrap();let folder=root.path().join("plugins/engine/context-assembly/artifact");std::fs::create_dir_all(&folder).unwrap();
  let executable=folder.join("webagent-context-service");std::fs::copy(binary,&executable).unwrap();std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o555)).unwrap();
  let mut declaration=descriptor();declaration.implementation_version="host-boundary-test".into();
  let catalog=Catalog{revision:"fixture".into(),root:Some(root.path().to_owned()),entries:BTreeMap::from([(CONTEXT.into(),declaration.clone())]),withdrawn:Default::default()};
  let process=service(&catalog,&declaration).unwrap();
  let mut compaction=context_compaction::Config::configured().unwrap().0;compaction.trigger_tokens=10000;compaction.tail_tokens=1000;compaction.summary_reserve_bytes=100;
  let mut input=webagent_context_contracts::protocol::PrepareRequest{history:vec![json!({"role":"user","content":"short"})],base:"host instructions".into(),guidance:" guidance".into(),schemas:vec![],contributions:vec![],
   identity:context_input::CallIdentity{turn:1,call:1},model:webagent_context_contracts::context_input::ModelMetadata{provider:"deepseek".into(),model:"test".into(),transport:"chat_completions".into()},max_bytes:8192,compaction,system_input_budget:None,summary_instructions:"summary instructions".into()};
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
 #[tokio::test]
 async fn restoration_preserves_manual_pauses_and_rechecks_access(){
  let (_root,app)=crate::runtime_tests::fixture().await;let sid=crate::runtime_tests::conversation(&app).await;
  let id=crate::runtime_tests::queued(&app,&sid).await;let mut run=app.run(&id).await.unwrap();
  let input=run["input"].clone();pause(&app,&mut run).await.unwrap();assert_eq!(run["status"],"paused");
  restore(&app,&id).await.unwrap();let restored=app.run(&id).await.unwrap();assert_eq!(restored["status"],"queued");assert_eq!(restored["input"],input);
  let mut manual=restored;manual["status"]=json!("paused");manual["component_pause"]=Value::Null;app.save(&mut manual).await.unwrap();
  restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"paused");
  pause(&app,&mut manual).await.unwrap();manual["selection_plan"]=Value::Null;app.save(&mut manual).await.unwrap();
  restore(&app,&id).await.unwrap();assert_eq!(app.run(&id).await.unwrap()["status"],"stopped");
 }
 #[test]
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
 #[tokio::test]
 async fn removal_during_credentials_pauses_normal_and_system_dispatch(){
  use axum::routing::post;
  for compact in [false,true] {
   let (_root,mut app)=crate::runtime_tests::fixture().await;
   let calls=Arc::new(std::sync::atomic::AtomicUsize::new(0));let captured=calls.clone();
   let mock=Router::new().route("/responses",post(move|Json(_body):Json<Value>|{
    let calls=captured.clone();async move{
     calls.fetch_add(1,Ordering::Relaxed);let event=json!({"type":"response.completed","response":{"status":"completed","output":[]}});
     ([("content-type","text/event-stream")],format!("data: {event}\n\n"))
    }
   }));
   let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
   let server=tokio::spawn(async move{axum::serve(listener,mock).await.unwrap();});
   app.provider=provider::Provider::for_test(format!("http://{address}/responses"));
   let sid=crate::runtime_tests::conversation(&app).await;let id=crate::runtime_tests::queued(&app,&sid).await;
   let mut run=app.run(&id).await.unwrap();
   if compact{
    let mut config=context_compaction::Config::read(&app.compaction_config_path).unwrap();
    config.trigger_tokens=16000;config.tail_tokens=50;config.summary_tokens=30;config.summary_reserve_bytes=300;config.persist(&app.compaction_config_path).unwrap();
    let mut history:Vec<Value>=(0..24).map(|index|json!({"role":if index%2==0{"user"}else{"assistant"},"content":"old".repeat(1400)})).collect();
    history.push(json!({"role":"user","content":"latest"}));run["input"]=Value::Array(history);
    app.save(&mut run).await.unwrap();
   }
   assert!(app.store.claim_run(&mut run,"engine-race-loop","engine-race").await.unwrap());
   let credentials=app.refresh.lock().await;let task_app=app.clone();let task_id=id.clone();
   let task=tokio::spawn(async move{crate::agent_loop::execute_run(task_app,task_id,"engine-race".into(),CancellationToken::new()).await});
   tokio::time::timeout(Duration::from_secs(10),async{
    loop{let current=app.run(&id).await.unwrap();
     if if compact {current["engine_component_prepared"]["kind"]=="compaction"}else{!current["context_assembly_report"].is_null()} {break;}
     if task.is_finished(){panic!("run ended before credential boundary: {}",current["error"]);}
     tokio::time::sleep(Duration::from_millis(10)).await;
    }
   }).await.unwrap();
   save_controls(&app.store.root,&Controls{revision:1,disabled:vec![CONTEXT.into()]}).unwrap();
   drop(credentials);tokio::time::timeout(Duration::from_secs(10),task).await.unwrap().unwrap().unwrap();
   let paused=app.run(&id).await.unwrap();assert_eq!(paused["status"],"paused");assert_eq!(paused["component_pause"]["component"],CONTEXT);
   assert_eq!(calls.load(Ordering::Relaxed),0);server.abort();
  }
 }

}
