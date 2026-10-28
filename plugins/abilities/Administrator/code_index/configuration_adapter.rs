//! Fresh bounded configuration excerpts accompanying a feature-bound source read.
//! Index metadata is untrusted navigation data. Only native fixed configuration
//! aliases appoint runtime reads; private snapshots never mix with live settings.
use webagent_accounts_host::AccountsStore;
use crate::App;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

const MAX_CONFIG_BYTES: u64 = 512 * 1024;
const MAX_POINTERS: usize = 64;

/// Preserve UTF-8 while applying the same byte budget as source excerpts.
fn bounded(text: &str, budget: usize) -> (&str, bool) {
    let mut end=text.len().min(budget);
    while !text.is_char_boundary(end){end-=1;}
    (&text[..end],end<text.len())
}

fn pointers(reference:&Value)->Vec<String> {
    let mut result=Vec::new();
    for pointer in reference["pointers"].as_array().into_iter().flatten().filter_map(Value::as_str).take(16) {
        if !pointer.starts_with('/') || pointer.len()>192 {continue;}
        result.push(pointer.to_owned());
        if reference["profile_overrides"]==true {
            if let Some(tail)=pointer.strip_prefix("/chat_common/") {
                for profile in ["chat_desktop","chat_mobile","chat_widget"] {
                    result.push(format!("/{profile}/{tail}"));
                }
            }
        }
    }
    result.truncate(MAX_POINTERS);
    result
}

fn runtime_state(path:&str,published:bool,administrator:bool)->&'static str {
    if !published {"snapshot_only"}
    else if path=="crates/modules/context/context-compaction.json" && !administrator {"not_permitted"}
    else {"read"}
}

async fn read_json(path:&std::path::Path)->Result<(Value,String)> {
    // A checkout-relative path is the current live file; an absolute one is a fixture.
    let bytes=webagent_configuration::live::read_path(path,MAX_CONFIG_BYTES,|_|Ok(()))?;
    let value=serde_json::from_slice(&bytes)?;
    Ok((value,format!("{:x}",Sha256::digest(&bytes))))
}

pub(crate) fn snippet_bytes(source:&Value)->usize {
    source["configuration_snippets"].as_array().into_iter().flatten()
        .filter_map(|entry|entry["text"].as_str()).map(str::len).sum()
}

