//! Base: the built-in core tools every agent has. Their handlers, schemas and role
//! defaults are compiled in; no `plugins/abilities` folder controls them. Capability
//! levels, saved tool permissions, account tiers and installation policy still apply.
use webagent_access::Permission;

pub const ID: &str = "base";
pub const NAME: &str = "Base";
pub const DESCRIPTION: &str = "Core conversation tools for bounded utilities, caller-owned session search, context compaction and recall, prompt and skill edits, task checkpoints, and declarative presentation. Remaining declarations need separate V3 contracts.";
pub const MIGRATION_NOTE: &str = "Native V3 Base tools include utilities, discovery, skills, private profiles and ratings, task artifacts, presentations, context controls, attributed session messaging, bounded saved functions, protected account/connection setup and private webhook inboxes. Live policy and exact owning API authority still apply.";
pub const ICON: &str = "box";
pub const COLOR: &str = "#7dcfff";

/// Tool names, in catalog order.
pub const TOOLS: &[&str] = &[
    "search_tools",
    "list_tools",
    "load_tool",
    "list_abilities",
    "load_ability",
    "list_skills",
    "load_skill",
    "context_assets",
    "set_execution_mode",
    "get_time",
    "get_date",
    "calculate",
    "read_attachment",
    "session_search",
    "message_other_session",
    "start_session",
    "similar_sessions",
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
    "task_status",
    "task_wait",
    "task_cancel",
    "present_plan",
    "present_chat_component",
    "manage_agent_profiles",
    "rate_skill",
    "request_agent_login",
    "run_soft_ability",
    "main_notes_read",
    "main_notes_write",
    #[cfg(feature = "test-support")]
    "test_owner_purchase",
];

pub const SKILL: &str = include_str!("../base/base.skill.md");

/// Usage guidance per tool, served like a tool folder's `tool.skill.md`.
pub const TOOL_SKILLS: &[(&str, &str)] = &[
    ("calculate", include_str!("../base/tools/calculate.skill.md")),
    ("context_assets", include_str!("../base/tools/context_assets.skill.md")),
    ("get_date", include_str!("../base/tools/get_date.skill.md")),
    ("get_time", include_str!("../base/tools/get_time.skill.md")),
    ("list_skills", include_str!("../base/tools/list_skills.skill.md")),
    ("list_tools", include_str!("../base/tools/list_tools.skill.md")),
    ("load_skill", include_str!("../base/tools/load_skill.skill.md")),
    ("load_tool", include_str!("../base/tools/load_tool.skill.md")),
    ("main_notes_read", include_str!("../base/tools/main_notes_read.skill.md")),
    ("main_notes_write", include_str!("../base/tools/main_notes_write.skill.md")),
    ("read_attachment", include_str!("../base/tools/read_attachment.skill.md")),
    ("read_own_prompt", include_str!("../base/tools/read_own_prompt.skill.md")),
    ("search_this_session", include_str!("../base/tools/search_this_session.skill.md")),
    ("search_tools", include_str!("../base/tools/search_tools.skill.md")),
    ("task_cancel", include_str!("../base/tools/task_cancel.skill.md")),
    ("task_status", include_str!("../base/tools/task_status.skill.md")),
    ("task_wait", include_str!("../base/tools/task_wait.skill.md")),
];

pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Coded role defaults: Operators get every Base tool; Members and Visitors may ask to
/// use read tools and get no write tools. A custom level takes the next role down.
pub fn default_access(rungs: &[String], is_read: bool) -> Permission {
    match rungs.iter().find(|rung| crate::tool_definitions::ROLES.contains(&rung.as_str())).map(String::as_str) {
        Some("operator") => Permission::Auto,
        Some(_) if is_read => Permission::Ask,
        _ => Permission::Deny,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_defaults_and_skills_name_real_tools() {
        let rungs = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(default_access(&rungs(&["operator", "member", "visitor"]), false), Permission::Auto);
        assert_eq!(default_access(&rungs(&["trusted", "member", "visitor"]), true), Permission::Ask);
        assert_eq!(default_access(&rungs(&["visitor"]), false), Permission::Deny);
        assert!(TOOL_SKILLS.iter().all(|(tool, text)| owns(tool) && !text.trim().is_empty()));
    }
}
