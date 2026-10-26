//! Application-wide requester slots. Absent canonical commits remain reserved.
use super::*;
impl Store {
 async fn requester_slots_schema(&self)->Result<()> {
  sqlx::query("CREATE TABLE IF NOT EXISTS requester_run_slots(run_id TEXT PRIMARY KEY,requester_id TEXT NOT NULL,owner_id TEXT NOT NULL,session_id TEXT NOT NULL,released INTEGER NOT NULL DEFAULT 0,sequence INTEGER NOT NULL,replaced_by TEXT); CREATE INDEX IF NOT EXISTS requester_run_slots_actor ON requester_run_slots(requester_id,released)").execute(&self.app_pool).await?;
  let has:i64=sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('requester_run_slots') WHERE name='replaced_by'").fetch_one(&self.app_pool).await?;
  if has==0 {
   if let Err(error)=sqlx::query("ALTER TABLE requester_run_slots ADD COLUMN replaced_by TEXT").execute(&self.app_pool).await {
    let has:i64=sqlx::query_scalar("SELECT count(*) FROM pragma_table_info('requester_run_slots') WHERE name='replaced_by'").fetch_one(&self.app_pool).await?;
    if has==0 {return Err(error.into());}
   }
  }
  Ok(())
 }
 pub(crate) async fn reconcile_requester_slots(&self)->Result<()> {
  self.requester_slots_schema().await?;
  // Read canonical WAL snapshots before the app writer. New admissions reserve
  // first. Absence cannot prove an interrupted canonical commit did not happen.
  let mut records=Vec::new();
  for owner in self.all_user_ids().await? {
   let path=self.root.join("user_data").join(&owner).join(format!("{owner}.db"));
   if !path.exists(){continue;}
   // Reuse the account's cached pool: a fresh connection per account on every admission
   // spawned a thread and reopened the database. Range bounds use the docs key index.
   let pool=self.for_user(&owner).await?.pool;
   let rows=sqlx::query("SELECT key,value FROM docs WHERE (key>='run:' AND key<'run;') OR (key>='manager_review:' AND key<'manager_review;') OR (key>='followup_queue:' AND key<'followup_queue;')").fetch_all(&pool).await?;
   for row in rows {
    let key:String=row.get("key");let value=self.unseal(&row.get::<Vec<u8>,_>("value"))?;
    if key.starts_with("followup_queue:") {
        for entry in value["entries"].as_array().into_iter().flatten() {
            let Some(id)=entry["id"].as_str() else {continue};
            let Some(sid)=value["session_id"].as_str() else {continue};
            let reviewing=entry["review_status"]=="reviewing";
            let terminal=matches!(entry["review_status"].as_str(),Some("acceptable"|"unacceptable"|"review_error"|"cancelled")) || matches!(entry["status"].as_str(),Some("cancelled"|"rejected"|"delivered"));
            if reviewing || terminal {records.push((format!("followup-review:{owner}:{sid}:{id}"),owner.clone(),owner.clone(),sid.to_owned(),terminal));}
        }
        continue;
    }
    let review=key.starts_with("manager_review:");let run=if review {&value["run"]}else{&value};
    let Some(rid)=run["id"].as_str() else {continue};let Some(sid)=run["session_id"].as_str() else {continue};
    let terminal=if review {matches!(value["status"].as_str(),Some("unacceptable"|"review_error"|"cancelled"))}else{matches!(run["status"].as_str(),Some("completed"|"cancelled"|"failed"))};
    if review && value["status"]=="acceptable" {continue;}
    records.push((rid.to_owned(),run["requester_user_id"].as_str().unwrap_or(&owner).to_owned(),owner.clone(),sid.to_owned(),terminal));
   }
  }
  let mut tx=self.app_pool.begin().await?;
  for (rid,actor,owner,sid,terminal) in records {
   if terminal {
    sqlx::query("UPDATE requester_run_slots SET released=1 WHERE run_id=? AND owner_id=?").bind(rid).bind(owner).execute(&mut *tx).await?;
   }else{
    sqlx::query("INSERT OR IGNORE INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots))").bind(rid).bind(actor).bind(owner).bind(sid).execute(&mut *tx).await?;
   }
  }
  tx.commit().await?;Ok(())
 }
 pub(crate) async fn reserve_requester_run(&self,run:&Value)->Result<()> {
  self.reconcile_requester_slots().await?;
  let rid=run["id"].as_str().ok_or_else(||anyhow!("Run id missing"))?;
  let sid=run["session_id"].as_str().ok_or_else(||anyhow!("Run session missing"))?;
  let actor=run["requester_user_id"].as_str().unwrap_or(self.current_user_id());
  let limit=self.live_access(actor,run["agent_id"].as_str().unwrap_or("shared_default")).await?.max_active_runs() as i64;
  let mut tx=self.app_pool.begin().await?;
  // Force app writer ownership before reading count; two canonical owners must
  // never both observe the same spare account capacity.
  sqlx::query("UPDATE requester_run_slots SET released=released WHERE run_id=?").bind(rid).execute(&mut *tx).await?;
  let existing=sqlx::query("SELECT requester_id,owner_id,session_id,released FROM requester_run_slots WHERE run_id=?").bind(rid).fetch_optional(&mut *tx).await?;
  if let Some(row)=existing{
   if row.get::<String,_>("requester_id")!=actor || row.get::<String,_>("owner_id")!=self.current_user_id() || row.get::<String,_>("session_id")!=sid || row.get::<i64,_>("released")!=0{return Err(anyhow!("Requester reservation mismatch"));}
  }else{
   let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0").bind(actor).fetch_one(&mut *tx).await?;
   if count>=limit{return Err(anyhow!("Account run limit reached"));}
   sqlx::query("INSERT INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots))").bind(rid).bind(actor).bind(self.current_user_id()).bind(sid).execute(&mut *tx).await?;
  }
  tx.commit().await?;Ok(())
 }

