//! The route table covers every registered route, and its widget entries stay inside the
//! chat. The routes are read from the router sources: axum cannot list a built router.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Where each router's routes are mounted. A region of a file starts at its marker and runs to
/// the next marker of the same file. `None` = not served over HTTP (the native Core tools
/// router, and the messaging layer's own region in `mod.rs`, which has no routes of its own).
/// `signed` = behind the identity middleware (`auth`).
const MOUNTS: &[(&str, &str, Option<&str>, bool)] = &[
    (
        "src/api/mod.rs",
        "fn host_admin_routes",
        Some("/api/v3"),
        true,
    ),
    ("src/api/mod.rs", "let event_aliases", Some(""), true),
    ("src/api/mod.rs", "let messaging", None, false),
    ("src/api/mod.rs", "let admin_api", Some("/api/v3"), true),
    ("src/api/mod.rs", "let ui =", Some(""), false),
    (
        "src/api/mod.rs",
        "pub(crate) fn core_tools_router",
        None,
        false,
    ),
    (
        "src/api/user.rs",
        "pub(crate) fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "src/api/user.rs",
        "pub(crate) fn messaging_router()",
        Some("/api/v3/admin/messaging"),
        false,
    ),
    (
        "src/api/administrator.rs",
        "fn session_router",
        Some("/api/v3"),
        true,
    ),
    (
        "src/api/app_defaults.rs",
        "fn admin_router",
        Some("/api/v3"),
        true,
    ),
    (
        "src/api/build_dashboard.rs",
        "fn admin_router",
        Some("/api/v3"),
        true,
    ),
    (
        "src/api/agent_members.rs",
        "pub fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "src/api/system.rs",
        "pub(crate) fn router()",
        Some("/api/v3/admin/system"),
        true,
    ),
    (
        "src/api/service_admin.rs",
        "pub(crate) fn router()",
        Some("/api/v3/admin/services"),
        true,
    ),
    (
        "src/diagnostics_plugin/mod.rs",
        "pub(crate) fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "src/diagnostics_plugin/ui_trace.rs",
        "fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "src/diagnostics_plugin/background.rs",
        "fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "crates/modules/chat/host/api/shared_chat.rs",
        "pub fn router()",
        Some("/api/v3"),
        true,
    ),
    (
        "crates/modules/access/host/api.rs",
        "pub fn router()",
        Some("/api/v3/admin/access"),
        true,
    ),
    (
        "crates/modules/administration/host/api/supply_chain.rs",
        "fn router(",
        Some("/api/v3"),
        true,
    ),
    (
        "crates/modules/agents/host/api.rs",
        "pub fn router()",
        Some("/api/v3/models"),
        true,
    ),
    (
        "crates/modules/agents/host/api.rs",
        "pub fn admin_router()",
        Some("/api/v3/admin/access"),
        true,
    ),
    (
        "crates/modules/models/host/api/selection.rs",
        "pub fn router()",
        Some("/api/v3/models"),
        true,
    ),
    (
        "crates/modules/models/host/api/platform_catalog.rs",
        "pub fn router()",
        Some("/api/v3/admin/models/catalog"),
        true,
    ),
    (
        "crates/modules/webapp/src/pages.rs",
        "pub fn routes",
        Some(""),
        false,
    ),
];

/// The source without its `#[cfg(test)] mod … { … }` blocks (test servers register routes too).
fn without_tests(mut source: String) -> String {
    while let Some(start) = source.find("#[cfg(test)]\nmod ") {
        let Some(open) = source[start..].find('{').map(|i| start + i) else {
            break;
        };
        let mut depth = 0;
        let mut end = source.len();
        for (i, c) in source[open..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        source.replace_range(start..end, "");
    }
    source
}

/// Each `.route("pattern", …)` in `text`: the pattern and the methods named in its call.
fn routes_in(text: &str) -> Vec<(String, BTreeSet<&'static str>)> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find(".route(") {
        rest = &rest[at + ".route(".len()..];
        let Some(literal) = rest.trim_start().strip_prefix('"') else {
            continue;
        };
        let Some(close) = literal.find('"') else {
            break;
        };
        let pattern = literal[..close].to_owned();
        let call = &literal[close + 1..];
        let mut depth = 0;
        let mut end = call.len();
        for (i, c) in call.char_indices() {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    end = i;
                    break;
                }
                ')' => depth -= 1,
                _ => {}
            }
        }
        let call = &call[..end];
        let mut methods = BTreeSet::new();
        for (name, method) in [
            ("get(", "GET"),
            ("post(", "POST"),
            ("put(", "PUT"),
            ("patch(", "PATCH"),
            ("delete(", "DELETE"),
        ] {
            let mut search = call;
            while let Some(i) = search.find(name) {
                let before = search[..i].chars().last();
                if before.is_none_or(|c| !(c.is_alphanumeric() || c == '_')) {
                    methods.insert(method);
                }
                search = &search[i + name.len()..];
            }
        }
        found.push((pattern, methods));
    }
    found
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name != "target") {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Every registered route: full pattern → (methods, behind the identity middleware).
pub(crate) fn registered() -> BTreeMap<String, (BTreeSet<&'static str>, bool)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut routes: BTreeMap<String, (BTreeSet<&'static str>, bool)> = BTreeMap::new();
    let files: BTreeSet<&str> = MOUNTS.iter().map(|mount| mount.0).collect();
    for file in &files {
        let source = without_tests(std::fs::read_to_string(root.join(file)).unwrap());
        let mut regions: Vec<_> = MOUNTS
            .iter()
            .filter(|mount| mount.0 == *file)
            .map(|mount| {
                let start = source
                    .find(mount.1)
                    .unwrap_or_else(|| panic!("{file}: marker `{}` moved; update MOUNTS", mount.1));
                (start, mount.2, mount.3)
            })
            .collect();
        regions.sort_by_key(|region| region.0);
        for (i, (start, prefix, signed)) in regions.iter().enumerate() {
            let end = regions.get(i + 1).map_or(source.len(), |next| next.0);
            let found = routes_in(&source[*start..end]);
            let Some(prefix) = prefix else {
                continue;
            };
            for (pattern, methods) in found {
                let full = if pattern.starts_with("/api/v3") {
                    pattern
                } else if pattern == "/" && !prefix.is_empty() {
                    (*prefix).to_owned()
                } else {
                    format!("{prefix}{pattern}")
                };
                let entry = routes
                    .entry(full)
                    .or_insert_with(|| (BTreeSet::new(), *signed));
                entry.0.extend(methods);
                entry.1 |= *signed;
            }
        }
    }
    // A router in any other file must be mounted above (or be a test server).
    let mut sources = Vec::new();
    rust_files(&root.join("src"), &mut sources);
    rust_files(&root.join("crates"), &mut sources);
    for path in sources {
        let relative = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if files.contains(relative.as_str()) || relative.ends_with("tests.rs") {
            continue;
        }
        let source = without_tests(std::fs::read_to_string(&path).unwrap());
        // Model transports and test helpers stand up fake provider servers, never app routes.
        let fake_servers = relative.starts_with("crates/modules/models/src/transport/");
        assert!(
            fake_servers || routes_in(&source).is_empty(),
            "{relative} registers routes: add it to MOUNTS and classify its routes in ROUTES"
        );
    }
    routes
}

