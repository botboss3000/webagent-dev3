//! Atomic cross-session queue admission and owner-controlled route revocation.
use super::*;
use anyhow::bail;
impl Store {
 /// Commit an authorized send and optional new session in the owner's user transaction.
 pub(crate) async fn messaging_commit(&self,source:&str,run:&Value,pending:&Value,entry:Value,docs:Vec<(String,Value)>,created:&Value,receipt_key:&str,input_digest:&str,result:&Value,remember:&Value)->Result<()>{
  let _guard=self.context_gate.write().await;
  let mut tx=self.pool.begin().await?;
  sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(format!("session:{source}")).execute(&mut *tx).await?;
  if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?").bind(receipt_key).fetch_optional(&mut *tx).await?{
   let r=self.unseal(&raw)?;if r["digest"]!=input_digest{bail!("Message identity conflict");}return Ok(());
  }
  self.require_active_session(&mut tx,source).await?;
  let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("run:{}",run["id"].as_str().unwrap())).fetch_one(&mut *tx).await?;
  let current=self.decode_document(&mut tx,&raw).await?;
  if current["generation"]!=run["generation"]||current["status"]!="running"||current["run_revision"]!=run["run_revision"]{bail!("Sender execution changed");}
  self.require_participation(&mut tx,run).await?;
  self.followup_lease_guard(&mut tx,run,&current).await?;
  let target=entry["session_id"].as_str().unwrap();
  if !created.is_null(){
   self.require_spawn_capacity(&mut tx,source,&entry["agent_message"]["spawn_limits"]).await?;
   let exists:i64=sqlx::query_scalar("SELECT count(*) FROM docs WHERE key=?").bind(format!("session:{target}")).fetch_one(&mut *tx).await?;
   if exists!=0{bail!("Destination already exists");}
   let sessions=self.list("session:").await?;
   let access=self.live_access(self.current_user_id(),entry["agent_id"].as_str().unwrap()).await?;
   if sessions.iter().filter(|s|s["status"]!="deleted").count()>=access.max_sessions() as usize{bail!("Session limit reached");}

   sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'session_created',?)").bind(target).bind(self.encode_user_record(created)?).execute(&mut *tx).await?;
  }
  let mut touched=docs.iter().map(|(k,_)|k.clone()).collect::<Vec<_>>();
  for(k,v)in docs{self.write_document(&mut tx,&k,&v).await?;}
  self.require_active_session(&mut tx,target).await?;
  if !created.is_null(){self.stage_notification(&mut tx,&format!("created:{target}"),"session.created",target,None,None).await?;}
  self.validate_agent_delivery_tx(&mut tx,&entry).await?;
  let mut q=self.queue_read_tx(&mut tx,target).await?;
  if q["entries"].as_array().unwrap().iter().filter(|e|!matches!(e["status"].as_str(),Some("delivered"|"cancelled"|"rejected"))).count()>=256{bail!("Followup queue is full");}
  if let Some(k)=entry["messaging_route"]["key"].as_str(){
   let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(k).fetch_one(&mut *tx).await?;
   let r=self.unseal(&raw)?;if r["revoked"]==true||r["revision"]!=entry["messaging_route"]["revision"]{bail!("Route changed");}
  }
  if entry["choice"]=="interrupt"{
   let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("run:{}",entry["target_run_id"].as_str().unwrap())).fetch_one(&mut *tx).await?;
   let r=self.unseal(&raw)?;if r["generation"]!=entry["interrupt_generation"]||r["status"]!="running"{bail!("Interrupt target changed");}
  }
  let id=entry["id"].as_str().unwrap();
  if q["entries"].as_array().unwrap().iter().any(|e|e["id"]==id){bail!("Message identity already exists");}
  let exists:i64=sqlx::query_scalar("SELECT count(*) FROM chat WHERE id=?").bind(format!("user:{id}")).fetch_one(&mut *tx).await?;if exists!=0{bail!("Message identity already exists");}
  let meta=json!({"author_id":self.current_user_id(),"author_name":format!("Message from {} · {}",entry["agent_message"]["source_agent_id"].as_str().unwrap_or("agent"),entry["agent_message"]["source_title"].as_str().unwrap_or("conversation")),"message_kind":"agent_message","agent_message":entry["agent_message"],"context_eligible":false,"queued":true,"context_content":entry["context_content"]});
  touched.push(format!("message_meta:{id}"));
  self.write_document(&mut tx,&format!("message_meta:{id}"),&meta).await?;
  self.insert_chat(&mut tx,target,id,"user",&entry["content"]).await?;
  q["entries"].as_array_mut().unwrap().push(entry.clone());
  self.queue_write_tx(&mut tx,&mut q).await?;
  if remember.is_object(){
   let k=remember["key"].as_str().unwrap();
   let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(k).fetch_optional(&mut *tx).await?;
   let revision=raw.map(|v|self.unseal(&v)).transpose()?.map(|r|r["revision"].as_u64().unwrap_or(0)).unwrap_or(0)+1;
   let mut grant=remember.clone();grant["revision"]=json!(revision);
   self.write_document(&mut tx,k,&grant).await?;
  }
  self.write_document(&mut tx,receipt_key,&json!({"digest":input_digest,"result":result})).await?;
  if pending["name"]=="delegate_to_agent" {
   let key=format!("module_state:core_ui:{source}");
   let previous:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_optional(&mut *tx).await?;
   let revision=previous.map(|v|self.unseal(&v)).transpose()?.and_then(|v|v["revision"].as_u64()).unwrap_or(0)+1;
   let request=json!({"revision":revision,"state":{"request_id":crate::digest(&json!([source,run["id"],pending["id"]])),"actor_id":self.current_user_id(),"agent_id":entry["agent_id"],"destination":"handoff","target_session_id":target,"session_id":source,"expires_at":crate::now()+120}});
   sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(&key).bind(self.seal(&request)?).execute(&mut *tx).await?;
   touched.push(key);
  }
  tx.commit().await?;
  self.context_cache.lock().unwrap().invalidate(&self.context_cache_key(target));
  let mut keys=touched;keys.extend([format!("session:{target}"),receipt_key.to_owned()]);if let Some(k)=remember["key"].as_str(){keys.push(k.to_owned());}
  self.cache.lock().unwrap().invalidate(&keys);
  Ok(())
 }
 /// Capacity is checked under the same SQLite writer as child creation, including competing loop processes.
 async fn require_spawn_capacity(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,source:&str,limits:&Value)->Result<()> {
  if limits.is_null(){return Ok(());}
  let depth=limits["depth"].as_u64().ok_or_else(||anyhow!("Helper depth missing"))?;
  let max_depth=limits["max_depth"].as_u64().filter(|v|*v<=5).ok_or_else(||anyhow!("Invalid helper depth limit"))?;
  let max_children=limits["max_children"].as_u64().filter(|v|*v<=10).ok_or_else(||anyhow!("Invalid helper breadth limit"))?;
  let max_active=limits["max_active"].as_u64().filter(|v|(1..=20).contains(v)).ok_or_else(||anyhow!("Invalid helper capacity"))?;
  if depth>max_depth{bail!("Helper recursion depth reached");}
  let prefix=format!("receipt:agent_message:{source}:");
  let rows=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE substr(key,1,length(?))=? LIMIT 1001").bind(&prefix).bind(&prefix).fetch_all(&mut **tx).await?;
  if rows.len()>1000{bail!("Parent helper history quota reached");}
  let mut total=0u64;let mut active=0u64;
  for raw in rows {
   let receipt=self.unseal(&raw)?;
   if receipt["result"]["session_created"]!=true{continue;}
   total+=1;
   let id=receipt["result"]["session_id"].as_str().ok_or_else(||anyhow!("Child identity missing"))?;
   let runs:i64=sqlx::query_scalar("SELECT count(*) FROM run_index WHERE session_id=? AND status NOT IN ('completed','cancelled','failed','stopped')").bind(id).fetch_one(&mut **tx).await?;
   let q=self.queue_read_tx(tx,id).await?;
   if runs>0||q["entries"].as_array().into_iter().flatten().any(|v|matches!(v["status"].as_str(),Some("pending"|"reviewing"|"held"))){active+=1;}
  }
  if active>=max_active||depth>0&&total>=max_children{bail!("Helper capacity reached");}
  Ok(())
 }
 /// Recheck owner-plane source, recipient mode and route inside the consuming writer transaction.
 pub(super) async fn validate_agent_delivery_tx(&self,tx:&mut sqlx::Transaction<'_,sqlx::Sqlite>,e:&Value)->Result<()>{
  if !e["agent_message"].is_object(){return Ok(());}
  let b=&e["agent_message"];
  for (agent,revision) in [("source_agent_id","source_access_revision"),("target_agent_id","target_access_revision")]{
   let access=self.live_access(self.current_user_id(),b[agent].as_str().ok_or_else(||anyhow!("Delegation agent missing"))?).await?;
   if !access.admitted()||json!(access.revision)!=b[revision]{bail!("Delegation access changed");}
  }
  let source=b["source_session_id"].as_str().ok_or_else(||anyhow!("Source missing"))?;
  self.require_active_session(tx,source).await?;
  for(sid,expected)in [(source,&b["source_agent_id"]),(b["target_session_id"].as_str().unwrap_or(""),&b["target_agent_id"])]{
   let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("session:{sid}")).fetch_one(&mut **tx).await?;
   let s=self.unseal(&raw)?;let epoch=if sid==source{&b["source_execution_epoch"]}else{&b["target_execution_epoch"]};if s["agent_id"]!=*expected||json!(s["execution_epoch"].as_u64().unwrap_or(0))!=*epoch{bail!("Session agent or lifecycle changed");}
  }
  let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(format!("session_mode:{}",b["target_session_id"].as_str().unwrap())).fetch_optional(&mut **tx).await?;
  let mode=raw.map(|v|self.unseal(&v)).transpose()?.unwrap_or_else(crate::default_session_mode);
  if mode!=b["target_mode_raw"]{bail!("Recipient mode changed");}
  if let Some(k)=e["messaging_route"]["key"].as_str(){
   let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(k).fetch_one(&mut **tx).await?;
   let r=self.unseal(&raw)?;if r["revoked"]==true||r["revision"]!=e["messaging_route"]["revision"]{bail!("Route revoked");}
  }
  Ok(())
 }
 /// Cancel withdrawn authority even when an interrupt choice is locked against human edits.
 pub(crate) async fn cancel_agent_delivery(&self,sid:&str,id:&str,revision:u64)->Result<()>{
  let mut tx=self.pool.begin().await?;self.queue_reserve(&mut tx,sid).await?;
  let mut q=self.queue_read_tx(&mut tx,sid).await?;
  if let Some(e)=q["entries"].as_array_mut().unwrap().iter_mut().find(|e|e["id"]==id&&e["revision"]==revision&&e["agent_message"].is_object()&&matches!(e["status"].as_str(),Some("pending"|"reviewing"|"held"))){
   e["status"]=json!("cancelled");e["choice"]=json!("cancel");e["revision"]=json!(revision+1);e["review_status"]=json!("cancelled");
   e["reason"]=json!("Messaging authority or recipient changed");
   self.legacy_message_meta(&mut tx,e,false).await?;
   self.queue_write_tx(&mut tx,&mut q).await?;
  }
  tx.commit().await?;Ok(())
 }
 /// Revision-fenced revocation and its retry receipt share the owner transaction.
 pub(crate) async fn revoke_messaging_route(&self,source:&str,target:&str,revision:u64,request:&str)->Result<Value>{
  let key=format!("messaging_route:{source}:{target}");let receipt=format!("receipt:messaging_route:{source}:{request}");
  let signature=json!([target,revision]);let mut tx=self.pool.begin().await?;
  sqlx::query("UPDATE docs SET key=key WHERE key=?").bind(&key).execute(&mut *tx).await?;
  if let Some(raw)=sqlx::query_scalar::<_,Vec<u8>>("SELECT value FROM docs WHERE key=?").bind(&receipt).fetch_optional(&mut *tx).await?{
   let r=self.unseal(&raw)?;if r["signature"]!=signature{bail!("Route request reused");}return Ok(r["result"].clone());
  }
  self.require_active_session(&mut tx,source).await?;
  let raw:Vec<u8>=sqlx::query_scalar("SELECT value FROM docs WHERE key=?").bind(&key).fetch_one(&mut *tx).await?;
  let mut r=self.unseal(&raw)?;if r["revision"]!=revision{bail!("Route revision changed");}
  r["revoked"]=json!(true);r["revision"]=json!(revision+1);
  self.write_document(&mut tx,&key,&r).await?;
  self.write_document(&mut tx,&receipt,&json!({"signature":signature,"result":r})).await?;
  tx.commit().await?;self.cache.lock().unwrap().invalidate(&[key,receipt]);Ok(r)
 }
}

