//! Base: the built-in core tools every agent has. Their handlers are compiled in; each
//! tool's schema, effect, policy ceiling, role defaults and skill live in its
//! `abilities/Core/base/tools/<tool>/` folder. Capability levels, saved tool
//! permissions, account tiers and installation policy still apply.

pub const ID: &str = "base";
pub const NAME: &str = "Base";
pub const DESCRIPTION: &str = "Core conversation tools for bounded utilities, caller-owned session search, context compaction and recall, prompt and skill edits, task checkpoints, declarative presentation and the caller-private credential vault. Remaining declarations need separate V3 contracts.";
pub const MIGRATION_NOTE: &str = "Native V3 Base tools include utilities, discovery, skills, private profiles and ratings, task artifacts, presentations, context controls, bounded saved functions, protected account/connection setup, the caller-private credential vault and private webhook inboxes. Live policy and exact owning API authority still apply.";
pub const ICON: &str = "box";
pub const COLOR: &str = "#7dcfff";

/// Tool names, in catalog order.
pub const TOOLS: &[&str] = &[
    "list_tools",
    "load_tool",
    "list_skills",
    "load_skill",
    "context_assets",
    "set_execution_mode",
    "get_time",
    "get_date",
    "calculate",
    "read_attachment",
    "compact_context",
    "recall_compacted",
    "search_this_session",
    "read_own_prompt",
    "edit_own_prompt",
    "save_own_skill",
    "remove_own_skill",
    "register_user",
    "register_webhook",
    "list_webhooks",
    "delete_webhook",
    "get_webhook_log",
    "update_task_checkpoint",
    "get_task_checkpoint",
    "background_status",
    "background_cancel",
    "present_plan",
    "present_chat_component",
    "manage_agent_profiles",
    "rate_skill",
    "request_agent_login",
    "main_notes_read",
    "main_notes_write",
    "script_save",
    "script_list",
    "script_run",
    "request_credential",
    "check_credential",
    "list_vault_keys",
    #[cfg(feature = "test-support")]
    "test_owner_purchase",
];

/// Base tools defined by a tool folder: every tool except the test-only fixture, whose
/// owner-authorization schema stays compiled.
pub const FOLDER_TOOLS: &[&str] = TOOLS.split_at(TOOLS.len() - TEST_FIXTURES).0;
#[cfg(feature = "test-support")]
const TEST_FIXTURES: usize = 1;
#[cfg(not(feature = "test-support"))]
const TEST_FIXTURES: usize = 0;

pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_folder_tool_is_a_base_tool() {
        assert!(FOLDER_TOOLS.iter().all(|tool| owns(tool)));
        assert!(!FOLDER_TOOLS.contains(&"test_owner_purchase"));
    }
}