#[test]
fn every_route_is_classified() {
    let registered = registered();
    assert!(
        registered.len() > 250,
        "the source scan found too few routes"
    );
    let table: BTreeMap<&str, &Route> = ROUTES.iter().map(|route| (route.path, route)).collect();
    assert_eq!(table.len(), ROUTES.len(), "a route has two rows");
    let unclassified: Vec<_> = registered
        .keys()
        .filter(|path| !table.contains_key(path.as_str()))
        .collect();
    assert!(
        unclassified.is_empty(),
        "decide which credential kinds may call these routes (widget methods, agent-browser \
         read) and add a row to ROUTES: {unclassified:?}"
    );
    let stale: Vec<_> = table
        .keys()
        .filter(|path| !registered.contains_key(**path))
        .collect();
    assert!(stale.is_empty(), "ROUTES rows with no route: {stale:?}");
    for (path, (methods, signed)) in &registered {
        let route = table[path.as_str()];
        assert_eq!(
            route.public, !signed,
            "{path}: public flag differs from where it is mounted"
        );
        for method in route.widget.split_whitespace() {
            assert!(
                methods.contains(method),
                "{path}: {method} is not registered"
            );
        }
        if route.agent_browser {
            assert!(methods.contains("GET"), "{path}: agent browsers only read");
        }
    }
}

/// The widget list keeps to the chat: no family that is never for widgets can slip in.
#[test]
fn the_widget_allowlist_stays_inside_the_chat() {
    let allowed: Vec<_> = ROUTES
        .iter()
        .filter(|route| !route.widget.is_empty())
        .collect();
    assert_eq!(
        allowed.len(),
        30,
        "a change to the widget allowlist needs a decision"
    );
    for route in allowed {
        let path = route.path;
        assert!(
            path.starts_with("/api/v3/sessions")
                || path.starts_with("/api/v3/runs/")
                || path.starts_with("/api/v3/approvals/")
                || path == "/api/v3/user/status"
                || path == "/api/v3/agents/{id}/access-summary"
                || path == "/api/v3/wiki/articles/{slug}",
            "{path}"
        );
        for refused in [
            "admin",
            "administrator",
            "settings",
            "mode",
            "model",
            "abilities",
            "compact",
            "fork",
            "undo",
            "archive",
            "unarchive",
            "build-tasks",
            "builds",
            "background-tasks",
            "messaging-routes",
            "invitations",
            "participation",
            "ui-preview",
            "feedback",
            "file-manager",
            "host",
            "storage",
            "promotions",
            "source-review",
            "state-inspect",
            "context-remedy",
        ] {
            assert!(
                !path.split('/').any(|part| part == refused),
                "{path} has the segment `{refused}`"
            );
        }
    }
    assert!(!widget_may(&Method::POST, Some("/api/v3/feedback")));
    assert!(!widget_may(&Method::GET, None));
    assert!(widget_may(&Method::HEAD, Some("/api/v3/sessions")));
    assert!(!widget_may(&Method::PATCH, Some("/api/v3/sessions/{id}")));
}

/// Agent browsers never read administrator routes or stored sign-ins and credentials.
#[test]
fn agent_browsers_do_not_read_credentials_or_administrator_routes() {
    for route in ROUTES.iter().filter(|route| route.agent_browser) {
        let path = route.path;
        assert!(
            !path.starts_with("/admin/")
                && !path.starts_with("/api/v3/admin/")
                && !path.contains("/administrator/")
                && !path.contains("credential")
                && !path.ends_with("/embed"),
            "{path}"
        );
    }
    for path in [
        "/api/v3/auth/sessions",
        "/api/v3/browser/sites",
        "/api/v3/browser/devices",
        "/api/v3/core/webhooks/{id}/setup",
        "/api/v3/status",
    ] {
        assert!(!agent_browser_may_read(Some(path)), "{path}");
    }
    assert!(agent_browser_may_read(Some("/api/v3/sessions/{id}")));
    assert!(!agent_browser_may_read(None));
}
