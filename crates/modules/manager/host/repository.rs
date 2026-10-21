//! Root compatibility delegates; Manager owns all review SQL and transitions.
use super::*;
impl Store {
 pub(crate) fn manager_repository(&self)->webagent_manager::storage::Repository<'_,Self> {webagent_manager::storage::Repository::new(&self.root,&self.pool,self.current_user_id(),self)}
 pub(crate) async fn claim_manager_review(&self,id:&str)->Result<Option<Value>> {self.manager_repository().claim_manager_review(id).await}
 pub(crate) async fn finish_manager_review(&self,observed:&Value,state:&str,verdict:Option<crate::manager::Verdict>)->Result<bool> {self.manager_repository().finish_manager_review(observed,state,verdict).await}
}
#[cfg(test)]
mod tests {
    use super::*;
    async fn submitted(store: &Store, sid: &str, id: &str) -> Value {
        if store.account_by_id("admin").await.unwrap().is_none(){store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();}
        store
            .put(
                &format!("session:{sid}"),
                &json!({"id":sid,"agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        let run = json!({"id":id,"session_id":sid,"agent_id":"shared_default","status":"queued","generation":"","updated_at":0});
        let message = json!({"role":"user","content":"Rejected coding question","run_id":id});
        let record =
            json!({"id":id,"session_id":sid,"status":"pending_review","run":run,"message":message});
        store
            .atomic_docs_event(
                vec![
                    (format!("manager_review:{id}"), record),
                    (format!("manager_pending:{sid}"), json!({"id":id})),
                    (
                        format!("receipt:message:{sid}:{id}"),
                        json!({"digest":"fixture"}),
                    ),
                ],
                sid,
                "manager_submission",
                &message,
            )
            .await
            .unwrap();
        store.claim_manager_review(id).await.unwrap().unwrap()
    }
    #[tokio::test]
    async fn rejected_message_is_visible_but_never_context_or_run() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        assert!(store.context("session").await.unwrap().is_empty());
        assert!(store.active_runs().await.unwrap().is_empty());
        assert!(store
            .claim_manager_review("review")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .finish_manager_review(
                &record,
                "unacceptable",
                Some(crate::manager::Verdict::Unacceptable)
            )
            .await
            .unwrap());
        assert!(!store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(reopened.chat_snapshot("session").await.unwrap().0.len(), 1);
        assert!(reopened.context("session").await.unwrap().is_empty());
        assert!(reopened.get("run:review").await.unwrap().is_none());
        assert!(reopened
            .get("manager_pending:session")
            .await
            .unwrap()
            .is_none());
        // A later admitted turn cannot pull rejected text into its context.
        reopened
            .atomic_docs_event(
                vec![],
                "session",
                "message",
                &json!({"role":"user","run_id":"next","content":"What sizes do you have?"}),
            )
            .await
            .unwrap();
        let input = reopened.context("session").await.unwrap();
        assert_eq!(input.len(), 1);
        assert_eq!(input[0]["content"], "What sizes do you have?");
    }
    #[tokio::test]
    async fn acceptance_is_atomic_fenced_and_exactly_once() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        let mut stale = record.clone();
        stale["generation"] = json!("stale");
        assert!(!store
            .finish_manager_review(
                &stale,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert!(store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert!(!store
            .finish_manager_review(
                &record,
                "acceptable",
                Some(crate::manager::Verdict::Acceptable)
            )
            .await
            .unwrap());
        assert_eq!(store.chat_snapshot("session").await.unwrap().0.len(), 1);
        assert_eq!(store.context("session").await.unwrap().len(), 1);
        assert_eq!(store.active_runs().await.unwrap().len(), 1);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM loop_commands WHERE command_id='manager:review'",
        )
        .fetch_one(&store.pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
    }
    #[tokio::test]
    async fn review_error_does_not_admit_and_pending_blocks_recycle() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let record = submitted(&store, "session", "review").await;
        assert!(store
            .finish_manager_review(&record, "review_error", None)
            .await
            .unwrap());
        assert!(store.context("session").await.unwrap().is_empty());
        assert!(store.get("run:review").await.unwrap().is_none());
    }
    #[tokio::test]
    async fn late_manager_verdict_abort_rolls_back_actual_owner_effects_and_preserves_submission() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let record=submitted(&store,"session","review").await;
        let before_docs:Vec<(String,Vec<u8>)>=sqlx::query_as("SELECT key,value FROM docs ORDER BY key").fetch_all(&store.pool).await.unwrap();
        let before_events:Vec<(i64,String,String,Vec<u8>)>=sqlx::query_as("SELECT seq,session_id,kind,payload FROM events ORDER BY seq").fetch_all(&store.pool).await.unwrap();
        let notifications:i64=sqlx::query_scalar("SELECT COUNT(*) FROM notification_stage").fetch_one(&store.pool).await.unwrap();
        sqlx::raw_sql("CREATE TRIGGER reject_manager_verdict BEFORE INSERT ON events WHEN NEW.kind='manager_verdict' BEGIN SELECT RAISE(ABORT,'fixture late verdict abort'); END;").execute(&store.pool).await.unwrap();
        assert!(store.finish_manager_review(&record,"acceptable",Some(crate::manager::Verdict::Acceptable)).await.is_err());
        let after_docs:Vec<(String,Vec<u8>)>=sqlx::query_as("SELECT key,value FROM docs ORDER BY key").fetch_all(&store.pool).await.unwrap();assert_eq!(after_docs,before_docs);
        let after_events:Vec<(i64,String,String,Vec<u8>)>=sqlx::query_as("SELECT seq,session_id,kind,payload FROM events ORDER BY seq").fetch_all(&store.pool).await.unwrap();assert_eq!(after_events,before_events);
        assert!(store.context("session").await.unwrap().is_empty());assert!(store.active_runs().await.unwrap().is_empty());
        let commands:i64=sqlx::query_scalar("SELECT COUNT(*) FROM loop_commands WHERE command_id='manager:review'").fetch_one(&store.pool).await.unwrap();assert_eq!(commands,0);
        assert_eq!(store.chat_snapshot("session").await.unwrap().0.len(),1);
        sqlx::query("DROP TRIGGER reject_manager_verdict").execute(&store.pool).await.unwrap();
        assert!(store.finish_manager_review(&record,"acceptable",Some(crate::manager::Verdict::Acceptable)).await.unwrap());
        assert!(!store.finish_manager_review(&record,"acceptable",Some(crate::manager::Verdict::Acceptable)).await.unwrap());
        let after_notifications:i64=sqlx::query_scalar("SELECT COUNT(*) FROM notification_stage").fetch_one(&store.pool).await.unwrap();assert_eq!(after_notifications,notifications,"acceptance must not restage original message notification");
    }
}
