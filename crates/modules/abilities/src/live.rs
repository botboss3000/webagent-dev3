//! Live ability edits. Ability JSON, Markdown and icons (`.svg`, checked when the catalog
//! reads them) in the checkout apply without a release:
//! they are laid over the published abilities as a snapshot folder the catalog loads like a
//! publication, so code and compiled plugins still come only from the release. A file the
//! checkout deleted is left out. Rechecked at most once a second; an edit that does not load
//! keeps the last good snapshot.
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, UNIX_EPOCH},
};

const RECHECK: Duration = Duration::from_secs(1);
/// Snapshot folders kept besides the one in use.
const KEEP: usize = 3;

struct State {
    checked: Instant,
    key: [u8; 32],
    root: PathBuf,
    /// The publication `root` holds (itself, or the base its snapshot was built on).
    base: PathBuf,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
static BUILDING: Mutex<()> = Mutex::new(());

fn editable(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("json" | "md" | "svg")
    )
}

/// Root-installed executable packages inside the abilities tree. They are git-ignored and
/// never released (the broker's `INSTALLED_PACKAGES`), so they are not ability edits;
/// counting them kept the overlay on and API/loop could never acknowledge a publication.
const INSTALLED_PACKAGES: &[&str] = &["Core/mcp/package"];

/// Checkout-relative ability JSON, Markdown and icon files (symlinks and installed
/// packages are skipped).
fn files(checkout: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![checkout.to_path_buf()];
    while let Some(folder) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if kind.is_dir() {
                let installed = path.strip_prefix(checkout).is_ok_and(|relative| {
                    INSTALLED_PACKAGES.iter().any(|p| relative == Path::new(p))
                });
                if !installed {
                    pending.push(path);
                }
            } else if kind.is_file() && editable(&path) {
                if let Ok(relative) = path.strip_prefix(checkout) {
                    found.push(relative.to_path_buf());
                }
            }
        }
    }
    found.sort();
    found
}

/// Names, sizes and modification times of the editable files, with the base they apply to.
fn fingerprint(base: &Path, checkout: &Path, files: &[PathBuf]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(base.as_os_str().as_encoded_bytes());
    for relative in files {
        hash.update(relative.as_os_str().as_encoded_bytes());
        if let Ok(info) = std::fs::metadata(checkout.join(relative)) {
            let modified = info
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            hash.update(info.len().to_le_bytes());
            hash.update(modified.to_le_bytes());
        }
    }
    hash.finalize().into()
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Gives the snapshot the base's other top-level entries (prompts, UI, assets), which
/// callers read through the catalog's source root: links on Unix, copies elsewhere.
fn share_rest(base: &Path, root: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(base)? {
        let entry = entry?;
        if entry.file_name() == "abilities" {
            continue;
        }
        let target = root.join(entry.file_name());
        #[cfg(unix)]
        std::os::unix::fs::symlink(entry.path(), &target)?;
        #[cfg(not(unix))]
        {
            if entry.file_type()?.is_dir() {
                copy_tree(&entry.path(), &target)?;
            } else {
                std::fs::copy(entry.path(), &target)?;
            }
        }
    }
    Ok(())
}

/// One snapshot change: the checkout's version of a file, or its removal (`None`).
type Change = (PathBuf, Option<Vec<u8>>);

/// The editable files whose checkout version differs from `base`, with their new bytes;
/// a base file the checkout no longer has is a removal.
fn changes(base: &Path, checkout: &Path, listed: &[PathBuf]) -> Vec<Change> {
    let base_abilities = base.join("abilities");
    let mut found: Vec<Change> = listed
        .iter()
        .filter_map(|relative| {
            let now = std::fs::read(checkout.join(relative)).ok()?;
            (std::fs::read(base_abilities.join(relative)).ok().as_ref() != Some(&now))
                .then(|| (relative.clone(), Some(now)))
        })
        .collect();
    let kept: std::collections::BTreeSet<&PathBuf> = listed.iter().collect();
    found.extend(
        files(&base_abilities)
            .into_iter()
            .filter(|relative| !kept.contains(relative))
            .map(|relative| (relative, None)),
    );
    found
}

/// Builds the snapshot `root/abilities` = `base/abilities` with `changed` applied, beside the
/// base's other entries, unless it already exists.
fn build(base: &Path, changed: &[Change], root: &Path) -> std::io::Result<()> {
    if root.join("abilities").is_dir() {
        return Ok(());
    }
    let parent = root.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let partial = parent.join(format!(
        ".partial-{}-{}",
        std::process::id(),
        UNIX_EPOCH.elapsed().map_or(0, |d| d.as_nanos())
    ));
    let result = (|| {
        copy_tree(&base.join("abilities"), &partial.join("abilities"))?;
        share_rest(base, &partial)?;
        for (relative, body) in changed {
            let target = partial.join("abilities").join(relative);
            match body {
                Some(body) => {
                    if let Some(folder) = target.parent() {
                        std::fs::create_dir_all(folder)?;
                    }
                    // Release files are read-only and their copies keep that mode: replace
                    // the copy rather than writing into it.
                    match std::fs::remove_file(&target) {
                        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                            return Err(error)
                        }
                        _ => {}
                    }
                    std::fs::write(target, body)?;
                }
                None => std::fs::remove_file(target)?,
            }
        }
        match std::fs::rename(&partial, root) {
            // Another process built the same snapshot first.
            Err(_) if root.join("abilities").is_dir() => Ok(()),
            other => other,
        }
    })();
    let _ = std::fs::remove_dir_all(&partial);
    result
}

