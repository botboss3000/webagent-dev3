//! Base: the built-in core tools every agent has. Their handlers, schemas and role
//! defaults are compiled in; no `plugins/abilities` folder controls them. Capability
//! levels, saved tool permissions, account tiers and installation policy still apply.
use webagent_access::Permission;

pub const ID: &str = "base";
pub const NAME: &str = "Base";
pub const DESCRIPTION: &str = "Core conversation tools for bounded utilities, caller-owned session search, context compaction and recall, prompt and skill edits, task checkpoints, declarative presentation and the caller-private credential vault. Remaining declarations need separate V3 contracts.";
pub const MIGRATION_NOTE: &str = "Native V3 Base tools include utilities, discovery, skills, private profiles and ratings, task artifacts, presentations, context controls, bounded saved functions, protected account/connection setup, the caller-private credential vault and private webhook inboxes. Live policy and exact owning API authority still apply.";
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
    "request_credential",
    "check_credential",
    "list_vault_keys",
    #[cfg(feature = "test-support")]
    "test_owner_purchase",
];

pub const SKILL: &str = include_str!("../base/base.skill.md");

/// Usage guidance per tool, served like a tool folder's `tool.skill.md`.
pub const TOOL_SKILLS: &[(&str, &str)] = &[
    (
        "calculate",
        include_str!("../base/tools/calculate.skill.md"),
    ),
    (
        "context_assets",
        include_str!("../base/tools/context_assets.skill.md"),
    ),
    ("get_date", include_str!("../base/tools/get_date.skill.md")),
    ("get_time", include_str!("../base/tools/get_time.skill.md")),
    (
        "list_skills",
        include_str!("../base/tools/list_skills.skill.md"),
    ),
    (
        "list_tools",
        include_str!("../base/tools/list_tools.skill.md"),
    ),
    (
        "load_skill",
        include_str!("../base/tools/load_skill.skill.md"),
    ),
    (
        "load_tool",
        include_str!("../base/tools/load_tool.skill.md"),
    ),
    (
        "main_notes_read",
        include_str!("../base/tools/main_notes_read.skill.md"),
    ),
    (
        "main_notes_write",
        include_str!("../base/tools/main_notes_write.skill.md"),
    ),
    (
        "read_attachment",
        include_str!("../base/tools/read_attachment.skill.md"),
    ),
    (
        "read_own_prompt",
        include_str!("../base/tools/read_own_prompt.skill.md"),
    ),
    (
        "search_this_session",
        include_str!("../base/tools/search_this_session.skill.md"),
    ),
    (
        "search_tools",
        include_str!("../base/tools/search_tools.skill.md"),
    ),
    (
        "background_cancel",
        include_str!("../base/tools/background_cancel.skill.md"),
    ),
    (
        "background_status",
        include_str!("../base/tools/background_status.skill.md"),
    ),
];

pub fn owns(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Role defaults (operator, member, visitor) that differ from the coded rule: the
/// credential vault keeps the defaults it had as a Visualizer tool, so private setup
/// stays open to Members and its metadata reads to Visitors.
const ROLE_DEFAULTS: &[(&str, [Permission; 3])] = &[
    (
        "request_credential",
        [Permission::Auto, Permission::Auto, Permission::Ask],
    ),
    (
        "check_credential",
        [Permission::Auto, Permission::Auto, Permission::Auto],
    ),
    (
        "list_vault_keys",
        [Permission::Auto, Permission::Auto, Permission::Auto],
    ),
];

/// Coded role defaults: Operators get every Base tool; Members and Visitors may ask to
/// use read tools and get no write tools, unless `ROLE_DEFAULTS` names the tool. A custom
/// level takes the next role down.
pub fn default_access(name: &str, rungs: &[String], is_read: bool) -> Permission {
    let role = rungs.iter().find_map(|rung| {
        crate::tool_definitions::ROLES
            .iter()
            .position(|role| role == rung)
    });
    if let Some((_, defaults)) = ROLE_DEFAULTS.iter().find(|(tool, _)| *tool == name) {
        return role.map_or(Permission::Deny, |index| defaults[index]);
    }
    match role {
        Some(0) => Permission::Auto,
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
        assert_eq!(
            default_access(
                "get_time",
                &rungs(&["operator", "member", "visitor"]),
                false
            ),
            Permission::Auto
        );
        assert_eq!(
            default_access("get_time", &rungs(&["trusted", "member", "visitor"]), true),
            Permission::Ask
        );
        assert_eq!(
            default_access("get_time", &rungs(&["visitor"]), false),
            Permission::Deny
        );
        // The vault keeps its earlier tool-folder defaults.
        assert_eq!(
            default_access("request_credential", &rungs(&["trusted", "member"]), false),
            Permission::Auto
        );
        assert_eq!(
            default_access("request_credential", &rungs(&["visitor"]), false),
            Permission::Ask
        );
        assert_eq!(
            default_access("list_vault_keys", &rungs(&["visitor"]), true),
            Permission::Auto
        );
        assert_eq!(
            default_access("check_credential", &rungs(&["trusted"]), true),
            Permission::Deny
        );
        assert!(ROLE_DEFAULTS.iter().all(|(tool, _)| owns(tool)));
        assert!(TOOL_SKILLS
            .iter()
            .all(|(tool, text)| owns(tool) && !text.trim().is_empty()));
    }
}
