//! Human escalation tests across the tool family.
use super::*;
#[test]
fn schemas_match_owned_tools() {
    let names: Vec<_> = crate::schemas(TOOLS)
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names.len(), 4);
    assert!(names.iter().all(|n| owns(n)));
}
