//! Read bundled guidance by exact catalog ID under fresh caller/agent policy.
//! Loading is evidence only: it never activates an ability or grants execution.
use crate::{App, ability_controls, session_agent_id, session_mode, live_tool_decision};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

pub fn owns(name:&str)->bool {matches!(name,"list_skills"|"load_skill")}
pub fn schemas()->Vec<Value> {
    let id=json!({"type":"string","minLength":1,"maxLength":64,"pattern":"^[a-z0-9_]+$"});
    vec![
        json!({"type":"function","name":"list_skills","description":"List inactive discoverable skills; include_available also shows visible and already loaded guidance",
            "parameters":{"type":"object","properties":{"ability_id":id,"query":{"type":"string","maxLength":200},"include_available":{"type":"boolean"}},"required":["ability_id"],"additionalProperties":false}}),
        json!({"type":"function","name":"load_skill","description":"Load discoverable guidance for this run. Returns only acknowledgment and metadata; full guidance is supplied in the next model context. Does not activate abilities or tools",
            "parameters":{"type":"object","properties":{"ability_id":id,"skill_id":id,
                "kind":{"type":"string","enum":["ability","tool","selectable","own"]},
                "expected_catalog_version":{"type":"string","minLength":64,"maxLength":64,"pattern":"^[a-f0-9]{64}$"}},
                "required":["ability_id","kind","skill_id"],"additionalProperties":false}}),
    ]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs { ability_id:String, #[serde(default)] query:Option<String>, #[serde(default)] include_available:bool }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoadArgs {
    ability_id:String, kind:String, skill_id:String,
    #[serde(default)] expected_catalog_version:Option<String>,
}
fn valid_id(id:&str)->bool {
    !id.is_empty() && id.len()<=64 && id.bytes().all(|b|b.is_ascii_lowercase()||b.is_ascii_digit()||b==b'_')
}
fn skill_matches(skill:&Value, query:&str)->bool {
    let words=crate::abilities::search_words(query);
    if words.is_empty() {return false;}
    let text=format!("{} {} {}",skill["skill_id"].as_str().unwrap_or(""),
        skill["summary"].as_str().unwrap_or(""),
        skill["description"].as_str().unwrap_or(""));
    let candidates=crate::abilities::search_words(&text);
    words.iter().any(|word|candidates.iter().any(|candidate|
        crate::abilities::near_word(word,candidate)))
}
fn validate_load(args:&LoadArgs)->Result<()> {
    if !valid_id(&args.ability_id) || !valid_id(&args.skill_id)
        || !matches!(args.kind.as_str(),"ability"|"tool"|"selectable"|"own")
        || args.kind=="own" && args.ability_id!="base"
        || args.expected_catalog_version.as_ref().is_some_and(|s|s.len()!=64
            || !s.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))) {
        bail!("Invalid bundled skill selector");
    }
    Ok(())
}
struct View {
    app:App, access:crate::access_policy::LiveAccess,
    policy:Value, visibility:Value, state:Value, loaded:Value, read_only:bool,
    own:Value, loaded_skills:Value, version:String, fingerprint:String,
}
async fn view(app:&App,sid:&str,run_id:&str,name:&str)->Result<View> {
    // Resolve the session in the authenticated user's plane before its agent.
    let agent=session_agent_id(app,sid).await?;
    let mut scoped=app.clone();
    scoped.store=app.store.for_agent(&agent).await?;
    let run=scoped.run(run_id).await?;
    if run["session_id"]!=sid || run["agent_id"].as_str().unwrap_or("shared_default")!=agent {
        bail!("Run does not belong to current session and agent");
    }
    let loaded=run["loaded_abilities"].clone();
    let loaded_skills=run["loaded_skills"].clone();
    let (policy,mode_revision,visibility,state)=ability_controls(&scoped,sid).await?;
    let mode=session_mode(&scoped,sid).await?;
    let read_only=mode["mode"]=="read_only";
    let access=scoped.store.live_access(scoped.store.current_user_id(),&agent).await?;
    if !access.admitted() || live_tool_decision(&scoped,&access,name,&policy,
        &visibility,&state,&loaded,read_only).permission==crate::access::Permission::Deny {
        bail!("Bundled skill tool unavailable under current access");
    }
    // Own-skill revision is part of the read fingerprint; a skill edit
    // invalidates outstanding list/load results before their text is returned.
    let own=scoped.store.list_own_skills().await?;
    let bundle_version=scoped.abilities.live_version();
    let version=crate::digest(&json!([bundle_version,own["revision"]]));
    let fingerprint=crate::digest(&json!([version,policy,mode_revision,visibility,state,
        loaded,loaded_skills,mode,access.context_fingerprint()]));
    Ok(View {app:scoped,access,policy,visibility,state,loaded,read_only,own,loaded_skills,version,fingerprint})
}
fn same_asset(left:&Value,right:&Value)->bool {
    left["ability_id"]==right["ability_id"] && left["kind"]==right["kind"]
        && left["skill_id"]==right["skill_id"] && left["sha256"]==right["sha256"]
        && left["revision"]==right["revision"]
}
fn mark_availability(asset:&mut Value,loaded:&Value,tools:&Value) {
    let tool_selected=asset["kind"]=="tool" && tools.as_array().is_some_and(|items|items.iter()
        .any(|item|item.as_str()==Some(format!("tool:{}",asset["skill_id"].as_str().unwrap_or("")).as_str())));
    let available=asset["availability"]=="visible" || asset["availability"]=="activated" || tool_selected
        || loaded.as_array().is_some_and(|items|items.iter().any(|item|same_asset(item,asset)));
    if available && asset["availability"]!="visible" {asset["availability"]=json!("activated");}
    if !available {asset["availability"]=json!("discoverable");}
    asset["requires_activation"]=json!(!available);
}
fn own_asset(record:&Value)->Result<Value> {
    // Own-skill storage currently supports selectable mode only, never pinned text.
    if record["mode"]!="selectable" {bail!("Current-agent skill unavailable");}
    let text=record["instructions"].as_str().context("Invalid own-skill instructions")?;
    use sha2::{Digest,Sha256};
    Ok(json!({"ability_id":"base","kind":"own","skill_id":record["name"],
        "mode":"discoverable","availability":"discoverable","requires_activation":true,
        "revision":record["revision"],"bytes":text.len(),
        "sha256":hex::encode(Sha256::digest(text.as_bytes())),"text":text,
        "source_kind":"untrusted_agent_skill","activates":false}))
}
fn skill_tool_allowed(current:&View,tool:&str)->bool {
    // This selector permits reading tool guidance; it grants no execution state.
    let mut loaded=current.loaded.as_array().cloned().unwrap_or_default();
    loaded.push(json!(format!("tool:{tool}")));
    live_tool_decision(&current.app,&current.access,tool,&current.policy,
        &current.visibility,&current.state,&json!(loaded),current.read_only)
        .permission!=crate::access::Permission::Deny
}
fn load_asset(current:&View,query:&LoadArgs)->Result<Value> {
    let allow_ability=|id:&str|current.access.ability_permission(id)!=crate::access::Permission::Deny;
    let allow_tool=|tool:&str|skill_tool_allowed(current,tool);
    if query.kind=="own" {
        current.app.abilities.bundled_skill_list("base",&current.visibility,
            &current.state,allow_ability,allow_tool)?;
        let record=current.own["skills"].as_array().context("Invalid own-skill list")?
            .iter().find(|record|record["name"]==query.skill_id)
            .context("Current-agent skill unavailable")?;
        own_asset(record)
    } else {
        current.app.abilities.bundled_skill_load(&query.ability_id,&query.kind,
            &query.skill_id,&current.visibility,&current.state,allow_ability,allow_tool)
    }
}
/// Persist selectors only after the caller fences the current native execution.
pub(crate) fn apply_legacy_selection(run:&mut Value,result:&Value) {
    // New acknowledgment selectors were committed with their native result.
    // Import only old full-text evidence, never acknowledgments from another run.
    if result["output"]["text"].is_string() {apply_selection(run,result);}
}
pub(crate) fn apply_selection(run:&mut Value,result:&Value) {
    if run["pending"]["name"]!="load_skill" || result["status"]!="success" {return;}
    let asset=&result["output"];
    let Ok(evidence)=serde_json::from_value::<SelectionEvidence>(asset.clone()) else {return;};
    let args=run["pending"]["arguments"].clone();
    let selector=if let Some(raw)=args.as_str() {serde_json::from_str::<LoadArgs>(raw)}
        else {serde_json::from_value::<LoadArgs>(args)};
    let Ok(selector)=selector else {return;};
    use sha2::{Digest,Sha256};
    if validate_load(&selector).is_err() || evidence.activates
        || evidence.ability_id!=selector.ability_id || evidence.kind!=selector.kind
        || evidence.skill_id!=selector.skill_id || !valid_hash(&evidence.sha256)
        || !match evidence.text.as_deref() {
            // Compatibility for native results persisted before acknowledgment-only loads.
            Some(text)=>evidence.bytes==text.len()
                && evidence.sha256==hex::encode(Sha256::digest(text.as_bytes())),
            // Only the fenced native loader produces this content-free result.
            None=>evidence.status.as_deref()==Some("loaded") && evidence.bytes>0
                && evidence.context_source.as_deref()==Some("live_skill_catalog")
                && evidence.catalog_version.as_deref().is_some_and(valid_hash),
        }
        || !(evidence.source_kind=="untrusted_bundled_guidance" && evidence.kind!="own" && evidence.revision.is_none()
            || evidence.source_kind=="untrusted_agent_skill" && evidence.kind=="own" && evidence.revision.is_some_and(|r|r>0)) {return;}
    let mut selections=run["loaded_skills"].as_array().cloned().unwrap_or_default();
    if selections.iter().any(|item|same_asset(item,asset)) {return;}
    selections.retain(|item| !(item["ability_id"]==asset["ability_id"]
        && item["kind"]==asset["kind"] && item["skill_id"]==asset["skill_id"]));
    let mut selected=json!({"ability_id":asset["ability_id"],"kind":asset["kind"],
        "skill_id":asset["skill_id"],"sha256":asset["sha256"]});
    if let Some(revision)=evidence.revision {selected["revision"]=json!(revision);}
    selections.push(selected);
    run["loaded_skills"]=json!(selections);
}
fn valid_hash(hash:&str)->bool {
    hash.len()==64 && hash.bytes().all(|b|b.is_ascii_digit()||(b'a'..=b'f').contains(&b))
}
#[derive(Deserialize)]
struct SelectionEvidence {
    ability_id:String, kind:String, skill_id:String, sha256:String,
    bytes:usize, source_kind:String, activates:bool,
    #[serde(default)] text:Option<String>,
    #[serde(default)] revision:Option<i64>,
    #[serde(default)] status:Option<String>,
    #[serde(default)] catalog_version:Option<String>,
    #[serde(default)] context_source:Option<String>,
}
fn skill_acknowledgment(asset:&Value,catalog_version:&str)->Value {
    let available=asset["requires_activation"]==false;
    let mut ack=json!({"status":if available {"already_available"} else {"loaded"},
        "ability_id":asset["ability_id"],"kind":asset["kind"],"skill_id":asset["skill_id"],
        "sha256":asset["sha256"],"bytes":asset["bytes"],"source_kind":asset["source_kind"],
        "activates":false,"scope":"run","catalog_version":catalog_version,
        "context_source":"live_skill_catalog","requires_activation":false,
        "availability":if available {asset["availability"].as_str().unwrap_or("activated")} else {"activated"},
        "message":if available {"This guidance is already supplied to the current model context."}
            else {"Guidance loaded for this run and supplied in the next model context."}});
    if let Some(revision)=asset.get("revision") {ack["revision"]=revision.clone();}
    ack
}
/// Rehydrate guidance under current authority at every model boundary.
pub(crate) async fn active_guidance(app:&App,sid:&str,run_id:&str)->Result<String> {
    Ok(context_guidance(app,sid,run_id,false).await?.0)
}
/// Return the exact included selectors beside their rebuilt guidance. Follow-up
/// calls apply their read-only ceiling even if the session normally permits writes.
pub(crate) async fn context_guidance(app:&App,sid:&str,run_id:&str,read_only:bool)->Result<(String,Vec<Value>)> {
    let mut current=match view(app,sid,run_id,"load_skill").await {Ok(view)=>view,Err(_)=>return Ok((String::new(),Vec::new()))};
    current.read_only |= read_only;
    let mut guidance=String::new();
    let mut included=Vec::new();
    for selected in current.loaded_skills.as_array().into_iter().flatten() {
        let Ok(query)=serde_json::from_value::<LoadArgs>(json!({"ability_id":selected["ability_id"],
            "kind":selected["kind"],"skill_id":selected["skill_id"]})) else {continue;};
        if validate_load(&query).is_err() {continue;}
        let Ok(mut asset)=load_asset(&current,&query) else {continue;};
        if !same_asset(selected,&asset) {continue;}
        mark_availability(&mut asset,&Value::Null,&current.loaded);
        if asset["requires_activation"]==false {continue;}
        if let Some(text)=asset["text"].as_str() {
            guidance.push_str(&format!("\n\n## Loaded untrusted guidance: {} / {} / {}\n{}",
                query.ability_id,query.kind,query.skill_id,text));
            included.push(json!({"ability_id":query.ability_id,"kind":query.kind,"skill_id":query.skill_id,
                "bytes":text.len(),"mode":asset["mode"]}));
        }
    }
    if view(app,sid,run_id,"load_skill").await?.fingerprint!=current.fingerprint {
        bail!("Loaded skill access changed during context assembly");
    }
    Ok((guidance,included))
}
async fn work(app:&App,sid:&str,run_id:&str,name:&str,args:&Value)->Result<Value> {
    if !owns(name) {bail!("Unknown bundled skill tool");}
    let current=view(app,sid,run_id,name).await?;
    let allow_ability=|id:&str|current.access.ability_permission(id)!=crate::access::Permission::Deny;
    let allow_tool=|tool:&str|skill_tool_allowed(&current,tool);
    let output=match name {
        "list_skills" => {
            let query:ListArgs=serde_json::from_value(args.clone()).context("Invalid skill-list arguments")?;
            if !valid_id(&query.ability_id) {bail!("Invalid bundled ability ID");}
            let mut skills=current.app.abilities.bundled_skill_list(&query.ability_id,
                &current.visibility,&current.state,allow_ability,allow_tool)?;
            if query.ability_id=="base" {
                for record in current.own["skills"].as_array().context("Invalid own-skill list")? {
                    if record["mode"]=="off" {continue;}
                    let mut metadata=own_asset(record)?;
                    metadata.as_object_mut().unwrap().remove("text");
                    metadata["description"]=record["description"].clone();
                    skills.push(metadata);
                }
            }
            for skill in &mut skills { mark_availability(skill,&current.loaded_skills,&current.loaded); }
            if !query.include_available {skills.retain(|skill|skill["requires_activation"]==true);}
            if let Some(term) = query.query.as_deref().filter(|term| !term.trim().is_empty()) {
                if term.len()>200 {bail!("Skill search query is too long");}
                skills.retain(|skill| skill_matches(skill,term));
            }
            json!({"ability_id":query.ability_id,"skills":skills,"include_available":query.include_available,"catalog_version":current.version,
                "source_kind":"skill_metadata","activates":false})
        }
        "load_skill" => {
            let query:LoadArgs=serde_json::from_value(args.clone()).context("Invalid skill-load arguments")?;
            validate_load(&query)?;
            if query.expected_catalog_version.as_ref().is_some_and(|v|v!=&current.version) {
                bail!("Skill catalog changed; list skills again");
            }
            let mut skill=load_asset(&current,&query)?;
            mark_availability(&mut skill,&current.loaded_skills,&current.loaded);
            skill_acknowledgment(&skill,&current.version)
        }
        _=>unreachable!(),
    };
    // Reject a read that raced a tier, level, membership, mode or bundle change.
    if view(app,sid,run_id,name).await?.fingerprint!=current.fingerprint {
        bail!("Bundled skill access changed during read");
    }
    Ok(output)
}
pub async fn execute(app:&App,sid:&str,run_id:&str,name:&str,args:&Value,cancel:CancellationToken)->Result<Value> {
    if cancel.is_cancelled() {bail!("tool execution cancelled");}
    let result=tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=work(app,sid,run_id,name,args)=>result,
    };
    if cancel.is_cancelled() {bail!("tool execution cancelled");}
    Ok(match result {
        Ok(output)=>json!({"status":"success","output":output}),
        Err(error)=>json!({"status":"error","error_kind":"bundled_skill_rejected","output":error.to_string()}),
    })
}
// run.input is derived recovery context and may persist this masked projection.
// The immutable encrypted interactions journal retains the original evidence.
#[derive(Deserialize)]
struct NativeEvidence {
    ability_id:String, kind:String, skill_id:String, sha256:String,
    bytes:usize, text:String, source_kind:String, activates:bool,
    #[serde(default)]
    revision:Option<i64>,
}
#[derive(Deserialize)]
struct NativeResult { status:String, output:NativeEvidence }
fn mask_history(input:&mut [Value],allow:impl Fn(&NativeEvidence)->bool)->bool {
    use std::collections::BTreeMap;
    use sha2::{Digest,Sha256};
    let mut calls:BTreeMap<String,Option<LoadArgs>>=BTreeMap::new();
    let mut changed=false;
    for item in input {
        if item["type"]=="function_call" {
            let Some(id)=item["call_id"].as_str().filter(|id|!id.is_empty()&&id.len()<=256) else {continue;};
            let selector=if item["name"]=="load_skill" {
                item["arguments"].as_str().filter(|args|args.len()<=1024)
                    .and_then(|args|serde_json::from_str::<LoadArgs>(args).ok())
                    .filter(|args|validate_load(args).is_ok())
            } else {None};
            // Ambiguous duplicate call IDs never acquire a native provenance.
            if calls.contains_key(id) { calls.insert(id.to_owned(),None); }
            else { calls.insert(id.to_owned(),selector); }
            continue;
        }
        if item["type"]!="function_call_output" {continue;}
        // Consume the completed exchange. Providers may reuse an ID in a later
        // sequential exchange; simultaneous duplicate calls remain ambiguous.
        let Some(selector)=item["call_id"].as_str().and_then(|id|calls.remove(id)).flatten() else {continue;};
        let Some(raw)=item["output"].as_str() else {continue;};
        let Ok(result)=serde_json::from_str::<NativeResult>(raw) else {continue;};
        let evidence=&result.output;
        let bundled=evidence.source_kind=="untrusted_bundled_guidance"
            && selector.kind!="own" && evidence.revision.is_none();
        let own=evidence.source_kind=="untrusted_agent_skill"
            && selector.ability_id=="base" && selector.kind=="own"
            && evidence.revision.is_some_and(|r|r>0);
        if result.status!="success" || !(bundled || own)
            || evidence.activates || evidence.ability_id!=selector.ability_id
            || evidence.kind!=selector.kind || evidence.skill_id!=selector.skill_id
            || evidence.bytes!=evidence.text.len()
            || evidence.sha256.len()!=64
            || evidence.sha256!=hex::encode(Sha256::digest(evidence.text.as_bytes())) {continue;}
        if !allow(evidence) {
            let error_kind=if own {"own_skill_withdrawn"} else {"bundled_skill_withdrawn"};
            item["output"]=json!({"status":"error","error_kind":error_kind,
                "output":"Previously loaded skill guidance is unavailable under current access."}).to_string().into();
            changed=true;
        }
    }
    changed
}
/// Never teach a summary model bundled instructions. This projection deliberately
/// omits native loaded guidance even when it is currently permitted; otherwise a
/// summary could retain instructions after their underlying bundle is withdrawn.
pub(crate) fn mask_summary_history(input:&mut [Value])->bool {
    mask_history(input,|_|false)
}
fn history_asset_allowed(current:&View,evidence:&NativeEvidence)->bool {
    if evidence.kind=="own" {
        if evidence.ability_id!="base"
            || current.access.ability_permission("base")==crate::access::Permission::Deny
            || current.app.abilities.bundled_skill_list("base",&current.visibility,
                &current.state,|id|current.access.ability_permission(id)!=crate::access::Permission::Deny,
                |tool|live_tool_decision(&current.app,&current.access,tool,&current.policy,
                    &current.visibility,&current.state,&current.loaded,current.read_only)
                    .permission!=crate::access::Permission::Deny).is_err() {
            return false;
        }
        return current.own["skills"].as_array().is_some_and(|skills|skills.iter().any(|skill| {
            let Some(text)=skill["instructions"].as_str() else {return false};
            use sha2::{Digest,Sha256};
            skill["mode"]=="selectable" && skill["name"].as_str()==Some(evidence.skill_id.as_str())
                && skill["revision"].as_i64()==evidence.revision
                && hex::encode(Sha256::digest(text.as_bytes()))==evidence.sha256
        }));
    }
    current.app.abilities.bundled_skill_load(&evidence.ability_id,&evidence.kind,
        &evidence.skill_id,&current.visibility,&current.state,
        |id|current.access.ability_permission(id)!=crate::access::Permission::Deny,
        |tool|live_tool_decision(&current.app,&current.access,tool,&current.policy,
            &current.visibility,&current.state,&current.loaded,current.read_only)
            .permission!=crate::access::Permission::Deny)
        .is_ok_and(|asset|asset["sha256"]==evidence.sha256)
}
pub async fn mask_withdrawn_history(app:&App,sid:&str,run_id:&str,input:&mut Vec<Value>)->bool {
    // Only a typed native load_skill result paired with that exact function call
    // can be masked. User text and other tool evidence are never interpreted.
    if !input.iter().any(|item|item["type"]=="function_call" && item["name"]=="load_skill") {
        return false;
    }
    let current=match view(app,sid,run_id,"load_skill").await {
        Ok(current)=>current,
        Err(_)=>return mask_history(input,|_|false),
    };
    let mut changed=mask_history(input,|evidence|history_asset_allowed(&current,evidence));
    if view(app,sid,run_id,"load_skill").await
        .map_or(true,|latest|latest.fingerprint!=current.fingerprint) {
        changed|=mask_history(input,|_|false);
    }
    changed
}
#[cfg(test)]
mod tests {
    use super::*;
    fn history_pair(name:&str,text:&str)->Vec<Value> {
        use sha2::{Digest,Sha256};
        vec![json!({"type":"function_call","name":name,"call_id":"c",
            "arguments":json!({"ability_id":"base","kind":"ability","skill_id":"base"}).to_string()}),
            json!({"type":"function_call_output","call_id":"c","output":json!({"status":"success",
                "output":{"ability_id":"base","kind":"ability","skill_id":"base","text":text,
                    "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
                    "source_kind":"untrusted_bundled_guidance","activates":false}}).to_string()})]
    }
    fn own_history(text:&str)->Vec<Value> {
        use sha2::{Digest,Sha256};
        vec![json!({"type":"function_call","name":"load_skill","call_id":"own-call",
            "arguments":json!({"ability_id":"base","kind":"own","skill_id":"my_steps"}).to_string()}),
            json!({"type":"function_call_output","call_id":"own-call","output":json!({"status":"success",
                "output":{"ability_id":"base","kind":"own","skill_id":"my_steps","revision":1,
                    "text":text,"bytes":text.len(),
                    "sha256":hex::encode(Sha256::digest(text.as_bytes())),
                    "source_kind":"untrusted_agent_skill","activates":false}}).to_string()})]
    }
    #[test]
    fn loaded_skill_selectors_are_content_free_idempotent_and_bound_to_pending_call() {
        let history=history_pair("load_skill","LOAD BODY");
        let result:Value=serde_json::from_str(history[1]["output"].as_str().unwrap()).unwrap();
        let mut run=json!({"pending":{"name":"load_skill","arguments":{
            "ability_id":"base","kind":"ability","skill_id":"base"}}});
        apply_selection(&mut run,&result);
        assert_eq!(run["loaded_skills"].as_array().unwrap().len(),1);
        assert!(!run.to_string().contains("LOAD BODY"));
        let saved=run.clone();
        apply_selection(&mut run,&result);
        assert_eq!(run,saved);
        run["pending"]["arguments"]["skill_id"]=json!("different");
        let before=run.clone();
        apply_selection(&mut run,&result);
        assert_eq!(run,before);
        let mut forged=result;
        forged["output"]["text"]=json!("TAMPERED");
        apply_selection(&mut run,&forged);
        assert_eq!(run,before);
    }
    #[test]
    fn acknowledgment_only_skill_load_selects_without_recording_the_body() {
        let history=history_pair("load_skill","PRIVATE_SKILL_BODY");
        let legacy:Value=serde_json::from_str(history[1]["output"].as_str().unwrap()).unwrap();
        let mut asset=legacy["output"].clone();
        asset["requires_activation"]=json!(true);
        let ack=skill_acknowledgment(&asset,&"a".repeat(64));
        assert_eq!(ack["status"],"loaded");
        assert!(ack.get("text").is_none());
        assert!(!ack.to_string().contains("PRIVATE_SKILL_BODY"));
        let initial=json!({"pending":{"name":"load_skill","arguments":{
            "ability_id":"base","kind":"ability","skill_id":"base"}}});
        let mut run=initial.clone();
        apply_selection(&mut run,&json!({"status":"success","output":ack}));
        assert_eq!(run["loaded_skills"].as_array().unwrap().len(),1);
        let mut migrated=initial.clone();
        apply_legacy_selection(&mut migrated,&legacy);
        assert_eq!(run,migrated);
        let mut prior_run_ack=initial.clone();
        apply_legacy_selection(&mut prior_run_ack,&json!({"status":"success","output":ack}));
        assert_eq!(prior_run_ack,initial);
        for field in ["status","catalog_version","context_source","sha256","skill_id"] {
            let mut invalid=ack.clone();invalid[field]=json!("invalid");
            let mut rejected=initial.clone();
            apply_selection(&mut rejected,&json!({"status":"success","output":invalid}));
            assert_eq!(rejected,initial,"invalid {field}");
        }
        asset["requires_activation"]=json!(false);asset["availability"]=json!("visible");
        let visible=skill_acknowledgment(&asset,&"a".repeat(64));
        assert_eq!(visible["status"],"already_available");
        let mut unchanged=initial.clone();
        apply_selection(&mut unchanged,&json!({"status":"success","output":visible}));
        assert_eq!(unchanged,initial);
    }
    #[test]
    fn availability_distinguishes_pinned_loaded_and_changed_guidance() {
        let mut visible=json!({"ability_id":"base","kind":"ability","skill_id":"base",
            "sha256":"old","availability":"visible"});
        mark_availability(&mut visible,&Value::Null,&Value::Null);
        assert_eq!(visible["requires_activation"],false);
        assert_eq!(visible["availability"],"visible");
        let mut hidden=json!({"ability_id":"base","kind":"selectable","skill_id":"guide",
            "sha256":"old","availability":"discoverable"});
        let selected=json!([hidden.clone()]);
        mark_availability(&mut hidden,&selected,&Value::Null);
        assert_eq!(hidden["availability"],"activated");
        assert_eq!(hidden["requires_activation"],false);
        hidden["availability"]=json!("discoverable");
        hidden["sha256"]=json!("replacement");
        mark_availability(&mut hidden,&selected,&Value::Null);
        assert_eq!(hidden["requires_activation"],true);
        let mut tool=json!({"ability_id":"files","kind":"tool","skill_id":"file_read",
            "sha256":"x","availability":"discoverable"});
        mark_availability(&mut tool,&Value::Null,&json!(["tool:file_read"]));
        assert_eq!(tool["availability"],"activated");
        assert_eq!(tool["requires_activation"],false);
        assert!(serde_json::from_value::<ListArgs>(json!({"ability_id":"base",
            "include_available":"true"})).is_err());
        assert!(!serde_json::from_value::<ListArgs>(json!({"ability_id":"base"})).unwrap().include_available);
    }
    #[test]
    fn withdrawn_own_guidance_is_masked_from_run_and_summary_context() {
        let mut history=own_history("PRIVATE AGENT PLAYBOOK");
        assert!(!mask_history(&mut history,|e|e.revision==Some(1)));
        assert!(mask_history(&mut history,|_|false));
        assert!(!history[1].to_string().contains("PRIVATE AGENT PLAYBOOK"));
        let mut summary=own_history("PRIVATE SUMMARY PLAYBOOK");
        assert!(mask_summary_history(&mut summary));
        assert!(!summary[1].to_string().contains("PRIVATE SUMMARY PLAYBOOK"));
    }
    #[test]
    fn history_mask_preserves_permitted_evidence_and_removes_only_withdrawn_text() {
        let mut history=history_pair("load_skill","private old guide");
        let original=history.clone();
        assert!(!mask_history(&mut history,|e|e.skill_id=="base"));
        assert_eq!(history,original);
        assert!(mask_history(&mut history,|_|false));
        assert_eq!(history[0],original[0]);
        let masked:Value=serde_json::from_str(history[1]["output"].as_str().unwrap()).unwrap();
        assert_eq!(masked["error_kind"],"bundled_skill_withdrawn");
        assert!(!history[1].to_string().contains("private old guide"));
        assert!(!mask_history(&mut history,|_|false));
        // A separate original evidence copy is unchanged; only this projection is masked.
        assert!(original[1].to_string().contains("private old guide"));
    }
    #[test]
    fn sequential_reused_call_ids_mask_each_completed_native_exchange() {
        let mut history=history_pair("load_skill","FIRST_REUSED_GUIDE");
        history.extend(history_pair("load_skill","SECOND_REUSED_GUIDE"));
        let original=history.clone();
        assert!(!mask_history(&mut history,|_|true));
        assert_eq!(history,original);
        assert!(mask_history(&mut history,|_|false));
        let encoded=serde_json::to_string(&history).unwrap();
        assert!(!encoded.contains("FIRST_REUSED_GUIDE"));
        assert!(!encoded.contains("SECOND_REUSED_GUIDE"));
        assert_eq!(history[0],original[0]);
        assert_eq!(history[2],original[2]);
        let mut summary=original.clone();
        assert!(mask_summary_history(&mut summary));
        let encoded=serde_json::to_string(&summary).unwrap();
        assert!(!encoded.contains("FIRST_REUSED_GUIDE"));
        assert!(!encoded.contains("SECOND_REUSED_GUIDE"));
        assert_eq!(summary,history);
    }