 pub(crate) async fn transfer_requester_run(&self,origin:&Value,next:&Value)->Result<()> {
  self.reconcile_requester_slots().await?;
  let old=origin["id"].as_str().ok_or_else(||anyhow!("Origin id missing"))?;
  let new=next["id"].as_str().ok_or_else(||anyhow!("Successor id missing"))?;
  let actor=origin["requester_user_id"].as_str().unwrap_or(self.current_user_id());
  let sid=origin["session_id"].as_str().ok_or_else(||anyhow!("Origin session missing"))?;
  if old==new || next["requester_user_id"].as_str().unwrap_or(self.current_user_id())!=actor || next["session_id"]!=sid || actor!=self.current_user_id(){return Err(anyhow!("Requester handoff identity mismatch"));}
  let limit=self.live_access(actor,next["agent_id"].as_str().unwrap_or("shared_default")).await?.max_active_runs() as i64;
  let mut tx=self.app_pool.begin().await?;
  sqlx::query("UPDATE requester_run_slots SET released=released WHERE run_id=?").bind(old).execute(&mut *tx).await?;
  let previous=sqlx::query("SELECT requester_id,owner_id,session_id,released,replaced_by,sequence FROM requester_run_slots WHERE run_id=?").bind(old).fetch_one(&mut *tx).await?;
  if previous.get::<String,_>("requester_id")!=actor || previous.get::<String,_>("owner_id")!=self.current_user_id() || previous.get::<String,_>("session_id")!=sid{return Err(anyhow!("Requester handoff reservation mismatch"));}
  if previous.get::<i64,_>("released")!=0 {
   if previous.get::<Option<String>,_>("replaced_by").as_deref()!=Some(new){return Err(anyhow!("Requester handoff already reserved"));}
   let successor=sqlx::query("SELECT requester_id,owner_id,session_id,released FROM requester_run_slots WHERE run_id=?").bind(new).fetch_one(&mut *tx).await?;
   if successor.get::<String,_>("requester_id")!=actor || successor.get::<String,_>("owner_id")!=self.current_user_id() || successor.get::<String,_>("session_id")!=sid || successor.get::<i64,_>("released")!=0{return Err(anyhow!("Requester successor reservation mismatch"));}
  }else{
   let sequence:i64=previous.get("sequence");
   let rank:i64=sqlx::query_scalar("SELECT count(*) FROM requester_run_slots WHERE requester_id=? AND released=0 AND sequence<=?").bind(actor).bind(sequence).fetch_one(&mut *tx).await?;
   if rank>limit{return Err(anyhow!("Account run limit reached"));}
   sqlx::query("INSERT INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence) VALUES(?,?,?,?,?)").bind(new).bind(actor).bind(self.current_user_id()).bind(sid).bind(sequence).execute(&mut *tx).await?;
   sqlx::query("UPDATE requester_run_slots SET released=1,replaced_by=? WHERE run_id=? AND released=0").bind(new).bind(old).execute(&mut *tx).await?;
  }
  // This is a conservative handoff reservation, not evidence that the canonical
  // old cancellation/new creation committed. No absence cleanup can undo it.
  tx.commit().await?;Ok(())
 }
 pub(crate) async fn requester_claim_ready(&self,run:&Value)->Result<bool>{
  self.reserve_requester_run(run).await?;
  let actor=run["requester_user_id"].as_str().unwrap_or(self.current_user_id());
  let limit=self.live_access(actor,run["agent_id"].as_str().unwrap_or("shared_default")).await?.max_active_runs() as i64;
  let rank:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)").bind(actor).bind(run["id"].as_str().unwrap()).fetch_one(&self.app_pool).await?;Ok(rank<=limit)
 }
}

