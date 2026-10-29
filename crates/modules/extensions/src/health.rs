//! Package health and idle polling. Health receipts are content-free probe results bound
//! to one activation generation (and, for native packages, one implementation); they
//! expire fifteen seconds after the probe. Idle decisions never replace a reducer's or an
//! admission's own decision.
use serde_json::Value;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Seconds a probe result stays valid.
const FRESH: u64 = 15;
const CACHED: usize = 16;

/// Whether `health` is a healthy probe of `generation` no older than fifteen seconds.
#[must_use]
pub fn current(health: &Value, generation: &str, at: u64) -> bool {
    health["healthy"] == true
        && health["generation"] == generation
        && health["checked_at"]
            .as_u64()
            .is_some_and(|checked| checked <= at && at.saturating_sub(checked) <= FRESH)
}

/// `current`, and probed on `implementation` when one is bound.
#[must_use]
pub fn binding_current(
    health: &Value,
    generation: &str,
    implementation: Option<&str>,
    at: u64,
) -> bool {
    current(health, generation, at)
        && implementation.is_none_or(|value| health["implementation"] == value)
}

type Identity = (u64, u64, u64, i64, i64, i64, i64, u32);

struct Entry {
    path: PathBuf,
    identity: Identity,
    generation: String,
    implementation: Option<String>,
    checked: u64,
    available: bool,
}

/// The health cache; a poisoned cache is cleared and rebuilt, never read.
fn cache() -> MutexGuard<'static, Vec<Entry>> {
    static CACHE: OnceLock<Mutex<Vec<Entry>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Mutex::default);
    cache.lock().unwrap_or_else(|poisoned| {
        cache.clear_poison();
        let mut entries = poisoned.into_inner();
        entries.clear();
        entries
    })
}

/// Whether the health receipt at `path` makes the package available now. Cached only
/// until the probe expires; a replaced file or changed binding is reread at once.
#[must_use]
pub fn available(path: &Path, generation: &str, implementation: Option<&str>, at: u64) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 {
        return false;
    }
    let identity = (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
        meta.mode(),
    );
    let mut cache = cache();
    if let Some(entry) = cache.iter().find(|entry| {
        entry.path == path
            && entry.identity == identity
            && entry.generation == generation
            && entry.implementation.as_deref() == implementation
    }) {
        return entry.available && at >= entry.checked && at.saturating_sub(entry.checked) <= FRESH;
    }
    cache.retain(|entry| entry.path != path);
    let Some(health) = std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    else {
        return false;
    };
    let result = binding_current(&health, generation, implementation, at);
    if cache.len() == CACHED {
        cache.remove(0);
    }
    cache.push(Entry {
        path: path.to_owned(),
        identity,
        generation: generation.to_owned(),
        implementation: implementation.map(str::to_owned),
        checked: health["checked_at"].as_u64().unwrap_or(0),
        available: result,
    });
    result
}

/// Whether a user's package envelope has nothing to do: same generation, cursor at
/// `latest`, nothing pending, no active definitions and no unfinished occurrences.
#[must_use]
pub fn idle(envelope: &Value, latest: i64, generation: &str) -> bool {
    if envelope["generation"]
        .as_str()
        .is_some_and(|value| value != generation)
        || envelope["event_cursor"].as_i64().unwrap_or(0) != latest
        || envelope["pending"]
            .as_array()
            .is_some_and(|pending| !pending.is_empty())
        || envelope["admission_results"]
            .as_object()
            .is_some_and(|results| !results.is_empty())
    {
        return false;
    }
    let state = &envelope["state"];
    let active = state["definitions"]
        .as_object()
        .into_iter()
        .flatten()
        .any(|(_, definition)| definition["status"] == "active");
    let unfinished =
        state["occurrences"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, occurrence)| {
                !matches!(
                    occurrence["status"].as_str(),
                    Some("completed" | "failed" | "cancelled" | "skipped")
                )
            });
    !active && !unfinished
}

