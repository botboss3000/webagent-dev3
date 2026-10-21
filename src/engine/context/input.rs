//! Root compatibility names for Engine borrowed context inputs.
pub use webagent_engine::context::input::*;

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
