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
        let process = crate::module_services::supervised("usage-reports")?.current().await?;
        let client = webagent_usage::UsageReportsClient(process.peer().clone());
        Ok(Self(process, client))
    }
    fn reports(&self) -> &impl UsageReports { &self.1 }
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
