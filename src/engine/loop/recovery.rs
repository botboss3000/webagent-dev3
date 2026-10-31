//! Role compatibility for actual Engine restart/reconciliation logic.
use super::*;
use crate::engine_runtime::Engine;
pub(crate) fn reconcile_late_tool_result(run: &mut Value, result: Result<Value>) -> Result<bool> {
    Engine::reconcile_late_tool_result(run, result)
}
pub(crate) fn attach_external_build_result(run: &mut Value, result: &Value) -> Result<()> {
    Engine::attach_external_build_result(run, result)
}
pub(crate) async fn recover(app: &App, startup: bool) -> Result<()> {
    Engine::recover(&app.engine(), startup).await
}
pub(crate) async fn user_apps(app: &App) -> Result<Vec<App>> {
    let mut views = Vec::new();
    for user in app.store.scan_user_ids().await? {
        let mut view = app.clone();
        view.store = app.store.for_user(&user).await?;
        views.push(view);
    }
    Ok(views)
}
