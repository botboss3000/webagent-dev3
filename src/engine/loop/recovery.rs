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
/// The accounts the periodic scan returns (`among`: only these of them). At start-up that is
/// every account, since none has been evaluated yet. Callers open each with [`user_app`] and let
/// it go before the next (one at a time), so the storage bound on connected account databases
/// can close idle ones: collecting a view of every account first held a thread per account and
/// reached the loop's 1,024-task limit.
pub(crate) async fn scanned_user_ids(
    app: &App,
    among: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<String>> {
    Ok(app
        .store
        .scan_user_ids()
        .await?
        .into_iter()
        .filter(|user| among.is_none_or(|among| among.contains(user)))
        .collect())
}
/// Every account, for reads that must also count work the scan leaves closed (paused and
/// approval-waiting runs); open each with [`user_app`] in turn.
pub(crate) async fn every_user_id(app: &App) -> Result<Vec<String>> {
    app.store.account_ids().await
}
/// One account's view; an unreadable account is skipped (logged) so it cannot stop every
/// other account's work.
pub(crate) async fn user_app(app: &App, user: &str) -> Option<App> {
    match app.store.for_user(user).await {
        Ok(store) => {
            let mut view = app.clone();
            view.store = store;
            Some(view)
        }
        Err(error) => {
            tracing::warn!(%error, "Account skipped: its storage could not be opened");
            None
        }
    }
}
