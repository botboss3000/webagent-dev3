//! Root composition for the independently compiled Manager production package.
use crate::*;
#[path="configuration.rs"]pub mod config;
mod bridge;
mod review;
mod scheduler;
#[cfg(test)]mod tests;
pub(crate) use scheduler::scheduler;
pub(crate) use webagent_manager::Verdict;
pub(crate) async fn prepare(app:&App,agent:&str)->Result<Option<Value>> {bridge::runtime(app).prepare(agent).await}
pub(crate) fn public(record:&Value)->Value {webagent_manager::public(record,crate::usage::public)}
pub(crate) async fn decorate(app:&App,messages:&mut[Value])->Result<()> {app.store.manager_repository().decorate(messages).await}
pub(crate) async fn judge_followup(app:&App,record:&mut Value)->Result<Verdict> {review::judge(app,record).await}
#[cfg(test)]
fn system_proxy(app:&App,record:&Value)->Value {bridge::runtime(app).system_proxy(record)}
