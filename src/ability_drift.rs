//! The running app's view of checkout drift: `/live-checkout/plugins/abilities` compared
//! with the published release it runs. Rechecked at most every five seconds, and only
//! recomputed when a file in either tree changed. Without a publication (tests, local
//! runs) there is no drift.
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, UNIX_EPOCH},
};
use webagent_abilities::drift::{self, Drift};

const RECHECK: Duration = Duration::from_secs(5);

struct Cached {
    checked: Instant,
    signature: [u8; 32],
    drift: Arc<Drift>,
}

/// The current drift. A comparison that fails leaves the running release in charge.
pub(crate) fn current() -> Arc<Drift> {
    static CACHE: Mutex<Option<Cached>> = Mutex::new(None);
    // A poisoned cache is cleared and rebuilt, never read.
    let mut cache = CACHE.lock().unwrap_or_else(|poisoned| {
        CACHE.clear_poison();
        let mut cache = poisoned.into_inner();
        *cache = None;
        cache
    });
    if let Some(cached) = cache
        .as_ref()
        .filter(|cached| cached.checked.elapsed() < RECHECK)
    {
        return cached.drift.clone();
    }
    let Some(published) =
        crate::live_assets::published_root().map(|root| root.join("plugins/abilities"))
    else {
        return Arc::default();
    };
    let checkout = Path::new(webagent_configuration::live::CHECKOUT).join("plugins/abilities");
    if !checkout.is_dir() {
        return Arc::default();
    }
    let signature = signature(&published, &checkout);
    if let Some(cached) = cache
        .as_mut()
        .filter(|cached| cached.signature == signature)
    {
        cached.checked = Instant::now();
        return cached.drift.clone();
    }
    let drift = drift::compare(&published, &checkout)
        .map(Arc::new)
        .unwrap_or_else(|error| {
            tracing::warn!("Ability drift unavailable: {error:#}");
            Arc::default()
        });
    *cache = Some(Cached {
        checked: Instant::now(),
        signature,
        drift: drift.clone(),
    });
    drift
}

/// Path, size and modification time of every checkout file, plus the published revision
/// (immutable, so its path identifies it).
fn signature(published: &Path, checkout: &Path) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(published.as_os_str().as_encoded_bytes());
    let mut pending = vec![checkout.to_owned()];
    let mut entries = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else {
                let modified = meta
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok());
                entries.push((
                    entry.path(),
                    meta.len(),
                    modified.map(|time| time.as_nanos()),
                ));
            }
        }
    }
    entries.sort();
    for (path, len, modified) in entries {
        hasher.update(path.as_os_str().as_encoded_bytes());
        hasher.update(len.to_le_bytes());
        hasher.update(modified.unwrap_or(0).to_le_bytes());
    }
    hasher.finalize().into()
}