/// Drops all but the newest snapshot folders, never `keep`.
fn prune(cache: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(cache) else {
        return;
    };
    let mut folders: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|k| k.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    folders.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, folder) in folders.into_iter().skip(KEEP) {
        if folder != keep {
            let _ = std::fs::remove_dir_all(folder);
        }
    }
}

/// The catalog source to load: `base` (a publication root holding `abilities/`) when the
/// checkout's ability JSON and Markdown match it, else a snapshot under `cache` with the
/// checkout's versions. `valid` decides whether a new snapshot loads; one that does not
/// keeps the previous choice.
pub fn overlay(
    base: &Path,
    checkout: &Path,
    cache: &Path,
    valid: impl Fn(&Path) -> bool,
) -> PathBuf {
    overlay_every(RECHECK, base, checkout, cache, valid)
}

fn overlay_every(
    recheck: Duration,
    base: &Path,
    checkout: &Path,
    cache: &Path,
    valid: impl Fn(&Path) -> bool,
) -> PathBuf {
    if let Some(current) = state().as_ref() {
        if current.checked.elapsed() < recheck {
            return current.root.clone();
        }
    }
    // One builder at a time. `STATE` is only held briefly: checking a snapshot reads tool
    // definitions, which ask `selected` for it.
    let _building = BUILDING
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let listed = files(checkout);
    let key = fingerprint(base, checkout, &listed);
    let previous = {
        let mut state = state();
        if let Some(current) = state.as_mut() {
            if current.key == key {
                current.checked = Instant::now();
                return current.root.clone();
            }
        }
        state.as_ref().map_or_else(
            || (base.to_path_buf(), base.to_path_buf()),
            |s| (s.root.clone(), s.base.clone()),
        )
    };
    let changed = changes(base, checkout, &listed);
    let (chosen, built_on) = if changed.is_empty() {
        (base.to_path_buf(), base.to_path_buf())
    } else {
        // The name carries the snapshot layout, so an older layout is never reused.
        let root = cache.join(format!("v3-{}", hex::encode(&key[..12])));
        match build(base, &changed, &root) {
            Ok(()) if valid(&root.join("abilities")) => {
                prune(cache, &root);
                (root, base.to_path_buf())
            }
            Ok(()) => {
                eprintln!("Live ability edits do not load; keeping the previous abilities");
                let _ = std::fs::remove_dir_all(&root);
                previous
            }
            Err(error) => {
                eprintln!("Live ability snapshot failed: {error}");
                previous
            }
        }
    };
    *state() = Some(State {
        checked: Instant::now(),
        key,
        root: chosen.clone(),
        base: built_on,
    });
    chosen
}

/// The chosen snapshot; a poisoned lock forgets it, so the next use chooses again.
fn state() -> std::sync::MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|poisoned| {
        STATE.clear_poison();
        let mut state = poisoned.into_inner();
        *state = None;
        state
    })
}

/// The publication root the catalog last chose (`overlay`), if it has chosen one in this
/// process; tool definitions follow it so live edits reach every reader.
pub fn selected() -> Option<PathBuf> {
    state().as_ref().map(|state| state.root.clone())
}

/// The publication that `root` holds when it is the catalog's last choice: `root` itself,
/// or the release its live snapshot was built on (so adoption names that release).
pub fn built_on(root: &Path) -> Option<PathBuf> {
    state()
        .as_ref()
        .filter(|state| state.root == root)
        .map(|state| state.base.clone())
}

