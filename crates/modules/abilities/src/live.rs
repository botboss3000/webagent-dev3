//! Live ability edits. Ability JSON and Markdown in the checkout apply without a release:
//! they are laid over the published abilities as a snapshot folder the catalog loads like a
//! publication, so code and compiled plugins still come only from the release. Rechecked at
//! most once a second; an edit that does not load keeps the last good snapshot.
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
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

fn editable(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("json" | "md")
    )
}

/// Checkout-relative ability JSON and Markdown files (symlinks are skipped).
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
                pending.push(path);
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

/// Builds the snapshot `root/abilities` = `base/abilities` with `changed` taken from
/// `checkout`, beside the base's other entries, unless it already exists.
fn build(base: &Path, checkout: &Path, changed: &[PathBuf], root: &Path) -> std::io::Result<()> {
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
        for relative in changed {
            let target = partial.join("abilities").join(relative);
            if let Some(folder) = target.parent() {
                std::fs::create_dir_all(folder)?;
            }
            std::fs::copy(checkout.join(relative), target)?;
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
    let mut state = STATE.lock().unwrap_or_else(|poisoned| {
        STATE.clear_poison();
        let mut state = poisoned.into_inner();
        *state = None;
        state
    });
    if let Some(current) = state.as_ref() {
        if current.checked.elapsed() < recheck {
            return current.root.clone();
        }
    }
    let listed = files(checkout);
    let key = fingerprint(base, checkout, &listed);
    if let Some(current) = state.as_mut() {
        if current.key == key {
            current.checked = Instant::now();
            return current.root.clone();
        }
    }
    let previous = state
        .as_ref()
        .map_or_else(|| base.to_path_buf(), |s| s.root.clone());
    let changed: Vec<PathBuf> = listed
        .into_iter()
        .filter(|relative| {
            std::fs::read(checkout.join(relative)).ok()
                != std::fs::read(base.join("abilities").join(relative)).ok()
        })
        .collect();
    let chosen = if changed.is_empty() {
        base.to_path_buf()
    } else {
        // The name carries the snapshot layout, so an older layout is never reused.
        let root = cache.join(format!("v2-{}", hex::encode(&key[..12])));
        match build(base, checkout, &changed, &root) {
            Ok(()) if valid(&root.join("abilities")) => {
                prune(cache, &root);
                root
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
    *state = Some(State {
        checked: Instant::now(),
        key,
        root: chosen.clone(),
    });
    chosen
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
        let ok = |_: &Path| true;
        let now = Duration::ZERO;
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
        std::fs::write(checkout.join("Core/demo/tool.json"), "{\"v\":2}").unwrap();
        let live = overlay_every(now, &base, &checkout, &cache, ok);
        assert!(live.starts_with(&cache));
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
        std::fs::write(checkout.join("Core/demo/tool.json"), "{\"v\":1}").unwrap();
        assert_eq!(overlay_every(now, &base, &checkout, &cache, ok), base);
    }
}
