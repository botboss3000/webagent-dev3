//! Reusable bounded private-pipe wrapper for registered pure engine implementations.
use anyhow::{bail,Result};
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use std::io::{BufRead,Read,Write};
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
    if getrlimit(resource,&mut current)!=0||current.iter().any(|limit|*limit>bound){bail!("engine resource bounds unavailable");}
   }
  }
  if prctl(38,1,0,0,0)!=0||prctl(22,2,&p)!=0 {bail!("engine isolation unavailable");}
 }Ok(())
}
#[cfg(not(all(target_os="linux",target_arch="x86_64")))]
fn isolate()->Result<()> {bail!("engine isolation unsupported");}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {version:u32,id:String,run_key:String,#[serde(default)]operation:Option<String>,#[serde(default)]input:Option<Value>}
#[derive(Serialize)]
struct Reply {version:u32,id:String,result:Value}
/// Accept only fixed computation/release operations; no paths, sockets or ambient credentials.
pub fn serve(limit:usize, compute:impl Fn(Value)->Result<Value>) -> Result<()> {
 isolate()?;
 let stdin=std::io::stdin();let mut reader=stdin.lock();let stdout=std::io::stdout();let mut writer=stdout.lock();
 loop {
  let mut bytes=Vec::new();let length=(&mut reader).take((limit+1)as u64).read_until(b'\n',&mut bytes)?;
  if length==0 {return Ok(());}
  if bytes.len()>limit || bytes.last()!=Some(&b'\n') {bail!("Engine frame limit");}
  let call:Call=serde_json::from_slice(&bytes)?;
  if call.version!=1 || call.id.is_empty() || call.id.len()>128 || call.run_key.is_empty() || call.run_key.len()>256 {bail!("Engine invocation rejected");}
  let result=if call.operation.as_deref()==Some("release") {
   if call.input.is_some(){bail!("Engine release arguments");}json!({"status":"released","removed":false})
  } else {
   if call.operation.as_deref().is_some_and(|op|op!="prepare"){bail!("Engine operation rejected");}
   compute(call.input.ok_or_else(||anyhow::anyhow!("Engine request missing"))?)?
  };
  let response=serde_json::to_vec(&Reply{version:1,id:call.id,result})?;
  if response.len()+1>limit {bail!("Engine response limit");}
  writer.write_all(&response)?;writer.write_all(b"\n")?;writer.flush()?;
 }
}
