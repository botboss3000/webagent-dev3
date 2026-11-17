//! Self-edit argument tests spanning the edit, save and remove tools.
use super::edit_own_prompt::PromptArgs;
use super::remove_own_skill::RemoveArgs;
use super::save_own_skill::SaveArgs;
use super::*;
#[test]
fn selectors_cannot_target_other_agents_or_skip_revisions() {
    assert!(
        serde_json::from_value::<PromptArgs>(json!({"slot_name":"style",
        "content":"brief","expected_revision":1,"agent_id":"foreign"}))
        .is_err()
    );
    assert!(serde_json::from_value::<SaveArgs>(json!({"name":"how_to",
        "expected_revision":0,"user_id":"foreign"}))
    .is_err());
    assert!(serde_json::from_value::<RemoveArgs>(json!({"name":"how_to",
        "expected_revision":1,"ability_id":"foreign"}))
    .is_err());
}
