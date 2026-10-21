//! Root supplies the existing owner views and shared handles to Manager scheduling.
use super::*;
pub(crate) async fn scheduler(app:App) {webagent_manager::scheduler::scheduler(bridge::runtime(&app),bridge::Owners(app)).await}
#[cfg(test)]pub(super) async fn process(app:App,id:String)->Result<()> {webagent_manager::scheduler::process(bridge::runtime(&app),id).await}
