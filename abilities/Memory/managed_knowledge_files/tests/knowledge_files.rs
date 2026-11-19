//! Knowledge file tests for helpers shared across the tool family.
use super::*;
#[test]
fn file_ids_are_bounded_hex() {
    assert!(file_id("../x").is_err());
    assert!(file_id("").is_err());
    assert_eq!(file_id("ab12").unwrap(), "ab12");
}
