//! Core borrowed request adapter; trusted contribution selection lives in the library.
use crate::compaction as context_compaction;
use webagent_models::provider;
use serde_json::Value;
pub use webagent_context_contracts::context_input::{CallIdentity,Contribution,Lifetime,select};
/// Borrowed call ingredients plus resolved model and budget policies.
/// This is not an assembled payload: history and schemas stay borrowed, and
/// assembly combines instructions only after contribution selection succeeds.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub history: &'a [Value],
    pub base: &'a str,
    pub guidance: &'a str,
    pub schemas: &'a [Value],
    /// Only native, trusted producers can supply instruction contributions.
    pub contributions: &'a [Contribution],
    pub identity: CallIdentity,
    pub selection: &'a provider::ProviderSelection,
    pub max_bytes: usize,
    pub compaction: &'a context_compaction::Config,
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn selection_is_borrowed_and_does_not_reinject_history() {
  let c=Contribution{id:"a".into(),source:"native".into(),lifetime:Lifetime::Call,content:"private".into(),turn:1,call:2};
  let items=[c.clone(),c.clone(),Contribution{lifetime:Lifetime::History,..c.clone()},Contribution{call:3,id:"old".into(),..c}];
  let selected=select(&items,CallIdentity{turn:1,call:2}).unwrap();
  assert_eq!(selected.iter().map(|x|x.disposition).collect::<Vec<_>>(),vec!["included","duplicate","already_in_history","expired"]);
  assert!(std::ptr::eq(selected[0].item,&items[0]));
 }
 #[test] fn invalid_expired_input_is_rejected() {
  let c=Contribution{id:"".into(),source:"native".into(),lifetime:Lifetime::Call,content:"x".into(),turn:9,call:9};
  assert!(select(&[c],CallIdentity{turn:1,call:1}).is_err());
 }
}
