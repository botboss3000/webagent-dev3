//! The Administrator native tools. Each has a compiled handler and a tool folder under
//! `plugins/abilities/Administrator/<ability>/tools/<tool>/`.

/// Every Administrator native tool.
pub const TOOLS: [&str; 50] = [
    "code_index_scan",
    "code_index_lookup",
    "code_index_coverage",
    "code_index_trace",
    "code_index_progress",
    "code_index_maintenance",
    "git_tool",
    "resolve_conflict",
    "commit_and_push",
    "repository_access",
    "source_remote_read",
    "source_remote_commit",
    "source_site_create",
    "source_site_status",
    "source_preview_start",
    "source_preview_stop",
    "source_publish",
    "source_rollback",
    "terminal_open",
    "terminal_read",
    "terminal_send",
    "terminal_wait",
    "terminal_list",
    "terminal_close",
    "v3_workspace_status",
    "v3_workspace_action",
    "v3_workspace_receipt",
    "v3_cargo_task",
    "v3_build_work",
    "v3_source_list",
    "v3_source_read",
    "v3_source_prepare",
    "v3_source_apply_prepared",
    "v3_source_apply",
    "v3_source_receipt",
    "v3_build_status",
    "v3_build_worker_restart",
    "v3_build_image",
    "v3_worker_submit",
    "v3_worker_job",
    "read_diagnostics",
    "login_as_current_user",
    "login_as_current_user_v3",
    "dev_browser_url",
    "dev_browser_action",
    "dev_browser_record",
    "start_tunnel",
    "get_tunnel_url",
    "build_work",
    "performance_work",
];

/// Codebase Admin package tools whose live-checkout fallback this module also routes.
pub const CHECKOUT_TOOLS: [&str; 6] = [
    "file_list",
    "file_read",
    "file_search",
    "file_edit",
    "file_write",
    "checkout_edit",
];

/// Broker requests whose admission pins a renewal record, so the host can keep or revoke
/// the job's lease while it runs.
#[must_use]
pub fn renews(name: &str, action: Option<&str>) -> bool {
    matches!(name, "v3_build_image" | "v3_worker_submit")
        || (matches!(name, "v3_build_work" | "build_work")
            && matches!(action, Some("finish" | "decide" | "check")))
}

/// The authority a broker request is fingerprinted under: finishing or deciding work
/// needs deployment authority, not just build authority.
#[must_use]
pub fn authority_name<'a>(name: &'a str, action: Option<&str>) -> &'a str {
    match (name, action) {
        ("build_work", Some("finish" | "decide")) => "build_work_finish",
        ("v3_build_work", Some("finish" | "decide")) => "v3_build_work_finish",
        _ => name,
    }
}

/// Whether `name` is an Administrator native tool.
#[must_use]
pub fn supported(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Whether this module executes `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    CHECKOUT_TOOLS.contains(&name) || supported(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_disjoint_from_checkout_tools() {
        let unique: std::collections::BTreeSet<_> = TOOLS.iter().collect();
        assert_eq!(unique.len(), TOOLS.len(), "duplicate Administrator tool");
        assert!(
            CHECKOUT_TOOLS.iter().all(|name| !supported(name)),
            "checkout tools are package tools, not native schemas"
        );
        assert!(owns("checkout_edit") && owns("v3_build_work") && !owns("web_search"));
    }

    #[test]
    fn finishing_work_needs_deployment_authority() {
        assert_eq!(
            authority_name("build_work", Some("finish")),
            "build_work_finish"
        );
        assert_eq!(
            authority_name("v3_build_work", Some("decide")),
            "v3_build_work_finish"
        );
        assert_eq!(authority_name("build_work", Some("check")), "build_work");
        assert!(renews("build_work", Some("check")) && renews("v3_worker_submit", None));
        assert!(!renews("build_work", Some("status")) && !renews("v3_source_read", None));
    }
}
