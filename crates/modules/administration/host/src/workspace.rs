//! Workspace operations on the build broker that hold a lease while they run.
use crate::{trusted, AdministrationHost};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;

/// Run workspace operation `name` on the broker for session `sid`. The lease is renewed
/// every 10 seconds; a cancelled call revokes it, and its receipt must be inspected
/// before another attempt.
pub async fn workspace_host(
    host: &dyn AdministrationHost,
    sid: &str,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let user = host.store().current_user_id();
    let fingerprint = json!(digest(&json!([user, sid, name, args])));
    let future = host.broker(
        name,
        args,
        sid,
        Some(trusted(user, sid, &fingerprint)),
        cancel.clone(),
    );
    tokio::pin!(future);
    let empty = json!({});
    loop {
        tokio::select! {
            result = &mut future => {
                let reply = result?;
                if reply["status"] == "success" {
                    if !reply["output"].is_object() {
                        bail!("Malformed workspace service response");
                    }
                    return Ok(reply["output"].clone());
                }
                return Ok(reply);
            },
            () = cancel.cancelled() => {
                let revoke = trusted(user, sid, &fingerprint);
                let _ = host
                    .broker("administrator_host_revoke", &empty, sid, Some(revoke), CancellationToken::new())
                    .await;
                bail!("Workspace operation interrupted; inspect its receipt");
            },
            () = tokio::time::sleep(Duration::from_secs(10)) => {
                let lease = trusted(user, sid, &fingerprint);
                host.broker("administrator_host_lease", &empty, sid, Some(lease), cancel.clone()).await?;
            }
        }
    }
}
