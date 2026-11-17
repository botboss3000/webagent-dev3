//! Tests for base_extras' identifier check shared by several account tools.
use super::*;
#[test]
fn private_record_identifiers_cannot_be_paths_or_query_strings() {
    for value in ["../x", "x/y", "x?user=other", ""] {
        assert!(identifier(Some(value), 64).is_err());
    }
    assert!(identifier(Some("profile-1"), 64).is_ok());
}
