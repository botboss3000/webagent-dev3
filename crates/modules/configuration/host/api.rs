//! Revision-fenced administrator updates to existing application configuration.
use super::*;
use anyhow::Context;
use webagent_configuration::{digest, read};
const IDS:[&str;9]=["app_settings","chat","prompts","context_compaction","loop_runtime","tool_output_retention","access_policy","message_visibility","closing_status"];
/// Map a known configuration owner to its existing runtime path; never accept a caller filesystem path.
fn path(app:&App,id:&str)->Result<PathBuf>{
    Ok(match id{
        "context_compaction"=>app.compaction_config_path.clone(),
        "loop_runtime"=>app.runtime_path.clone(),
        "chat"=>app.chat_ui_config_path.clone(),
        "app_settings"=>app.store.root.join("config/app-settings.json"),
        "prompts"=>app.store.root.join("config/prompts.json"),
        "tool_output_retention"=>app.store.root.join("config/tool-output-retention.json"),
        "access_policy"=>app.store.root.join("config/access-policy.json"),
        "message_visibility"=>app.store.root.join("config/gates/message_visibility_v1.json"),
        "closing_status"=>app.store.root.join("config/gates/closing_status_v1.json"),
        _=>anyhow::bail!("Unknown configuration")
    })
}
/// Reuse each owner's domain validator before an administrator save.
pub(crate) fn validate(id:&str,value:&Value)->Result<()> {
    if !value.is_object(){anyhow::bail!("Configuration must be an object");}
    match id{
        "app_settings"=>{crate::store::admin_encryption_enabled(value)?;
            webagent_webapp::configuration::validate_app_settings(value)?;
        },
        "chat"=>{
            for key in ["max_file_size_mb","max_per_message"]{
                let v=&value["chat_limits"]["attachments"][key];
                if !v.is_null()&&v.as_u64().is_none_or(|n|n==0||usize::try_from(n).is_err()||(key=="max_file_size_mb"&&n.checked_mul(1024*1024).is_none())){anyhow::bail!("Invalid attachment limit");}
            }
            for v in [&value["chat_limits"],&value["chat_limits"]["attachments"]]{if !v.is_null()&&!v.is_object(){anyhow::bail!("Invalid chat limits");}}
        },
        "loop_runtime"=>serde_json::from_value::<loop_runtime::Config>(value.clone())?.validate()?,
        "context_compaction"=>serde_json::from_value::<context_compaction::Config>(value.clone())?.validate()?,
        "tool_output_retention"=>serde_json::from_value::<tool_output::Policy>(value.clone())?.validate()?,
        "access_policy"=>crate::registration::validate_configuration(value)?,
        "message_visibility"|"closing_status"=>{
            let expected=if id=="message_visibility"{gate_registry::MESSAGE_VISIBILITY}else{gate_registry::CLOSING_STATUS};
            let manifest:gate_registry::Manifest=serde_json::from_value(value.clone())?;
            gate_registry::validate_manifest(expected,&manifest)?;
            // The integral gate validator checks both config and fixed tool binding.
        },
        "prompts"=>validate_prompts(value)?,
        _=>anyhow::bail!("Unknown configuration")
    }Ok(())
}
/// Validate base strings and optional naming, manager, and retention sections without exposing their values.
/// Published source sections cannot borrow missing naming fields from the current publication.
pub(crate) fn validate_source_prompts(value:&Value)->Result<()> {
    if let Some(naming)=value.get("session_naming") {crate::session_naming::validate_source_settings(naming)?;}
    validate_prompts(value)
}
pub(crate) fn validate_prompts(value:&Value)->Result<()>{
    let object=value.as_object().ok_or_else(||anyhow!("Invalid prompts"))?;
    let placeholder=value["tool_result_deduplication"]["placeholder"].as_str().context("Required duplicate-result placeholder missing")?;
    webagent_context_contracts::tool_results::validate_template(placeholder)?;
    for key in ["base_instructions","closing_reporting","jev_guidance"] {
        if value[key].as_str().is_none_or(|s|s.trim().is_empty()||s.len()>65536){anyhow::bail!("Invalid base prompt");}
    }
    for (key,v) in object {
        if let Some(text)=v.as_str(){if text.len()>65536{anyhow::bail!("Prompt exceeds limit");}}
        else if !v.is_object(){anyhow::bail!("Invalid prompt section");}
        match key.as_str(){
            "session_naming"=>crate::session_naming::validate_settings(v)?,
            "manager_incoming_message"=>if v.as_object().is_none_or(|m|m.values().any(|v|v.as_str().is_none_or(|s|s.len()>8192))){anyhow::bail!("Invalid manager prompts");},
            "tool_output_retention"=>for key in ["guidance","placeholder"]{if v[key].as_str().is_none_or(|s|s.trim().is_empty()||s.len()>8192){anyhow::bail!("Invalid retention prompt");}},
            _=>{}
        }
    }Ok(())
}
/// Describe supported configuration owners and the boundaries at which their consumers adopt updates.
pub(crate) async fn list(State(_app):State<App>)->Api{
    Ok(Json(json!({"configurations":IDS.iter().map(|id|json!({"id":id,"adoption":if *id=="loop_runtime"{"next_scheduler_tick"}else if *id=="prompts"{"next_model_call_or_new_job"}else{"next_request_or_checkpoint"}})).collect::<Vec<_>>(),
        "diagnostics":"Use the existing package-owned diagnostics configuration API",
        "infrastructure":"Use the protected operator reconfiguration workflow"})))
}
/// Return the saved configuration with a canonical digest for stale-write protection.
pub(crate) async fn get_config(State(app):State<App>,Path(id):Path<String>)->Api{
    let value=read(&path(&app,&id).map_err(|_|bad("Unknown configuration"))?)?;
    Ok(Json(json!({"id":id,"config":value,"digest":digest(&value)})))
}
/// Validate and atomically replace configuration under the owning lock only when its digest still matches.
pub(crate) async fn put_config(State(app):State<App>,Path(id):Path<String>,Json(body):Json<Value>)->Api{
    let path=path(&app,&id).map_err(|_|bad("Unknown configuration"))?;
    let expected=required(&body,"expected_digest",64)?;
    let value=&body["config"];validate(&id,value).map_err(|_|bad("Invalid configuration"))?;
    let _guard=app.gate.lock().await;
    let lock_path=match id.as_str(){
        "prompts"=>app.store.root.join("config/.prompts.lock"),
        "access_policy"=>app.store.root.join("config/access-policy.lock"),
        _=>path.with_extension("json.publication.lock"),
    };
    let saved=webagent_configuration::update_if_current(&path,&lock_path,&expected,None,|_|Ok(value.clone()))?;
    if saved.is_none(){return Err(conflict("Configuration changed; reload before saving"));}
    if id=="loop_runtime"{crate::wake::notify(&app).await;}
    Ok(Json(json!({"id":id,"digest":digest(value),"restart_required":false})))
}
/// Report only catalog identities and bounded API/loop adoption receipts.
pub(crate) async fn publication(State(app):State<App>)->Api{
    let consumers=["api","loop"].iter().map(|role|{
        let receipt=std::fs::read(app.store.root.join("publication").join(format!("{role}.json"))).ok()
            .and_then(|b|serde_json::from_slice::<Value>(&b).ok()).unwrap_or(Value::Null);
        json!({"role":role,"revision":receipt["revision"],"catalog_version":receipt["catalog_version"]})
    }).collect::<Vec<_>>();
    Ok(Json(json!({"catalog_version":app.abilities.version(),"consumers":consumers,"plugin_protocol":1})))
}
#[cfg(test)]mod tests{
 use super::*;
 #[tokio::test]async fn invalid_configuration_and_stale_digest_preserve_current(){
  let (_root,app)=crate::runtime_tests::fixture().await;
  let before=get_config(State(app.clone()),Path("loop_runtime".into())).await.unwrap().0;
  let mut value=before["config"].clone();value["scheduler"]["max_active_runs"]=json!(5);
  let result=put_config(State(app.clone()),Path("loop_runtime".into()),Json(json!({"expected_digest":before["digest"],"config":value}))).await.unwrap().0;
  assert_eq!(result["restart_required"],false);
  assert!(put_config(State(app.clone()),Path("loop_runtime".into()),Json(json!({"expected_digest":before["digest"],"config":before["config"]}))).await.is_err());
  value["scheduler"]["max_active_runs"]=json!(999);
  assert!(put_config(State(app.clone()),Path("loop_runtime".into()),Json(json!({"expected_digest":result["digest"],"config":value}))).await.is_err());
  assert_eq!(get_config(State(app),Path("loop_runtime".into())).await.unwrap().0["digest"],result["digest"]);
 }
}

#[cfg(test)] mod background_interval_tests {
 use super::*;
 /// The generic saved-file path emits the same adoption hint and rejects invalid timing.
 #[tokio::test] async fn generic_interval_save_notifies_and_preserves_invalid_input() {
  let (_root,app)=crate::runtime_tests::fixture().await;
  let before=get_config(State(app.clone()),Path("loop_runtime".into())).await.unwrap().0;
  let mut value=before["config"].clone();
  value["scheduler"]["background_rescan_interval_ms"]=json!(15000);
  let hints=crate::wake::subscribe();
  let saved=put_config(State(app.clone()),Path("loop_runtime".into()),Json(json!({
   "expected_digest":before["digest"],"config":value}))).await.unwrap().0;
  assert!(hints.has_changed().unwrap());
  assert_eq!(get_config(State(app.clone()),Path("loop_runtime".into())).await.unwrap().0["config"],value);
  value["scheduler"]["background_rescan_interval_ms"]=json!(0);
  assert!(put_config(State(app.clone()),Path("loop_runtime".into()),Json(json!({
   "expected_digest":saved["digest"],"config":value}))).await.is_err());
  assert_eq!(get_config(State(app),Path("loop_runtime".into())).await.unwrap().0["digest"],saved["digest"]);
 }
}