/// Builds at `root` what the live snapshot would be once `edits` (checkout-relative ability
/// paths, `None` deletes) are written to the checkout, so they can be checked first. The
/// caller removes `root`.
pub fn candidate(
    base: &Path,
    checkout: &Path,
    edits: &[(PathBuf, Option<Vec<u8>>)],
    root: &Path,
) -> std::io::Result<()> {
    let mut changed: std::collections::BTreeMap<PathBuf, Option<Vec<u8>>> =
        changes(base, checkout, &files(checkout))
            .into_iter()
            .collect();
    for (relative, body) in edits {
        match body {
            None if !base.join("abilities").join(relative).is_file() => {
                changed.remove(relative);
            }
            _ => {
                changed.insert(relative.clone(), body.clone());
            }
        }
    }
    build(base, &changed.into_iter().collect::<Vec<_>>(), root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_edits_overlay_the_base_and_bad_ones_keep_the_last_good() {
        let dir = tempfile::tempdir().unwrap();
        let (base, checkout, cache) = (
            dir.path().join("release"),
            dir.path().join("checkout"),
            dir.path().join("cache"),
        );
        std::fs::create_dir_all(base.join("config")).unwrap();
        std::fs::write(base.join("config/prompt.md"), "prompt").unwrap();
        for root in [base.join("abilities"), checkout.clone()] {
            std::fs::create_dir_all(root.join("Core/demo")).unwrap();
            std::fs::write(root.join("Core/demo/tool.json"), "{\"v\":1}").unwrap();
            std::fs::write(root.join("Core/demo/handler.rs"), "code").unwrap();
        }
        // Published releases are read-only, as deployed.
        let mut read_only = std::fs::metadata(base.join("abilities/Core/demo/tool.json"))
            .unwrap()
            .permissions();
        read_only.set_readonly(true);
        std::fs::set_permissions(base.join("abilities/Core/demo/tool.json"), read_only).unwrap();
        let ok = |_: &Path| true;
        let now = Duration::ZERO;
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
        // An installed package exists only in the checkout and is not an edit.
        std::fs::create_dir_all(checkout.join("Core/mcp/package")).unwrap();
        std::fs::write(checkout.join("Core/mcp/package/plugin.json"), "{}").unwrap();
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
        std::fs::write(checkout.join("Core/demo/tool.json"), "{\"v\":2}").unwrap();
        let live = overlay_every(now, &base, &checkout, &cache, ok);
        assert!(live.starts_with(&cache));
        assert_eq!(
            built_on(&live),
            Some(base.clone()),
            "a snapshot names its release"
        );
        assert_eq!(built_on(&base), None, "only the current choice is answered");
        let tool = std::fs::read_to_string(live.join("abilities/Core/demo/tool.json")).unwrap();
        assert_eq!(tool, "{\"v\":2}");
        assert!(live.join("abilities/Core/demo/handler.rs").is_file());
        assert_eq!(
            std::fs::read_to_string(live.join("config/prompt.md")).unwrap(),
            "prompt",
            "the release's other folders stay readable through the snapshot"
        );
        std::fs::write(checkout.join("Core/demo/tool.json"), "{broken").unwrap();
        let kept = overlay_every(now, &base, &checkout, &cache, |root| {
            std::fs::read_to_string(root.join("Core/demo/tool.json"))
                .is_ok_and(|t| t.starts_with("{\""))
        });
        assert_eq!(
            kept, live,
            "an edit that does not load keeps the last good snapshot"
        );
        assert_eq!(built_on(&kept), Some(base.clone()));
        std::fs::write(checkout.join("Core/demo/tool.json"), "{\"v\":1}").unwrap();
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
        for root in [base.join("abilities"), checkout.clone()] {
            std::fs::write(root.join("Core/demo/old.skill.md"), "old").unwrap();
        }
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
        std::fs::remove_file(checkout.join("Core/demo/old.skill.md")).unwrap();
        let live = overlay_every(now, &base, &checkout, &cache, ok);
        assert!(
            !live.join("abilities/Core/demo/old.skill.md").exists(),
            "a file the checkout deleted leaves the snapshot"
        );
        assert!(live.join("abilities/Core/demo/tool.json").is_file());
    }

    #[test]
    fn a_candidate_applies_drafts_over_the_checkout() {
        let dir = tempfile::tempdir().unwrap();
        let (base, checkout) = (dir.path().join("release"), dir.path().join("checkout"));
        for root in [base.join("abilities"), checkout.clone()] {
            std::fs::create_dir_all(root.join("Core/demo")).unwrap();
            std::fs::write(root.join("Core/demo/tool.json"), "{\"v\":1}").unwrap();
            std::fs::write(root.join("Core/demo/a.skill.md"), "a").unwrap();
        }
        std::fs::write(checkout.join("Core/demo/b.skill.md"), "b").unwrap();
        let root = dir.path().join("candidate");
        let edits = [
            (
                PathBuf::from("Core/demo/tool.json"),
                Some(b"{\"v\":2}".to_vec()),
            ),
            (PathBuf::from("Core/demo/a.skill.md"), None),
            (PathBuf::from("Core/demo/c.skill.md"), Some(b"c".to_vec())),
        ];
        candidate(&base, &checkout, &edits, &root).unwrap();
        let read =
            |name: &str| std::fs::read_to_string(root.join("abilities/Core/demo").join(name)).ok();
        assert_eq!(read("tool.json").as_deref(), Some("{\"v\":2}"));
        assert_eq!(read("a.skill.md"), None);
        assert_eq!(
            read("b.skill.md").as_deref(),
            Some("b"),
            "the checkout's own edits stay"
        );
        assert_eq!(read("c.skill.md").as_deref(), Some("c"));
    }
}
