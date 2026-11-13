//! Where the app's folders are. Each root is an environment variable whose default is
//! the container layout, so Docker needs no settings and an install outside a
//! container sets only the roots that differ. Read on every call (no caching), so
//! tests and child processes see the environment they were given.
use std::path::PathBuf;

fn root(name: &str, default: &str) -> PathBuf {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(default), PathBuf::from)
}

/// The source checkout (`V3_CHECKOUT_ROOT`, default `/code`).
#[must_use]
pub fn checkout() -> PathBuf {
    root("V3_CHECKOUT_ROOT", "/code")
}

/// Application data (`V3_STATE_ROOT`, default `/state`).
#[must_use]
pub fn state() -> PathBuf {
    root("V3_STATE_ROOT", "/state")
}

/// Sockets, locks and other per-boot files (`V3_RUNTIME_ROOT`, default `/runtime`).
#[must_use]
pub fn runtime() -> PathBuf {
    root("V3_RUNTIME_ROOT", "/runtime")
}

/// Per-session workspaces (`V3_WORKSPACE_ROOT`, default `/workspaces`).
#[must_use]
pub fn workspaces() -> PathBuf {
    root("V3_WORKSPACE_ROOT", "/workspaces")
}

/// The installed release: UI, ability folders and build assets (`V3_APP_ROOT`, default
/// `/opt/v3`).
#[must_use]
pub fn app() -> PathBuf {
    root("V3_APP_ROOT", "/opt/v3")
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn unset_roots_keep_the_container_layout() {
        assert_eq!(
            super::root("V3_TEST_UNSET_PATHS_ROOT", "/code"),
            Path::new("/code")
        );
    }
}