/// Milliseconds until the next tick: one second after any change or queued work,
/// otherwise up to fifteen seconds but never past a schedule or retry deadline.
#[must_use]
pub fn delay(before: &Value, after: &Value, next: Option<u64>, now_ms: u64) -> u64 {
    let occurrences = || {
        after["state"]["occurrences"]
            .as_object()
            .into_iter()
            .flatten()
    };
    if before["state"] != after["state"]
        || after["pending"]
            .as_array()
            .is_some_and(|pending| !pending.is_empty())
        || occurrences().any(|(_, occurrence)| {
            matches!(occurrence["status"].as_str(), Some("queued" | "admitted"))
        })
    {
        return 1000;
    }
    let retry = occurrences()
        .filter(|(_, occurrence)| occurrence["status"] == "retry_wait")
        .filter_map(|(_, occurrence)| occurrence["retry_ms"].as_u64())
        .min();
    next.into_iter()
        .chain(retry)
        .min()
        .map_or(15_000, |deadline| {
            deadline.saturating_sub(now_ms).clamp(1, 15_000)
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    #[test]
    fn idle_users_preserve_cursor_recovery_and_pending_work() {
        let empty = json!({"generation":"epoch","event_cursor":9,"state":{"definitions":{},"occurrences":{}}});
        assert!(idle(&empty, 9, "epoch"));
        assert!(!idle(&empty, 10, "epoch"));
        assert!(!idle(&empty, 9, "new"));
        for field in ["pending", "admission_results"] {
            let mut changed = empty.clone();
            changed[field] = if field == "pending" {
                json!([{}])
            } else {
                json!({"pending":{}})
            };
            assert!(!idle(&changed, 9, "epoch"));
        }
        let mut changed = empty.clone();
        changed["state"]["definitions"]["scheduled"] = json!({"status":"active"});
        assert!(!idle(&changed, 9, "epoch"));
        changed = empty.clone();
        changed["state"]["occurrences"]["manual"] = json!({"status":"queued"});
        assert!(!idle(&changed, 9, "epoch"));
    }

    #[test]
    fn idle_backoff_respects_deadlines_retries_and_state_changes() {
        let before = json!({"state":{"definitions":{},"occurrences":{}}});
        assert_eq!(delay(&before, &before, None, 1000), 15_000);
        assert_eq!(delay(&before, &before, Some(1500), 1000), 500);
        let mut retry = before.clone();
        retry["state"]["occurrences"]["retry"] = json!({"status":"retry_wait","retry_ms":1200});
        assert_eq!(delay(&retry, &retry, None, 1000), 200);
        assert_eq!(delay(&before, &retry, None, 1000), 1000);
    }

    #[test]
    fn health_cache_expires_and_rejects_changed_bindings_and_replacements() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("health");
        let receipt =
            json!({"healthy":true,"generation":"epoch","implementation":"code","checked_at":100});
        std::fs::write(&path, receipt.to_string()).unwrap();
        assert!(available(&path, "epoch", Some("code"), 100));
        assert!(available(&path, "epoch", Some("code"), 115));
        assert!(!available(&path, "epoch", Some("code"), 116));
        assert!(!available(&path, "other", Some("code"), 101));
        assert!(!available(&path, "epoch", Some("new"), 101));
        let failed =
            json!({"healthy":false,"generation":"epoch","implementation":"code","checked_at":102});
        std::fs::write(&path, failed.to_string()).unwrap();
        assert!(!available(&path, "epoch", Some("code"), 102));
    }

    #[test]
    fn native_health_requires_both_current_bindings_and_a_fresh_probe() {
        let mut health =
            json!({"healthy":true,"generation":"epoch","implementation":"code-a","checked_at":100});
        assert!(binding_current(&health, "epoch", Some("code-a"), 100));
        assert!(!binding_current(&health, "epoch", Some("code-b"), 100));
        assert!(!binding_current(&health, "next-epoch", Some("code-a"), 100));
        assert!(!binding_current(&health, "epoch", Some("code-a"), 116));
        health["implementation"] = Value::Null;
        assert!(!binding_current(&health, "epoch", Some("code-a"), 100));
        assert!(binding_current(&health, "epoch", None, 100));
        assert!(current(&health, "epoch", 115));
        assert!(!current(&health, "epoch", 116));
    }
}
