//! Atomic Engine publication over the existing scoped Storage resources.
use crate::*;
pub async fn terminal(store:&Store,run:&mut Value,output:&[Value],text:&str,policy:&workflow::Policy)->Result<()> {
 webagent_engine::publication::terminal(&webagent_engine::runtime::Scope::new(store.clone()),run,output,text,policy).await
}
