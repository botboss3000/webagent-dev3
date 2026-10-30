#![cfg(test)]
//! Root-fixture tests of `read_own_prompt` in `webagent_native_tools::prompt`.
use crate::{session_agent_id, App};
use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
async fn execute(app:&App,sid:&str,run_id:&str,name:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    let call=crate::tools::native::Call {sid,run_id,generation:"",pending_id:""};
    crate::tools::native::prompt::execute(&crate::tools::native_host::Host(app),call,name,args,cancel).await
}
mod tests {
    use super::*;
    #[tokio::test]
    async fn session_agent_routing_isolated_from_passed_app_view_and_live_deny() {
        let (root,app)=crate::runtime_tests::fixture().await;
        // Isolated encrypted fixture only: create a second tenant and migrate
        // through the normal storage splitter, never read runtime database files.
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES('prompt_tenant','Other','','openai_subscription','fixture','active',?,1,1,1)")
            .bind(app.store.seal(&json!({"owner_id":"admin","ability_revision":0})).unwrap())
            .execute(&app.store.agent_pool).await.unwrap();
        app.store.migrate_agent_storage(root.path()).await.unwrap();
        let other=app.store.for_agent("prompt_tenant").await.unwrap();
        app.store.upsert_prompt_slot("identity","DEFAULT PRIVATE",None,None,None,0).await.unwrap();
        other.upsert_prompt_slot("identity","TENANT PRIVATE",None,None,None,0).await.unwrap();
        app.store.put("session:tenant",&json!({"id":"tenant","agent_id":"prompt_tenant","title":"t","created_at":1})).await.unwrap();
        let agent=session_agent_id(&app,"tenant").await.unwrap();
        assert_eq!(agent,"prompt_tenant");
        let scoped=app.store.for_agent(&agent).await.unwrap();
        let output=json!(scoped.list_prompt_slots().await.unwrap());
        assert!(output.to_string().contains("TENANT PRIVATE"));
        assert!(!output.to_string().contains("DEFAULT PRIVATE"));
        app.store.put("run:read-run",&json!({"id":"read-run","session_id":"tenant",
            "agent_id":"prompt_tenant","loaded_abilities":["base"],"status":"executing"})).await.unwrap();
        // Newly installed Base tools have no implicit tier/level grant.
        let denied=execute(&app,"tenant","read-run","read_own_prompt",&json!({}),CancellationToken::new()).await.unwrap();
        assert_eq!(denied["status"],"error");
        assert!(!denied.to_string().contains("TENANT PRIVATE"));
        let malformed=execute(&app,"tenant","read-run","read_own_prompt",&json!({"agent_id":"shared_default"}),CancellationToken::new()).await.unwrap();
        assert_eq!(malformed["status"],"error");
        let cancel=CancellationToken::new();cancel.cancel();
        assert!(execute(&app,"tenant","read-run","read_own_prompt",&json!({}),cancel).await.is_err());
    }
}
