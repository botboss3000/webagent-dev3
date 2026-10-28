//! Supervised module services for dev (`live-context`) builds, one per module id,
//! started from the published engine catalog.
use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use webagent_module_service::{Limits, Supervised};

/// The service for `id` at the current catalog revision; a new revision starts a
/// new generation while admitted calls finish on the old one.
pub(crate) fn supervised(id: &'static str) -> Result<Arc<Supervised>> {
    static CURRENT: Mutex<Option<HashMap<&'static str, (String, Arc<Supervised>)>>> = Mutex::new(None);
    let catalog = crate::engine_components::catalog()?;
    let descriptor = catalog.entries.get(id).ok_or_else(|| anyhow::anyhow!("Module {id} unavailable"))?;
    let root = catalog.root.clone().ok_or_else(|| anyhow::anyhow!("Module {id} unpublished"))?;
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    let services = current.get_or_insert_with(HashMap::new);
    if let Some((_, service)) = services.get(id).filter(|(revision, _)| *revision == catalog.revision) {
        return Ok(service.clone());
    }
    let executable = root.join("plugins/engine").join(id).join(&descriptor.executable);
    let limits = Limits {
        max_frame_bytes: descriptor.limits.max_request_bytes.max(descriptor.limits.max_response_bytes),
        call_timeout: std::time::Duration::from_millis(descriptor.limits.timeout_ms),
    };
    let service = Arc::new(Supervised::new(
        move || tokio::process::Command::new(&executable),
        limits,
        Arc::new(|_| Vec::new()),
    ));
    services.insert(id, (catalog.revision.clone(), service.clone()));
    Ok(service)
}
