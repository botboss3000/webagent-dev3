//! Idle polling decisions do not replace durable reducer or admission decisions.
use super::*;
use std::sync::{Mutex, OnceLock};

type HealthIdentity = (u64, u64, u64, i64, i64, i64, i64, u32);
struct HealthEntry {
    path: PathBuf,
    identity: HealthIdentity,
    generation: String,
    implementation: Option<String>,
    checked: u64,
    available: bool,
}
/// Cache only until the original probe expires; file replacement or binding changes invalidate immediately.
pub(super) fn available(path: &FsPath, generation: &str, implementation: Option<&str>, at: u64) -> bool {
    static CACHE: OnceLock<Mutex<Vec<HealthEntry>>> = OnceLock::new();
    let Ok(meta) = std::fs::symlink_metadata(path) else { return false; };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 4096 { return false; }
    let identity = (meta.dev(), meta.ino(), meta.len(), meta.mtime(), meta.mtime_nsec(), meta.ctime(), meta.ctime_nsec(), meta.mode());
    let mut cache = CACHE.get_or_init(Mutex::default).lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = cache.iter().find(|e| e.path == path && e.identity == identity && e.generation == generation && e.implementation.as_deref() == implementation) {
        return entry.available && at >= entry.checked && at.saturating_sub(entry.checked) <= 15;
    }
    cache.retain(|e| e.path != path);
    let Some(health) = std::fs::read(path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return false; };
    let result = health_binding_current(&health, generation, implementation, at);
    if cache.len() == 16 { cache.remove(0); }
    cache.push(HealthEntry { path: path.to_owned(), identity, generation: generation.to_owned(), implementation: implementation.map(str::to_owned), checked: health["checked_at"].as_u64().unwrap_or(0), available: result });
    result
}
/// Empty users need no reducer; retain generation recovery, queued work, and cursor advancement.
pub(super) fn empty(envelope: &Value, latest: i64, generation: &str) -> bool {
    if envelope["generation"].as_str().is_some_and(|v| v != generation)
        || envelope["event_cursor"].as_i64().unwrap_or(0) != latest
        || envelope["pending"].as_array().is_some_and(|v| !v.is_empty())
        || envelope["admission_results"].as_object().is_some_and(|v| !v.is_empty()) { return false; }
    !envelope["state"]["definitions"].as_object().into_iter().flatten().any(|(_, d)| d["status"] == "active")
        && !envelope["state"]["occurrences"].as_object().into_iter().flatten().any(|(_, o)| !matches!(o["status"].as_str(), Some("completed" | "failed" | "cancelled" | "skipped")))
}
/// A no-op tick can wait up to fifteen seconds, but never beyond a schedule/retry deadline.
pub(super) fn delay(before: &Value, after: &Value, next: Option<u64>, now_ms: u64) -> u64 {
    if before["state"] != after["state"] || after["pending"].as_array().is_some_and(|v| !v.is_empty())
        || after["state"]["occurrences"].as_object().into_iter().flatten().any(|(_, o)| matches!(o["status"].as_str(), Some("queued" | "admitted"))) { return 1000; }
    let retry = after["state"]["occurrences"].as_object().into_iter().flatten().filter_map(|(_, o)| (o["status"] == "retry_wait").then(|| o["retry_ms"].as_u64()).flatten()).min();
    next.into_iter().chain(retry).min().map_or(15000, |deadline| deadline.saturating_sub(now_ms).clamp(1, 15000))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_users_preserve_cursor_recovery_and_pending_work() {
        let empty = json!({"generation":"epoch","event_cursor":9,"state":{"definitions":{},"occurrences":{}}});
        assert!(super::empty(&empty,9,"epoch"));
        assert!(!super::empty(&empty,10,"epoch")); assert!(!super::empty(&empty,9,"new"));
        for field in ["pending", "admission_results"] {
            let mut changed=empty.clone();changed[field]=if field=="pending" {json!([{}])}else{json!({"pending":{}})};
            assert!(!super::empty(&changed,9,"epoch"));
        }
        let mut changed=empty.clone();changed["state"]["definitions"]["scheduled"]=json!({"status":"active"});assert!(!super::empty(&changed,9,"epoch"));
        changed=empty.clone();changed["state"]["occurrences"]["manual"]=json!({"status":"queued"});assert!(!super::empty(&changed,9,"epoch"));
    }
    #[test]
    fn idle_backoff_respects_deadlines_retries_and_state_changes() {
        let before=json!({"state":{"definitions":{},"occurrences":{}}});
        assert_eq!(delay(&before,&before,None,1000),15000);
        assert_eq!(delay(&before,&before,Some(1500),1000),500);
        let mut retry=before.clone();retry["state"]["occurrences"]["retry"]=json!({"status":"retry_wait","retry_ms":1200});
        assert_eq!(delay(&retry,&retry,None,1000),200);
        assert_eq!(delay(&before,&retry,None,1000),1000);
    }
    #[test]
    fn health_cache_expires_and_rejects_changed_bindings_and_replacements() {
        let root=tempfile::tempdir().unwrap();let p=root.path().join("health");
        std::fs::write(&p,json!({"healthy":true,"generation":"epoch","implementation":"code","checked_at":100}).to_string()).unwrap();
        assert!(available(&p,"epoch",Some("code"),100));assert!(available(&p,"epoch",Some("code"),115));assert!(!available(&p,"epoch",Some("code"),116));
        assert!(!available(&p,"other",Some("code"),101));assert!(!available(&p,"epoch",Some("new"),101));
        std::fs::write(&p,json!({"healthy":false,"generation":"epoch","implementation":"code","checked_at":102}).to_string()).unwrap();
        assert!(!available(&p,"epoch",Some("code"),102));
    }
}
