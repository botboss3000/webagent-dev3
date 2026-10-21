//! Optional Diagnostics recorder/server; no root App or Store dependency.
pub mod config;
pub mod server;
pub mod bridge;
pub mod background;
mod recorder;
pub use recorder::{Recorder,Settings,Filter,init,scope,settings,provider_enabled,failure,committed,assembly,summary,validation,read,status,context};
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
/// Existing scoped Storage codec and session existence, without routing or new resources.
pub trait SnapshotStore:Clone+Send+Sync+'static {
 fn current_user_id(&self)->&str;
 fn seal(&self,value:&Value)->Result<Vec<u8>>;
 fn unseal(&self,value:&[u8])->Result<Value>;
 fn session_exists<'a>(&'a self,sid:&'a str)->impl Future<Output=Result<bool,Error>>+Send+'a;
}
#[derive(Debug)]pub struct Error(pub axum::http::StatusCode,pub String);
impl Error {pub fn bad(message:&str)->Self {Self(axum::http::StatusCode::BAD_REQUEST,message.into())}}
impl From<anyhow::Error> for Error {fn from(_:anyhow::Error)->Self {
 tracing::error!("Request storage or execution operation failed");
 Self(axum::http::StatusCode::INTERNAL_SERVER_ERROR,"Operation failed; check development logs".into())
}}
pub(crate) fn now()->u64 {std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()}
pub(crate) fn digest<T:serde::Serialize>(value:&T)->String {use sha2::Digest;hex::encode(sha2::Sha256::digest(serde_json::to_vec(value).unwrap()))}
#[cfg(any(test,feature="test-support"))]
pub mod test_support {
 pub use crate::config::{self,Config};
 pub use crate::recorder::{Settings,Capture,identifier,permits,event_allowed,snapshot_allowed,insert_snapshot,prune,seed,open,measure,assembled_instructions,transition,read_snapshot,test_pool,test_write_errors};
}
#[cfg(test)]mod tests;
