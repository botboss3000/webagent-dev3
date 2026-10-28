//! The usage-reports module: linked into stable (`linked-context`) builds and run
//! as its own service process from the published engine catalog in dev
//! (`live-context`) builds. Callers pass only samples they have authorized.
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;
use webagent_usage::{Sample, UsageReports};

#[cfg(feature = "linked-context")]
struct Handle(webagent_usage_reports::Reports);
#[cfg(feature = "linked-context")]
impl Handle {
    async fn open() -> Result<Self> { Ok(Self(webagent_usage_reports::Reports)) }
    fn reports(&self) -> &impl UsageReports { &self.0 }
}

/// Holding the process keeps an admitted call on the generation it started on.
#[cfg(feature = "live-context")]
struct Handle(#[allow(dead_code)] std::sync::Arc<webagent_module_service::Process>, webagent_usage::UsageReportsClient);
#[cfg(feature = "live-context")]
impl Handle {
    async fn open() -> Result<Self> {
        let process = supervised()?.current().await?;
        let client = webagent_usage::UsageReportsClient(process.peer().clone());
        Ok(Self(process, client))
    }
    fn reports(&self) -> &impl UsageReports { &self.1 }
}

/// The service for the current catalog revision; a new revision starts a new
/// generation while admitted calls finish on the old one.
#[cfg(feature = "live-context")]
fn supervised() -> Result<std::sync::Arc<webagent_module_service::Supervised>> {
    use std::sync::{Arc, Mutex};
    const ID: &str = "usage-reports";
    static CURRENT: Mutex<Option<(String, Arc<webagent_module_service::Supervised>)>> = Mutex::new(None);
    let catalog = crate::engine_components::catalog()?;
    let descriptor = catalog.entries.get(ID).ok_or_else(|| anyhow::anyhow!("Usage reports module unavailable"))?;
    let root = catalog.root.clone().ok_or_else(|| anyhow::anyhow!("Usage reports module unpublished"))?;
    let mut current = CURRENT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((_, service)) = current.as_ref().filter(|(revision, _)| *revision == catalog.revision) {
        return Ok(service.clone());
    }
    let executable = root.join("plugins/engine").join(ID).join(&descriptor.executable);
    let limits = webagent_module_service::Limits {
        max_frame_bytes: descriptor.limits.max_request_bytes.max(descriptor.limits.max_response_bytes),
        call_timeout: std::time::Duration::from_millis(descriptor.limits.timeout_ms),
    };
    let service = Arc::new(webagent_module_service::Supervised::new(
        move || tokio::process::Command::new(&executable),
        limits,
        Arc::new(|_| Vec::new()),
    ));
    *current = Some((catalog.revision.clone(), service.clone()));
    Ok(service)
}

pub(crate) async fn analytics(samples: &[Sample], interval: &str, group_by: &str, labels: &HashMap<String, String>, now: i64) -> Result<Option<Value>> {
    Handle::open().await?.reports().analytics(samples, interval, group_by, labels, now).await
}
pub(crate) async fn model_totals(samples: &[Sample], entry: &str, model: &str, include_unassigned: bool) -> Result<Value> {
    Handle::open().await?.reports().model_totals(samples, entry, model, include_unassigned).await
}
pub(crate) async fn apply_resets(samples: &[Sample], resets: &Value) -> Result<Vec<Sample>> {
    Handle::open().await?.reports().apply_resets(samples, resets).await
}
pub(crate) async fn mark_reset(resets: &Value, kind: &str, model: &str, now: i64) -> Result<Option<Value>> {
    Handle::open().await?.reports().mark_reset(resets, kind, model, now).await
}