#[cfg(test)]mod core_spawn_tests {
 use super::*;
 #[tokio::test]async fn transaction_capacity_counts_queued_children_and_frees_settled_slots(){
  let (_root,app)=crate::runtime_tests::fixture_uninitialized().await;
  app.store.put("receipt:agent_message:parent:first",&json!({"result":{"session_created":true,"session_id":"child"}})).await.unwrap();
  app.store.put("followup_queue:child",&json!({"schema_version":1,"session_id":"child","revision":0,"held":false,"entries":[{"status":"pending"}]})).await.unwrap();
  let limits=json!({"depth":0,"max_depth":2,"max_children":1,"max_active":1});
  {let mut tx=app.store.pool.begin().await.unwrap();assert!(app.store.require_spawn_capacity(&mut tx,"parent",&limits).await.is_err());}
  app.store.put("followup_queue:child",&json!({"schema_version":1,"session_id":"child","revision":1,"held":false,"entries":[]})).await.unwrap();
  {let mut tx=app.store.pool.begin().await.unwrap();assert!(app.store.require_spawn_capacity(&mut tx,"parent",&limits).await.is_ok());}
  let mut nested=limits.clone();nested["depth"]=json!(1);
  {let mut tx=app.store.pool.begin().await.unwrap();assert!(app.store.require_spawn_capacity(&mut tx,"parent",&nested).await.is_err());}
  nested["depth"]=json!(3);
  {let mut tx=app.store.pool.begin().await.unwrap();assert!(app.store.require_spawn_capacity(&mut tx,"parent",&nested).await.is_err());}
 }
}
