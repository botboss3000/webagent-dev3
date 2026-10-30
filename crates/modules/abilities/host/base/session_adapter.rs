#![cfg(test)]
//! Root-fixture tests of the session read tools in `webagent_native_tools::session`.
use crate::App;
use anyhow::Result;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_accounts_host::AccountsStore;
async fn execute(app:&App,sid:&str,name:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    let call=crate::tools::native::Call {sid,run_id:"",generation:"",pending_id:""};
    crate::tools::native::session::execute(&crate::tools::native_host::Host(app),call,name,args,cancel).await
}
mod tests {
    use super::*;
    #[tokio::test]
    async fn encrypted_attachment_lookup_rejects_another_session_and_user_store() {
        let (_root,base)=crate::runtime_tests::fixture_uninitialized().await;
        let first=base.store.create_user_account("first-session-reader","long-fixture-hash-placeholder").await.unwrap();
        let second=base.store.create_user_account("second-session-reader","long-fixture-hash-placeholder").await.unwrap();
        let mut app=base.clone();
        app.store=base.store.for_user(first["user_id"].as_str().unwrap()).await.unwrap();
        let mut other_app=base.clone();
        other_app.store=base.store.for_user(second["user_id"].as_str().unwrap()).await.unwrap();
        app.store.put("session:current",&json!({"id":"current","title":"Current","created_at":1})).await.unwrap();
        app.store.put("session:foreign",&json!({"id":"foreign","title":"Foreign","created_at":1})).await.unwrap();
        let id="11111111-1111-4111-8111-111111111111";
        app.store.put(&format!("attachment:{id}"),&json!({"schema_version":1,"id":id,
            "session_id":"foreign","run_id":"r","name":"n","media_type":"text/plain",
            "bytes":6,"sha256":"hash","text":"secret"})).await.unwrap();
        let query=json!({"attachment_id":id});
        assert_eq!(execute(&app,"current","read_attachment",&query,CancellationToken::new()).await.unwrap()["status"],"error");
        assert_eq!(execute(&app,"foreign","read_attachment",&query,CancellationToken::new()).await.unwrap()["output"]["text"],"secret");
        // Same installation, shared encryption authority, and identical session/record
        // IDs: caller routing, rather than key uniqueness, must isolate these reads.
        other_app.store.put("session:foreign",&json!({"id":"foreign","title":"Other","created_at":1})).await.unwrap();
        assert_eq!(execute(&other_app,"foreign","read_attachment",&query,CancellationToken::new()).await.unwrap()["status"],"error");
        other_app.store.put(&format!("attachment:{id}"),&json!({"schema_version":1,"id":id,
            "session_id":"foreign","run_id":"r","name":"n","media_type":"text/plain",
            "bytes":5,"sha256":"other-hash","text":"other"})).await.unwrap();
        assert_eq!(execute(&other_app,"foreign","read_attachment",&query,CancellationToken::new()).await.unwrap()["output"]["text"],"other");
        assert_eq!(execute(&app,"foreign","read_attachment",&query,CancellationToken::new()).await.unwrap()["output"]["text"],"secret");
        for (reader,text) in [(&app,"first-only evidence"),(&other_app,"second-only evidence")] {
            sqlx::query("INSERT INTO chat(seq,id,session_id,run_id,role,content,created_at) VALUES(1,'same-message','foreign','r','user',?,1)")
                .bind(reader.store.seal(&json!(text)).unwrap()).execute(&reader.store.pool).await.unwrap();
        }
        let search=json!({"query":"first-only"});
        assert_eq!(execute(&app,"foreign","search_this_session",&search,CancellationToken::new()).await.unwrap()["output"]["messages"].as_array().unwrap().len(),1);
        assert!(execute(&other_app,"foreign","search_this_session",&search,CancellationToken::new()).await.unwrap()["output"]["messages"].as_array().unwrap().is_empty());
        let cancel=CancellationToken::new();cancel.cancel();
        assert!(execute(&app,"foreign","read_attachment",&query,cancel).await.is_err());
    }
    #[tokio::test]
    async fn encrypted_chat_search_returns_only_current_session_public_rows() {
        let (_root,app)=crate::runtime_tests::fixture_uninitialized().await;
        for sid in ["current","foreign"] {
            app.store.put(&format!("session:{sid}"),&json!({"id":sid,"title":sid,"created_at":1})).await.unwrap();
        }
        for (seq,sid,text) in [(1,"current","known needle"),(2,"foreign","foreign needle")] {
            sqlx::query("INSERT INTO chat(seq,id,session_id,run_id,role,content,created_at) VALUES(?,?,?,?,?,?,?)")
                .bind(seq).bind(format!("m{seq}")).bind(sid).bind("r").bind("user")
                .bind(app.store.seal(&json!(text)).unwrap()).bind(1)
                .execute(&app.store.pool).await.unwrap();
        }
        let output=execute(&app,"current","search_this_session",&json!({"query":"needle"}),CancellationToken::new()).await.unwrap();
        assert_eq!(output["output"]["messages"].as_array().unwrap().len(),1);
        assert_eq!(output["output"]["messages"][0]["content_preview"],"known needle");
        assert_eq!(output["output"]["has_more"],false);
        assert_eq!(execute(&app,"missing","search_this_session",&json!({"query":"needle"}),CancellationToken::new()).await.unwrap()["status"],"error");
    }
}