/// Called only inside an already-authorized Code Index read. Fresh account
/// status and administrator scope additionally constrain configuration access.
pub(crate) async fn attach(app:&App,args:&Value,result:&mut Value,cancel:&CancellationToken)->Result<()> {
    if args["action"]!="snippet" || result["status"]!="success" {return Ok(());}
    if cancel.is_cancelled(){bail!("Configuration read cancelled");}
    let Some(source)=result["results"].as_array_mut().and_then(|rows|rows.first_mut()) else{return Ok(());};
    if source["stale"]==true{return Ok(());}
    let Some(refs)=source["configuration"]["references"].as_array().cloned() else{return Ok(());};
    if refs.is_empty(){return Ok(());}
    let account=app.store.account_by_id(app.store.current_user_id()).await?;
    let principal=account.as_ref().map(crate::access::Principal::from_account).transpose()?;
    if !principal.as_ref().is_some_and(|p|p.active()){bail!("Configuration caller unavailable");}
    let administrator=principal.as_ref().is_some_and(|p|p.administrator());
    let code_bytes=source["text"].as_str().map(str::len).unwrap_or(0);
    let budget=args["max_chars"].as_u64().unwrap_or(12000).min(12000) as usize;
    let mut remaining=budget.saturating_sub(code_bytes+snippet_bytes(source));
    let published=args["root"]=="published";
    let mut snippets=source["configuration_snippets"].as_array().cloned().unwrap_or_default();
    for reference in refs.iter().take(4) {
        let Some(path)=reference["path"].as_str() else{continue;};
        if !matches!(path,"ui/chat/chat.json"|"crates/modules/context/context-compaction.json") {
            // The worker resolves other indexed source JSON; these two live files are read
            // by native readers that apply the caller's administrator scope.
            if !snippets.iter().any(|entry|entry["path"]==path) {
                snippets.push(json!({"path":path,"status":"unavailable","reason":"not_indexed_or_supported_runtime_alias"}));
            }
            continue;
        }
        let state=runtime_state(path,published,administrator);
        if state!="read" {snippets.push(json!({"path":path,"status":state}));continue;}
        let actual=if path.ends_with("context-compaction.json"){&app.compaction_config_path}else{&app.chat_ui_config_path};
        let load=read_json(actual);
        let loaded=tokio::select! {
            _=cancel.cancelled()=>bail!("Configuration read cancelled"),
            loaded=tokio::time::timeout(std::time::Duration::from_secs(2),load)=>loaded,
        };
        let Ok(Ok((value,sha256)))=loaded else {
            snippets.push(json!({"path":path,"status":"unavailable","reason":"bounded_read_failed"}));continue;
        };
        let requested=pointers(reference);
        let count=requested.len().max(1);
        let per_pointer=remaining/count;
        for pointer in requested {
            let mut item=json!({"path":path,"runtime_path":actual.to_string_lossy(),"pointer":pointer,"sha256":sha256,"read_at":crate::now(),"source_kind":"untrusted_configuration"});
            if let Some(selected)=value.pointer(&pointer) {
                let rendered=serde_json::to_string_pretty(selected)?;
                let (text,truncated)=bounded(&rendered,per_pointer.min(remaining));
                item["status"]=json!(if text.is_empty(){"budget_exhausted"}else{"set"});
                item["text"]=json!(text);item["truncated"]=json!(truncated);
                remaining=remaining.saturating_sub(text.len());
            } else {item["status"]=json!("unset");}
            snippets.push(item);
        }
    }
    // Re-read principal before exposing data, including administrator downgrade.
    let fresh=app.store.account_by_id(app.store.current_user_id()).await?;
    let fresh=fresh.as_ref().map(crate::access::Principal::from_account).transpose()?;
    if cancel.is_cancelled() || !fresh.as_ref().is_some_and(|p|p.active()) || administrator!=fresh.as_ref().is_some_and(|p|p.administrator()) {
        bail!("Configuration caller changed during read");
    }
    source["configuration_snippets"]=json!(snippets);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn feature_settings_attach_without_an_agent_turn_and_respect_scope() {
        let (root,mut app)=crate::runtime_tests::fixture().await;
        app.chat_ui_config_path=root.path().join("chat.json");
        app.compaction_config_path=root.path().join("context.json");
        tokio::fs::write(&app.chat_ui_config_path,br#"{"chat_common":{"message_bubbles":{"user":{"bg_dark":"blue"}}},"chat_desktop":{"message_bubbles":{"user":{"bg_dark":"red"}}}}"#).await.unwrap();
        tokio::fs::write(&app.compaction_config_path,br#"{"trigger_tokens":4096}"#).await.unwrap();
        let original=json!({"status":"success","results":[{"text":"paint()","stale":false,"configuration":{"status":"configured","references":[
            {"path":"ui/chat/chat.json","pointers":["/chat_common/message_bubbles/user/bg_dark"],"profile_overrides":true},
            {"path":"crates/modules/context/context-compaction.json","pointers":["/trigger_tokens"]},
            {"path":root.path().join("forbidden.json").to_string_lossy(),"pointers":["/secret"]}
        ]}}]});
        let args=json!({"action":"snippet","root":"published","max_chars":512});
        let cancel=CancellationToken::new();
        let mut result=original.clone();
        attach(&app,&args,&mut result,&cancel).await.unwrap();
        let rows=result["results"][0]["configuration_snippets"].as_array().unwrap();
        assert_eq!(rows[0]["text"],json!("\"blue\""));
        assert_eq!(rows[1]["text"],json!("\"red\""));
        assert_eq!(rows[2]["status"],"unset");
        assert_eq!(rows[4]["text"],"4096");
        assert_eq!(rows[5]["status"],"unavailable");
        assert!(snippet_bytes(&result["results"][0])+7<=512);
        assert!(rows[0]["runtime_path"].as_str().unwrap().ends_with("chat.json"));
        let hash=rows[0]["sha256"].clone();
        tokio::fs::write(&app.chat_ui_config_path,br#"{"chat_common":{"message_bubbles":{"user":{"bg_dark":"green"}}}}"#).await.unwrap();
        let mut fresh=original.clone();
        attach(&app,&args,&mut fresh,&cancel).await.unwrap();
        assert_eq!(fresh["results"][0]["configuration_snippets"][0]["text"],json!("\"green\""));
        assert_ne!(fresh["results"][0]["configuration_snippets"][0]["sha256"],hash);
        let mut private=original.clone();
        attach(&app,&json!({"action":"snippet","root":"webagent-v3","max_chars":512}),&mut private,&cancel).await.unwrap();
        assert_eq!(private["results"][0]["configuration_snippets"][0]["status"],"snapshot_only");
        assert_eq!(snippet_bytes(&private["results"][0]),0);
        let account=app.store.create_user_account("config_reader","fixture-password-hash-only").await.unwrap();
        let user=account["user_id"].as_str().unwrap();
        app.store.update_account_access(user,1,crate::access::AccountStatus::Active,crate::access::Role::User,"basic").await.unwrap();
        app.store=app.store.for_user(user).await.unwrap();
        let mut ordinary=original.clone();
        attach(&app,&args,&mut ordinary,&cancel).await.unwrap();
        assert_eq!(ordinary["results"][0]["configuration_snippets"][4]["status"],"not_permitted");
        app.store.update_account_access(user,2,crate::access::AccountStatus::Disabled,crate::access::Role::User,"basic").await.unwrap();
        assert!(attach(&app,&args,&mut original.clone(),&cancel).await.is_err());
        cancel.cancel();
        assert!(attach(&app,&args,&mut original.clone(),&cancel).await.is_err());
    }

    #[test]
    fn runtime_access_respects_snapshot_and_administrator_scope() {
        assert_eq!(runtime_state("ui/chat/chat.json",true,false),"read");
        assert_eq!(runtime_state("crates/modules/context/context-compaction.json",true,false),"not_permitted");
        assert_eq!(runtime_state("crates/modules/context/context-compaction.json",true,true),"read");
        assert_eq!(runtime_state("ui/chat/chat.json",false,true),"snapshot_only");
    }
    #[tokio::test]
    async fn current_json_hash_changes_and_oversized_or_invalid_files_fail() {
        let path=std::env::temp_dir().join(format!("code-index-config-{}.json",uuid::Uuid::new_v4()));
        tokio::fs::write(&path,br#"{"color":"blue"}"#).await.unwrap();
        let (before,hash)=read_json(&path).await.unwrap();
        assert_eq!(before.pointer("/color"),Some(&json!("blue")));
        assert!(before.pointer("/missing").is_none());
        tokio::fs::write(&path,br#"{"color":"red"}"#).await.unwrap();
        let (after,next_hash)=read_json(&path).await.unwrap();
        assert_eq!(after.pointer("/color"),Some(&json!("red")));
        assert_ne!(hash,next_hash);
        tokio::fs::write(&path,b"invalid JSON").await.unwrap();
        assert!(read_json(&path).await.is_err());
        tokio::fs::write(&path,vec![b' ';MAX_CONFIG_BYTES as usize+1]).await.unwrap();
        assert!(read_json(&path).await.is_err());
        tokio::fs::remove_file(&path).await.unwrap();
    }
    #[test]
    fn utf8_budget_and_profile_pointers() {
        assert_eq!(bounded("🙂 blue",3),("",true));
        assert_eq!(bounded("🙂 blue",5),("🙂 ",true));
        let value=json!({"pointers":["/chat_common/message_bubbles/user/bg_dark"],"profile_overrides":true});
        assert_eq!(pointers(&value),vec!["/chat_common/message_bubbles/user/bg_dark","/chat_desktop/message_bubbles/user/bg_dark","/chat_mobile/message_bubbles/user/bg_dark","/chat_widget/message_bubbles/user/bg_dark"]);
        assert_eq!(snippet_bytes(&json!({"configuration_snippets":[{"text":"🙂"},{"status":"unset"},{"text":"red"}]})),7);
    }
}
