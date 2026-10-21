//! Root compatibility for Engine-owned automatic Code Index context retrieval.
use crate::*;
pub use webagent_engine::context::retrieval::{bounded_failure_stage,validate_settings};
pub(crate) async fn prepare_run_dependencies(app: &App, sid: &str, run: &mut Value) -> Result<bool> {app.engine().code_index_prepare_run_dependencies(sid,run).await}
pub(crate) async fn authorize_attachment(
    app: &App,
    sid: &str,
    run_id: &str,
    expected_settings_sha256: &str,
) -> Result<()> {app.engine().code_index_authorize_attachment(sid,run_id,expected_settings_sha256).await}
pub(crate) async fn retrieve(
    app: &App,
    sid: &str,
    run_id: &str,
    user_text: &str,
    cancel: CancellationToken,
) -> Result<Option<Value>> {app.engine().code_index_retrieve(sid,run_id,user_text,cancel).await}
