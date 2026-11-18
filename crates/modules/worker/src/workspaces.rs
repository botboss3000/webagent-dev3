//! Orphaned session workspaces: the startup sweep removes the workspace of every session
//! that no longer exists, as a session delete does for its own. Folders that are not a
//! session's (the administrator and live-configuration workspaces), recent folders and
//! folders still in use stay.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Workspaces of fixed internal callers, never a chat session's: administrator, dashboard
/// and File Manager work, and live-configuration edits (`src/live_files.rs`).
const PROTECTED: [&str; 2] = [
    "00000000-0000-0000-0000-000000000000",
    "5f1c0a3e-7d2b-4c1e-9a6f-3b8d2e4c7a10",
];
/// A folder changed this recently may belong to a session created during the sweep.
pub const MIN_AGE: Duration = Duration::from_secs(24 * 3600);

/// The session workspaces under `root` to remove: canonical UUID folders (never symlinks)
/// that `keep` does not list, are not protected and have not changed for `min_age`.
#[must_use]
pub fn orphans(root: &Path, keep: &[String], min_age: Duration) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_str()?.to_owned();
            let canonical = uuid::Uuid::parse_str(&name)
                .ok()
                .is_some_and(|id| id.hyphenated().to_string() == name);
            if !canonical || PROTECTED.contains(&name.as_str()) || keep.contains(&name) {
                return None;
            }
            let meta = std::fs::symlink_metadata(entry.path()).ok()?;
            let age = now
                .duration_since(meta.modified().ok()?)
                .unwrap_or_default();
            (meta.is_dir() && age >= min_age).then(|| entry.path())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_old_unlisted_session_folders_are_orphans() {
        let dir = tempfile::tempdir().expect("dir");
        let kept = "11111111-1111-4111-8111-111111111111";
        let gone = "22222222-2222-4222-8222-222222222222";
        for name in [
            kept,
            gone,
            PROTECTED[0],
            PROTECTED[1],
            "notes",
            "ABCDEF00-0000-4000-8000-000000000000",
        ] {
            std::fs::create_dir(dir.path().join(name)).expect("folder");
        }
        std::os::unix::fs::symlink(
            dir.path().join(kept),
            dir.path().join("33333333-3333-4333-8333-333333333333"),
        )
        .expect("link");
        let keep = vec![kept.to_owned()];
        assert_eq!(
            orphans(dir.path(), &keep, Duration::ZERO),
            [dir.path().join(gone)]
        );
        // A folder changed within the minimum age stays.
        assert!(orphans(dir.path(), &keep, MIN_AGE).is_empty());
    }
}
