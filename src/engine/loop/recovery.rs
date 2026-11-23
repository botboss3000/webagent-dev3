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
    scanned_apps(app, None).await
}
/// [`user_apps`] for only these accounts, so a scanner woken for a few users' deadlines
/// does not open every account the scan returns.
pub(crate) async fn user_apps_among(
    app: &App,
    among: &std::collections::HashSet<String>,
) -> Result<Vec<App>> {
    scanned_apps(app, Some(among)).await
}
async fn scanned_apps(
    app: &App,
    among: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<App>> {
    let mut views = Vec::new();
    for user in app.store.scan_user_ids().await? {
        if among.is_some_and(|among| !among.contains(&user)) {
            continue;
        }
        // One unreadable account must not stop every other account's module work.
        let store = match app.store.for_user(&user).await {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(%error, "Account skipped: its storage could not be opened");
                continue;
            }
        };
        let mut view = app.clone();
        view.store = store;
        views.push(view);
    }
    Ok(views)
}
