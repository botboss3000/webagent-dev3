//! Tests for the Wiki argument validators shared by several tools.
use super::*;
#[test]
fn arguments_are_validated() {
    assert!(status_arg("public").is_err());
    assert!(tags_arg(&vec!["a".to_owned(); 21]).is_err());
    assert!(keywords_arg(&["x".repeat(61)]).is_err());
    assert!(title_arg("").is_err());
    assert!(body_arg(&"x".repeat(BODY_MAX_BYTES + 1)).is_err());
}
