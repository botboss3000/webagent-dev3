//! Root supplies existing registered-agent selection and configuration owner operations.
use anyhow::Result;
pub use webagent_manager::configuration::IncomingMessage;
pub async fn incoming(store:&crate::store::Store,agent:&str)->Result<IncomingMessage> {
 webagent_manager::settings::incoming(&store.manager_repository(),agent).await
}
pub async fn snapshot(store:&crate::store::Store,agent:&str)->Result<(IncomingMessage,u64)> {
 webagent_manager::settings::snapshot(&store.manager_repository(),agent).await
}
