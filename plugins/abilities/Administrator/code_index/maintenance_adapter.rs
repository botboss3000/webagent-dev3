//! Shared Code Index maintenance uses broker authority, never caller-selected paths.
//! API and loop receive active guidance through read-only assets; tool grants remain image-pinned.
use crate::{App,Value,json,CancellationToken,Result};
use anyhow::{bail,anyhow};
use std::time::Duration;
/// Normalize action fields; snapshot roots can never perform mutations.
pub(crate) fn arguments(args:&Value)->Result<Value>{
    let input=args.as_object().ok_or_else(||anyhow!("Maintenance object required"))?;
    const KNOWN:&[&str]=&["action","root","task_id","task_key","scope","status","feature","offset","limit","entry_id","request_id","expected_revision","source_hash","checks","remaining_work","reconciliation","claim_id","configuration_assessment"];
    if input.keys().any(|key|!KNOWN.contains(&key.as_str())){bail!("Unknown maintenance argument");}
    let action=args["action"].as_str().ok_or_else(||anyhow!("Maintenance action required"))?;
    let fields:&[&str]=match action{
        "list"=>&["root","task_id","task_key","scope","status","feature","offset","limit"],
        "get"=>&["root","entry_id","offset","limit"],
        "sync"=>&["root","task_id","task_key","scope","request_id"],
        "claim"=>&["root","task_id","task_key","entry_id","expected_revision","request_id","claim_id"],
        "release"=>&["root","task_id","task_key","entry_id","expected_revision","request_id","remaining_work","claim_id"],
        "record"=>&["root","task_id","task_key","entry_id","expected_revision","request_id","source_hash","status","checks","remaining_work","reconciliation","claim_id","configuration_assessment"],
        _=>bail!("Unknown maintenance action"),
    };
    let mut output=serde_json::Map::new();output.insert("action".into(),json!(action));
    for key in fields{if let Some(value)=input.get(*key).filter(|v|!v.is_null()){output.insert((*key).into(),value.clone());}}
    output.entry("root").or_insert(json!("live"));
    if output.get("root")!=Some(&json!("live")) && !matches!(action,"list"|"get"){bail!("Maintenance snapshots are read-only; select an isolated broker task");}
    Ok(Value::Object(output))
}
/// Dispatch through native authority and renew the broker lease during long scans.
pub(crate) async fn execute(app:&App,sid:&str,run_id:&str,args:&Value,cancel:CancellationToken)->Result<Value>{
    let args=arguments(args)?;
    let name="code_index_maintenance";
    let fingerprint=crate::tools::administrator::host_fingerprint(app,sid,run_id,name).await?;
    if args["root"]!="live"{
        let allowed=["action","root","scope","status","feature","offset","limit","entry_id"];
        let worker_args=Value::Object(args.as_object().unwrap().iter().filter(|(k,_)|allowed.contains(&k.as_str())).map(|(k,v)|(k.clone(),v.clone())).collect());
        let result=crate::tools::execute_trusted(name,&worker_args,sid,&app.tool_socket,None,cancel.clone()).await?;
        if cancel.is_cancelled()||crate::tools::administrator::host_fingerprint(app,sid,run_id,name).await?!=fingerprint{bail!("Maintenance authority changed during read");}
        return Ok(result)
    }
    let trusted=||json!({"user_id":crate::shared_chat::actor(app),"session_id":sid,"lease_until":crate::now()+30,"access_fingerprint":fingerprint});
    let socket=crate::tools::administrator::build_socket(app);
    let future=crate::tools::execute_host_trusted(name,&args,sid,&socket,Some(trusted()),cancel.clone());
    tokio::pin!(future);
    loop{tokio::select!{
        result=&mut future=>{
            let result=result?;
            if cancel.is_cancelled()||crate::tools::administrator::host_fingerprint(app,sid,run_id,name).await?!=fingerprint{bail!("Maintenance authority changed; inspect the existing request");}
            return Ok(result)
        },
        _=cancel.cancelled()=>{
            let _=crate::tools::execute_host_trusted("administrator_host_revoke",&json!({}),sid,&socket,Some(trusted()),CancellationToken::new()).await;
            bail!("Maintenance interrupted; inspect the original request before retrying")
        },
        _=tokio::time::sleep(Duration::from_secs(5))=>{
            let current=crate::tools::administrator::host_fingerprint(app,sid,run_id,name).await;
            if current.as_ref().ok()!=Some(&fingerprint){
                let _=crate::tools::execute_host_trusted("administrator_host_revoke",&json!({}),sid,&socket,Some(trusted()),CancellationToken::new()).await;
                bail!("Maintenance authority revoked")
            }
            crate::tools::execute_host_trusted("administrator_host_lease",&json!({}),sid,&socket,Some(trusted()),cancel.clone()).await?;
        }
    }}
}
#[cfg(test)]
mod tests{
 use super::*;
 #[test]fn actions_cannot_smuggle_identity_or_unused_mutations(){
  assert!(arguments(&json!({"action":"sync","user_id":"other"})).is_err());
  let a=arguments(&json!({"action":"list","root":null,"task_id":null,"checks":{"documentation":"verified"}})).unwrap();
  assert_eq!(a,json!({"action":"list","root":"live"}));
 }
 #[test]fn private_and_published_snapshots_cannot_be_written(){
  assert!(arguments(&json!({"action":"record","root":"published"})).is_err());
  assert!(arguments(&json!({"action":"sync","root":"webagent-v3"})).is_err());
  assert!(arguments(&json!({"action":"get","root":"published","entry_id":"e"})).is_ok());
 }
}
