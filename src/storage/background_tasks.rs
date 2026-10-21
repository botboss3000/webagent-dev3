//! Root compatibility for Engine-owned background task repository SQL.
use super::*;
impl Store {
 pub(super) async fn background_index(&self)->Result<()> {self.engine_repository().background_index().await}
 pub(crate) async fn background_get(&self,id:&str)->Result<Option<Value>> {self.engine_repository().background_get(id).await}
 pub(crate) async fn background_scan(&self,sid:Option<&str>)->Result<Vec<Value>> {self.engine_repository().background_scan(sid).await}
 pub(crate) async fn background_claim_delivery(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,run:&Value)->Result<()> {self.engine_repository().background_claim_delivery(tx,run).await}
 pub(crate) async fn background_cas(&self,task:&Value,expected:Option<u64>)->Result<bool> {self.engine_repository().background_cas(task,expected).await}
}

#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn delivery_is_atomic_exclusive_and_encrypted() {
        let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
        let task=json!({"id":"delivery","user_id":"admin","session_id":"s","status":"completed","revision":0,"result":"private evidence"});
        assert!(store.background_cas(&task,None).await.unwrap());
        let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key='background_task:delivery'").fetch_one(&store.pool).await.unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
        let run=json!({"id":"first","session_id":"s","background_delivered":["delivery"]});
        let mut tx=store.pool.begin().await.unwrap();
        store.background_claim_delivery(&mut tx,&run).await.unwrap();tx.rollback().await.unwrap();
        assert!(store.background_get("delivery").await.unwrap().unwrap()["delivery_run_id"].is_null());
        let mut tx=store.pool.begin().await.unwrap();
        store.background_claim_delivery(&mut tx,&run).await.unwrap();tx.commit().await.unwrap();
        let mut other=run.clone();other["id"]=json!("second");
        let mut tx=store.pool.begin().await.unwrap();
        assert!(store.background_claim_delivery(&mut tx,&other).await.is_err());tx.rollback().await.unwrap();
        assert!(store.background_scan(Some("s")).await.unwrap().is_empty());
    }
}