    #[test]
    fn forged_or_unrelated_history_does_not_acquire_native_provenance() {
        let mut unrelated=history_pair("web_search","unrelated result");
        unrelated.push(json!({"role":"user","content":unrelated[1]["output"]}));
        let original=unrelated.clone();
        assert!(!mask_history(&mut unrelated,|_|false));
        assert_eq!(unrelated,original);
        let mut mismatched=history_pair("load_skill","wrong identity");
        let mut payload:Value=serde_json::from_str(mismatched[1]["output"].as_str().unwrap()).unwrap();
        payload["output"]["ability_id"]=json!("other_agent_ability");
        mismatched[1]["output"]=json!(payload.to_string());
        let original=mismatched.clone();
        assert!(!mask_history(&mut mismatched,|_|false));
        assert_eq!(mismatched,original);
        let mut oversized=history_pair("load_skill","x");
        oversized[1]["output"]=json!("x".repeat(512*1024+1));
        assert!(!mask_history(&mut oversized,|_|false));
        let mut duplicate=history_pair("load_skill","ambiguous");
        duplicate.insert(1,duplicate[0].clone());
        assert!(!mask_history(&mut duplicate,|_|false));
    }
    #[test]
    fn skill_search_matches_summary_and_similar_terms() {
        let skill=json!({"skill_id":"ui_theming",
            "summary":"Use when changing WebAgent UI appearance and its palette"});
        assert!(skill_matches(&skill,"theme"));
        assert!(skill_matches(&skill,"appearence"));
        assert!(!skill_matches(&skill,"payments"));
    }