#[cfg(test)]
mod tests {
 use super::*;
 async fn setup()->(tempfile::TempDir,Store,Store,String){
  let root=tempfile::tempdir().unwrap();let store=Store::open(root.path()).await.unwrap();
  store.create_admin_account("root","long-hash-placeholder","generation").await.unwrap();
  let mut tier:crate::access::Tier=serde_json::from_value(store.read_access_policy().unwrap()["tiers"]["basic"].clone()).unwrap();
  tier.id="quota_one".into();tier.revision=0;tier.max_active_runs=1;
  store.save_entitlement_tier(tier,0).await.unwrap();
  let mut ids=Vec::new();
  for name in ["first-owner","second-owner","requester"] {
   let account=store.create_user_account(name,"long-hash-placeholder").await.unwrap();let id=account["user_id"].as_str().unwrap().to_owned();
   store.update_account_access(&id,1,crate::access::AccountStatus::Active,crate::access::Role::User,"quota_one").await.unwrap();ids.push(id);
  }
  let one=store.for_user(&ids[0]).await.unwrap();let two=store.for_user(&ids[1]).await.unwrap();
  for owner in [&one,&two] {owner.put("session:s",&json!({"id":"s","agent_id":"shared_default","status":"active"})).await.unwrap();}
  (root,one,two,ids[2].clone())
 }
 fn run(rid:&str,actor:&str)->Value {json!({"id":rid,"session_id":"s","agent_id":"shared_default","requester_user_id":actor,"status":"queued","generation":"","input":[]})}
 #[tokio::test]
 async fn concurrent_cross_owner_reservations_enforce_requester_cap_and_retry_after_reopen(){
  let (root,one,two,actor)=setup().await;
  let a=run("first",&actor);let b=run("second",&actor);
  let (a_result,b_result)=tokio::join!(one.reserve_requester_run(&a),two.reserve_requester_run(&b));
  assert_eq!(a_result.is_ok() as u8+b_result.is_ok() as u8,1);
  let (winner,rid)=if a_result.is_ok(){(&one,"first")}else{(&two,"second")};
  // No canonical write: absence cannot safely release an in-flight reservation.
  let fresh=Store::open(root.path()).await.unwrap();
  let reopened=fresh.for_user(winner.current_user_id()).await.unwrap();
  reopened.reserve_requester_run(&run(rid,&actor)).await.unwrap();
  assert!(fresh.for_user(if winner.current_user_id()==one.current_user_id(){two.current_user_id()}else{one.current_user_id()}).await.unwrap().reserve_requester_run(&run("third",&actor)).await.unwrap_err().to_string().contains("Account run limit"));
  let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0").bind(&actor).fetch_one(&fresh.app_pool).await.unwrap();assert_eq!(count,1);
  let mut terminal=run(rid,&actor);terminal["status"]=json!("cancelled");
  reopened.put(&format!("run:{rid}"),&terminal).await.unwrap();
  reopened.reconcile_requester_slots().await.unwrap();
  two.reserve_requester_run(&run("after-terminal",&actor)).await.unwrap();
 }
 #[tokio::test]
 async fn unrelated_owner_runs_do_not_consume_requester_capacity_and_legacy_runs_are_counted(){
  let (_root,one,two,actor)=setup().await;
  let owner_run=run("owner-only",one.current_user_id());
  one.reserve_requester_run(&owner_run).await.unwrap();
  one.put("run:owner-only",&owner_run).await.unwrap();
  two.reserve_requester_run(&run("participant",&actor)).await.unwrap();
  assert!(one.reserve_requester_run(&run("second-participant",&actor)).await.is_err());
  let mut done=run("participant",&actor);done["status"]=json!("cancelled");
  two.put("run:participant",&done).await.unwrap();two.reconcile_requester_slots().await.unwrap();
  let legacy=run("legacy",&actor);one.put("run:legacy",&legacy).await.unwrap();
  assert!(two.reserve_requester_run(&run("blocked-by-legacy",&actor)).await.is_err());
 }

 #[tokio::test]
 async fn interrupt_handoff_transfers_one_slot_atomically_and_retries_same_successor() {
  let (_root,one,_two,_actor)=setup().await;
  let actor=one.current_user_id();
  let original=run("origin",actor);let next=run("successor",actor);
  one.reserve_requester_run(&original).await.unwrap();
  one.put("run:origin",&original).await.unwrap();
  one.transfer_requester_run(&original,&next).await.unwrap();
  one.transfer_requester_run(&original,&next).await.unwrap();
  let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0").bind(actor).fetch_one(&one.app_pool).await.unwrap();
  assert_eq!(count,1);
  assert!(one.transfer_requester_run(&original,&run("wrong-successor",actor)).await.is_err());
  assert!(one.reserve_requester_run(&run("extra",actor)).await.is_err());
 }
}
