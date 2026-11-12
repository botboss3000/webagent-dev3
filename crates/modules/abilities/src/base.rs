//! Base: the built-in core tools every agent has. Their handlers, schemas and role
//! defaults are compiled in; `plugins/abilities/Core/base` holds only their guidance. Capability
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

/// Tool skill files under `plugins/abilities/Core/base/tools/`, as (tool, file stem).
/// One skill covers the three session-script tools.
pub const TOOL_SKILLS: &[(&str, &str)] = &[
    ("calculate", "calculate"),
    ("context_assets", "context_assets"),
    ("get_date", "get_date"),
    ("get_time", "get_time"),
    ("list_skills", "list_skills"),
    ("list_tools", "list_tools"),
    ("load_skill", "load_skill"),
    ("load_tool", "load_tool"),
    ("main_notes_read", "main_notes_read"),
    ("main_notes_write", "main_notes_write"),
    ("script_save", "session_scripts"),
    ("script_list", "session_scripts"),
    ("script_run", "session_scripts"),
    ("read_attachment", "read_attachment"),
    ("read_own_prompt", "read_own_prompt"),
    ("search_this_session", "search_this_session"),
    ("background_cancel", "background_cancel"),
    ("background_status", "background_status"),
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
