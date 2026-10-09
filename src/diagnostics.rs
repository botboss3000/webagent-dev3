//! Optional installation plugin. Removing its directory and rebuilding leaves safe no-op hooks.
#[cfg(diagnostics_plugin)]
#[path = "diagnostics_plugin/mod.rs"]
mod installed;
#[cfg(diagnostics_plugin)]
pub(crate) use installed::*;
#[cfg(not(diagnostics_plugin))]
pub(crate) async fn init(_: &std::path::Path, _: &crate::Store, _: &str) {}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn failure(_: &crate::Store, _: &serde_json::Value, _: &anyhow::Error, _: &str) {}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn committed(_: &crate::Store, _: &serde_json::Value, _: &str, _: bool) {}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn backend_error() {}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn router() -> axum::Router<crate::App> {
    axum::Router::new()
}

#[cfg(not(diagnostics_plugin))]
pub(crate) async fn http_errors(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    next.run(request).await
}

#[cfg(not(diagnostics_plugin))]
pub(crate) fn provider_enabled(_: &crate::Store) -> bool {
    false
}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn assembly(
    _: &crate::Store,
    _: &serde_json::Value,
    _: &crate::context_input::Request<'_>,
    _: &anyhow::Result<crate::context_assembly::Prepared<'_>>,
) -> Option<serde_json::Value> {
    None
}
#[cfg(not(diagnostics_plugin))]
pub(crate) fn validation(
    _: &crate::Store,
    _: &serde_json::Value,
    _: &'static str,
    _: &'static str,
) -> Option<serde_json::Value> {
    None
}
#[cfg(not(diagnostics_plugin))]
pub(crate) async fn context(
    _: &crate::Store,
    _: &str,
    _: &str,
    _: &str,
) -> Result<serde_json::Value, crate::ApiError> {
    Err(crate::ApiError(
        axum::http::StatusCode::NOT_FOUND,
        "Snapshot unavailable".into(),
    ))
}

#[cfg(not(diagnostics_plugin))]
pub(crate) fn summary(
    _: &crate::Store,
    _: &serde_json::Value,
    _: &crate::context_input::Request<'_>,
    _: &anyhow::Result<()>,
) -> Option<serde_json::Value> {
    None
}

// Observe only successful, advancing user transactions; diagnostics never participate in authority.
impl crate::Store {
    pub(crate) async fn save_run_observed(
        &self,
        run: &mut serde_json::Value,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let before = run["run_revision"].clone();
        self.save_run(run, kind, payload).await?;
        if run["run_revision"] != before {
            committed(self, run, kind, false);
        }
        Ok(())
    }
    pub(crate) async fn save_run_override_observed(
        &self,
        run: &mut serde_json::Value,
        kind: &str,
        payload: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let before = run["run_revision"].clone();
        self.save_run_override(run, kind, payload).await?;
        if run["run_revision"] != before {
            committed(self, run, kind, true);
        }
        Ok(())
    }
}
