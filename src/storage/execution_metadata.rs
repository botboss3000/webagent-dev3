//! Host-owned encrypted evidence uses the existing interaction identity and transaction.
use super::*;
impl Store {
 pub(super) async fn attach_execution_metadata(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,sid:&str,rid:&str,call_id:&str,value:&Value)->Result<()> {
  let metadata:webagent_context_contracts::execution::Metadata=serde_json::from_value(value.clone())?;metadata.validate()?;
  let row=sqlx::query("SELECT execution_metadata FROM interactions WHERE session_id=? AND run_id=? AND step=?")
   .bind(sid).bind(rid).bind(format!("tool:{call_id}")).fetch_one(&mut **tx).await?;
  if let Some(bytes)=row.get::<Option<Vec<u8>>,_>("execution_metadata") {
   if self.unseal(&bytes)?!=*value {anyhow::bail!("Execution metadata identity conflict");}return Ok(());
  }
  sqlx::query("UPDATE interactions SET execution_metadata=? WHERE session_id=? AND run_id=? AND step=? AND execution_metadata IS NULL")
   .bind(self.encode_user_record(value)?).bind(sid).bind(rid).bind(format!("tool:{call_id}")).execute(&mut **tx).await?;Ok(())
 }
}
#[cfg(test)]mod tests {
 use super::*;
 #[tokio::test]async fn atomic_encrypted_separate_recall_and_conflict_fenced(){
  let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
  let item=json!({"type":"function_call_output","call_id":"call","output":"original"});
  let metadata=json!({"version":1,"rpc_calls":[{"peer":"tool-worker","id":"routing-original"}]});
  let mut tx=store.pool.begin().await.unwrap();store.insert_interaction(&mut tx,"s","r","tool:call",&item).await.unwrap();
  store.attach_execution_metadata(&mut tx,"s","r","call",&metadata).await.unwrap();tx.rollback().await.unwrap();
  assert!(store.tool_result("s","r","call").await.unwrap().is_none());
  let mut tx=store.pool.begin().await.unwrap();store.insert_interaction(&mut tx,"s","r","tool:call",&item).await.unwrap();
  store.attach_execution_metadata(&mut tx,"s","r","call",&metadata).await.unwrap();tx.commit().await.unwrap();
  let reopened=Store::open(root.path()).await.unwrap();assert_eq!(reopened.tool_result("s","r","call").await.unwrap(),Some(item));
  let bytes:Vec<u8>=sqlx::query_scalar("SELECT execution_metadata FROM interactions").fetch_one(&store.pool).await.unwrap();assert!(!String::from_utf8_lossy(&bytes).contains("routing-original"));
  let activity=reopened.private_run_activity("s","r",0).await.unwrap();assert_eq!(activity["events"][0]["execution_metadata"],metadata);
  assert_eq!(activity["events"][0]["payload"]["output"],"original");
  let mut tx=store.pool.begin().await.unwrap();let mut changed=metadata.clone();changed["rpc_calls"][0]["id"]=json!("other");
  assert!(store.attach_execution_metadata(&mut tx,"s","r","call",&changed).await.is_err());tx.rollback().await.unwrap();
  assert!(store.private_run_activity("other","r",0).await.unwrap()["events"].as_array().unwrap().is_empty());
 }
}