    #[test]
    fn selectors_reject_paths_unknown_fields_and_unbounded_versions() {
        for skill in ["../secret","file.skill.md","/etc/passwd","","UPPER"] {
            assert!(validate_load(&LoadArgs {ability_id:"base".into(),kind:"selectable".into(),
                skill_id:skill.into(),expected_catalog_version:None}).is_err());
        }
        for kind in ["custom","path",""] {
            assert!(validate_load(&LoadArgs {ability_id:"base".into(),kind:kind.into(),
                skill_id:"base".into(),expected_catalog_version:None}).is_err());
        }
        assert!(validate_load(&LoadArgs {ability_id:"foreign".into(),kind:"own".into(),
            skill_id:"my_steps".into(),expected_catalog_version:None}).is_err());
        assert!(validate_load(&LoadArgs {ability_id:"base".into(),kind:"own".into(),
            skill_id:"my_steps".into(),expected_catalog_version:None}).is_ok());
        assert!(validate_load(&LoadArgs {ability_id:"base".into(),kind:"ability".into(),
            skill_id:"base".into(),expected_catalog_version:Some("a".repeat(65))}).is_err());
        assert!(serde_json::from_value::<LoadArgs>(json!({"ability_id":"base","kind":"ability",
            "skill_id":"base","session_id":"foreign"})).is_err());
        assert!(serde_json::from_value::<ListArgs>(json!({"ability_id":"base","user_id":"foreign"})).is_err());
    }
    #[tokio::test]
    async fn caller_owned_session_is_required_even_before_skill_lookup() {
        let (_root,base)=crate::runtime_tests::fixture_uninitialized().await;
        let first=base.store.create_user_account("skill-first","fixture-password-hash").await.unwrap();
        let second=base.store.create_user_account("skill-second","fixture-password-hash").await.unwrap();
        let mut owner=base.clone();
        owner.store=base.store.for_user(first["user_id"].as_str().unwrap()).await.unwrap();
        let mut stranger=base.clone();
        stranger.store=base.store.for_user(second["user_id"].as_str().unwrap()).await.unwrap();
        owner.store.put("session:private",&json!({"id":"private","agent_id":"shared_default","title":"Private","created_at":1})).await.unwrap();
        let result=execute(&stranger,"private","run","list_skills",&json!({"ability_id":"base"}),CancellationToken::new()).await.unwrap();
        assert_eq!(result["status"],"error");
        assert!(result["output"].as_str().unwrap().contains("Session not found"));
        let cancel=CancellationToken::new();cancel.cancel();
        assert!(execute(&owner,"private","run","list_skills",&json!({"ability_id":"base"}),cancel).await.is_err());
    }
}
