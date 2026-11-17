//! Knowledge file tests for helpers shared across the tool family.
use super::*;
#[test]
fn file_ids_are_bounded_hex() {
    assert!(file_id(&json!("../x")).is_err());
    assert!(file_id(&json!("")).is_err());
    assert_eq!(file_id(&json!("ab12")).unwrap(), "ab12");
}
