//! Tests for the Wiki argument validators shared by several tools.
use super::*;
#[test]
fn arguments_are_validated() {
    assert!(status_arg(&json!("public")).is_err());
    assert!(tags_arg(&json!(vec!["a"; 21])).is_err());
    assert!(keywords_arg(&json!(["x".repeat(61)])).is_err());
    assert!(title_arg(&json!("")).is_err());
    assert!(body_arg(&json!("x".repeat(BODY_MAX_BYTES + 1))).is_err());
}
