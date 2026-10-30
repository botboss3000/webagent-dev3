//! Root entry points for app navigation requests; the logic lives in
//! `webagent_native_tools::app_control`.
use crate::*;
use webagent_native_tools::{app_control, Call};

/// Only fixed native handlers can request these caller-bound surfaces; no model URL is accepted.
pub async fn enqueue(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending: &str,
    tool: &str,
    args: &Value,
    destination: &str,
    cancel: CancellationToken,
) -> Result<Value> {
    let call = Call {
        sid,
        run_id: rid,
        generation,
        pending_id: pending,
    };
    app_control::enqueue(
        &super::native_host::Host(app),
        call,
        tool,
        args,
        destination,
        cancel,
    )
    .await
}
pub async fn projection(app: &App, sid: &str) -> Result<Value> {
    app_control::projection(&super::native_host::Host(app), sid).await
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn credential_forms_never_expire_and_closing_one_reaches_every_device() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let actor = shared_chat::actor(&app).to_string();
        app.store.module_cas("core_ui:s",0,&json!({"state":{"destination":"credential","actor_id":actor,"credential_name":"example"}})).await.unwrap();
        assert_eq!(
            projection(&app, "s").await.unwrap()["request"]["credential_name"],
            "example"
        );
        app.store
            .module_cas("core_ui:s", 1, &json!({"state":{}}))
            .await
            .unwrap();
        assert_eq!(
            projection(&app, "s").await.unwrap(),
            json!({"revision":2,"request":null})
        );
        app.store
            .module_cas(
                "core_ui:s",
                2,
                &json!({"state":{"destination":"app:files","actor_id":actor,"expires_at":1}}),
            )
            .await
            .unwrap();
        assert_eq!(projection(&app, "s").await.unwrap(), Value::Null);
    }
}
