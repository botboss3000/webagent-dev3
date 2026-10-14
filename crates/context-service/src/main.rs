//! Pure context service over inherited private pipes. No ambient authority is accepted.
use anyhow::{bail,Result};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use std::{io::{BufRead,Read,Write},collections::BTreeMap};
use sha2::{Sha256,Digest};
use webagent_context_assembly::{protocol::PrepareRequest,context_accounting::ContextAccounting};
const LIMIT:usize=16*1024*1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {version:u32,id:String,run_key:String,#[serde(default)]operation:Option<String>,#[serde(default)]input:Option<PrepareRequest>}
#[derive(Serialize)]
struct Reply {version:u32,id:String,result:Value}
struct Cached {state:ContextAccounting,len:usize,prefix:String}
/// Fail closed on unsupported architectures and prohibit ambient files/network/execution.
#[cfg(all(target_os="linux",target_arch="x86_64"))]
fn isolate()->Result<()> {
 #[repr(C)]struct Filter {code:u16,jt:u8,jf:u8,k:u32}
 #[repr(C)]struct Program {len:u16,filter:*const Filter}
 unsafe extern "C" {fn close(fd:i32)->i32;fn prctl(option:i32,...)->i32;fn setrlimit(resource:i32,limit:*const [u64;2])->i32;fn getrlimit(resource:i32,limit:*mut [u64;2])->i32;}
 let mut code=vec![Filter{code:0x20,jt:0,jf:0,k:4},Filter{code:0x15,jt:1,jf:0,k:0xc000003e},Filter{code:0x06,jt:0,jf:0,k:0x80000000},Filter{code:0x20,jt:0,jf:0,k:0}];
 // Minimal pure-computation/read-write pipe syscall set; alternate conventions fail closed.
 for nr in [0,1,3,5,8,9,10,11,12,13,14,15,16,24,28,39,60,72,131,186,202,218,228,231,273,302,318,334] {
  code.push(Filter{code:0x15,jt:0,jf:1,k:nr});code.push(Filter{code:0x06,jt:0,jf:0,k:0x7fff0000});
 }
 code.push(Filter{code:0x06,jt:0,jf:0,k:0x00050001});
 let p=Program{len:code.len()as u16,filter:code.as_ptr()};
 unsafe {
  for fd in 3..1024 {close(fd);}
  // The host prevents limit mutation after exec. Standalone checks set them here;
  // a denied mutation is acceptable only when the host already installed tighter bounds.
  for (resource,bound) in [(9,512*1024*1024u64),(7,32u64)] {
   if setrlimit(resource,&[bound,bound])!=0 {
    let mut current=[0u64;2];
    if getrlimit(resource,&mut current)!=0||current.iter().any(|limit|*limit>bound){bail!("context resource bounds unavailable");}
   }
  }
  if prctl(38,1,0,0,0)!=0||prctl(22,2,&p)!=0 {bail!("context isolation unavailable");}
 }Ok(())
}
#[cfg(not(all(target_os="linux",target_arch="x86_64")))]
fn isolate()->Result<()> {bail!("context isolation unsupported");}
/// Bound every frame before parsing; caches contain accounting only and are disposable.
fn serve()->Result<()> {
 isolate()?;
 let stdin=std::io::stdin();let mut reader=stdin.lock();let stdout=std::io::stdout();let mut writer=stdout.lock();
 let mut caches:BTreeMap<String,Cached>=BTreeMap::new();
 loop {
  let mut bytes=Vec::new();
  let length=(&mut reader).take((LIMIT+1)as u64).read_until(b'\n',&mut bytes)?;
  if length==0 {break;}
  if bytes.len()>LIMIT||bytes.last()!=Some(&b'\n'){bail!("context frame limit");}
  let call:Call=serde_json::from_slice(&bytes)?;
  if call.version!=1||call.id.len()>128||call.id.is_empty()||call.run_key.len()>256||call.run_key.is_empty(){bail!("context invocation rejected");}
  if call.operation.as_deref()==Some("release") {
   if call.input.is_some(){bail!("context release arguments");}
   let removed=caches.remove(&call.run_key).is_some();
   let bytes=serde_json::to_vec(&Reply{version:1,id:call.id,result:json!({"status":"released","removed":removed})})?;
   writer.write_all(&bytes)?;writer.write_all(b"\n")?;writer.flush()?;continue;
  }
  if call.operation.as_deref().is_some_and(|op|op!="prepare"){bail!("context operation rejected");}
  let input=call.input.ok_or_else(||anyhow::anyhow!("context request missing"))?;
  if caches.len()>=128&&!caches.contains_key(&call.run_key){if let Some(key)=caches.keys().next().cloned(){caches.remove(&key);}}
  let cached=caches.entry(call.run_key.clone()).or_insert_with(||Cached{state:ContextAccounting::default(),len:0,prefix:String::new()});
  if input.history.len()<cached.len||cached.len>0&&hex::encode(Sha256::digest(serde_json::to_vec(&input.history[..cached.len])?))!=cached.prefix {cached.state.reset();}
  let result=match webagent_context_assembly::protocol::prepare_owned(&input,&mut cached.state) {
   Ok(result)=>json!({"status":"success","prepared":result}),
   Err(error)=>{let(code,stage)=webagent_context_assembly::context_assembly::failure_code(&error);json!({"status":"failure","code":code,"stage":stage})}
  };
  cached.len=input.history.len();cached.prefix=hex::encode(Sha256::digest(serde_json::to_vec(&input.history)?));
  let bytes=serde_json::to_vec(&Reply{version:1,id:call.id,result})?;
  if bytes.len()>LIMIT {bail!("context response limit");}
  writer.write_all(&bytes)?;writer.write_all(b"\n")?;writer.flush()?;
 }
 Ok(())
}
/// Fixed operator-only probe exercises real denied syscalls without reading private data.
#[cfg(all(target_os="linux",target_arch="x86_64"))]
fn probe()->Result<()> {
 isolate()?;
 unsafe extern "C" {fn syscall(number:i64,...)->i64;}
 let path=std::ffi::CString::new("/etc/hostname")?;
 let executable=std::ffi::CString::new("/bin/true")?;
 let arguments=[executable.as_ptr(),std::ptr::null()];let environment=[std::ptr::null::<i8>()];
 let denied=unsafe{json!({"open":syscall(2,path.as_ptr(),0,0)==-1,"x32_open":syscall(0x40000002,path.as_ptr(),0,0)==-1,
 "socket":syscall(41,1,1,0)==-1,"network":syscall(41,2,1,0)==-1,"fork":syscall(57)==-1,
 "exec":syscall(59,executable.as_ptr(),arguments.as_ptr(),environment.as_ptr())==-1,"handle":syscall(304,-1,std::ptr::null::<u8>(),0)==-1,
 "file_write":std::fs::write("/tmp/webagent-context-isolation-probe",b"denied").is_err()})};
 println!("{denied}");Ok(())
}
/// Start only the fixed private protocol or bounded operator isolation probe.
fn main(){
 #[cfg(all(target_os="linux",target_arch="x86_64"))]
 if std::env::args().nth(1).as_deref()==Some("--isolation-probe"){if probe().is_err(){std::process::exit(1);}return;}
 if serve().is_err(){std::process::exit(1);}
}
