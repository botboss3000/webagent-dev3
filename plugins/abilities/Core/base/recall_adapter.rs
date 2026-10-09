//! Bounded recall of this run's caller-owned, encrypted compaction sources.
//! The model cannot name another session or user; the segment must be listed
//! on the current session and match its durable source digest.
use crate::{App, ability_controls, live_tool_decision, session_agent_id, session_mode};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;

const MAX_OUTPUT_BYTES: usize = 16 * 1024;
pub fn owns(name: &str) -> bool { name == "recall_compacted" }
pub fn schemas() -> Vec<Value> {
    vec![json!({"type":"function","name":"recall_compacted",
        "description":"A first call with no selector lists this session's compacted segments. segment_id must be one of the listed ids. tool_call_id pages an eligible read result from this session instead; never pass a tool_call_id as a segment_id. Reasoning and revocable guidance are withheld.",
        "parameters":{"type":"object","properties":{
            "segment_id":{"type":"string","minLength":1,"maxLength":128},
            "offset_items":{"type":"integer","minimum":0,"maximum":4096},
            "limit_items":{"type":"integer","minimum":1,"maximum":20},
            "tool_call_id":{"type":"string","minLength":1,"maxLength":128},
            "offset_chars":{"type":"integer","minimum":0,"maximum":2000000},
            "limit_chars":{"type":"integer","minimum":1,"maximum":4096}},
            "required":[],"additionalProperties":false}})]
}
fn default_limit() -> usize { 10 }
fn default_chars() -> usize { 4096 }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)] segment_id: Option<String>,
    #[serde(default)] offset_items: usize,
    #[serde(default = "default_limit")] limit_items: usize,
    #[serde(default)] tool_call_id: Option<String>,
    #[serde(default)] offset_chars: usize,
    #[serde(default = "default_chars")] limit_chars: usize,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn validate(args: &Args) -> Result<()> {
    if !(1..=20).contains(&args.limit_items) || args.offset_items > 4096
        || !(1..=4096).contains(&args.limit_chars) || args.offset_chars>2_000_000
        || args.segment_id.as_deref().is_some_and(|id| !valid_id(id))
        || args.tool_call_id.as_deref().is_some_and(|id| !valid_id(id))
        || (args.segment_id.is_none() && args.offset_items != 0)
        || (args.tool_call_id.is_none() && args.segment_id.is_none() && args.offset_chars != 0)
        || (args.segment_id.is_some() && args.offset_chars != 0 && args.limit_items != 1)
        {
        bail!("Invalid compacted segment selector");
    }
    Ok(())
}
struct View { app: App, fingerprint: String }
async fn view(app: &App, sid: &str, run_id: &str) -> Result<View> {
    let agent = session_agent_id(app, sid).await?;
    let mut scoped = app.clone();
    scoped.store = app.store.for_agent(&agent).await?;
    let run = scoped.run(run_id).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let (policy, revision, visibility, state) = ability_controls(&scoped, sid).await?;
    let mode = session_mode(&scoped, sid).await?;
    let access = scoped.store.live_access(scoped.store.current_user_id(), &agent).await?;
    if !access.admitted() || live_tool_decision(&scoped, &access, "recall_compacted",
        &policy, &visibility, &state, &run["loaded_abilities"], mode["mode"]=="read_only")
        .permission == crate::access::Permission::Deny {
        bail!("Compacted recall unavailable under current access");
    }
    let fingerprint = crate::digest(&json!([agent, policy, revision, visibility,
        state, mode, run["loaded_abilities"], access.context_fingerprint(),
        scoped.abilities.live_version()]));
    Ok(View { app: scoped, fingerprint })
}
fn metadata(session: &Value) -> Result<Vec<Value>> {
    let Some(entries) = session.get("compaction_segments") else { return Ok(Vec::new()); };
    let entries = entries.as_array().context("Invalid compacted segment index")?;
    let mut found = HashSet::new();
    let mut output = Vec::new();
    for entry in entries {
        let id = entry["segment_id"].as_str().context("Invalid compacted segment ID")?;
        let digest = entry["source_digest"].as_str().context("Invalid compacted source digest")?;
        let count = entry["item_count"].as_u64().context("Invalid compacted item count")?;
        let created = entry["created_at"].as_u64().context("Invalid compacted creation time")?;
        if !valid_id(id) || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit()) || count > 1_000_000
            || !found.insert(id.to_owned()) {
            bail!("Invalid compacted segment index");
        }
        output.push(json!({"segment_id":id,"source_digest":digest,
            "item_count":count,"created_at":created}));
    }
    Ok(output)
}
fn mask_guidance(source: &mut [Value]) {
    // Mask on the full source before paging: splitting a call from its result
    // would defeat the provenance-aware bundled guidance masker.
    crate::tools::core_skills::mask_summary_history(source);
    let mut sensitive_calls = HashSet::new();
    for item in source {
        if item["type"] == "function_call" {
            if matches!(item["name"].as_str(), Some("read_own_prompt" | "prompt_slot_read")) {
                if let Some(id) = item["call_id"].as_str() { sensitive_calls.insert(id.to_owned()); }
            }
        } else if item["type"] == "function_call_output" {
            if item["call_id"].as_str().is_some_and(|id| sensitive_calls.remove(id)) {
                item["output"] = json!({"status":"error","error_kind":"guidance_withheld",
                    "output":"Previously read prompt guidance is unavailable in compacted recall."}).to_string().into();
            }
        }
        if let Some(object) = item.as_object_mut() { object.remove("deepseek_reasoning"); }
    }
}
fn segment_page(record: &Value, entry: &Value, sid: &str, args: &Args) -> Result<Value> {
    let id = args.segment_id.as_deref().context("Segment ID required")?;
    if !matches!(record["schema_version"].as_u64(),Some(1|2)) || record["session_id"] != sid
        || record["segment_id"] != id || record["source_digest"] != entry["source_digest"] {
        bail!("Compacted segment provenance conflict");
    }
    let mut source = record["source"].as_array().context("Compacted source unavailable")?.clone();
    if source.len() as u64 != entry["item_count"].as_u64().unwrap_or(u64::MAX)
        || crate::digest(&source) != entry["source_digest"].as_str().unwrap_or("") {
        bail!("Compacted source digest conflict");
    }
    mask_guidance(&mut source);
    let total = source.len();
    if args.offset_items > total { bail!("Compacted item cursor exceeds source"); }
    let end = args.offset_items.saturating_add(args.limit_items).min(total);
    let items = source[args.offset_items..end].iter()
        .filter(|item| item["type"] != "reasoning").cloned().collect::<Vec<_>>();
    let output = json!({"segment_id":id,"source_digest":entry["source_digest"],
        "source_kind":"compacted_provider_history","items":items,
        "offset_items":args.offset_items,"next_offset_items":end,"total_items":total,
        "has_more":end<total,"reasoning_included":false,"revocable_guidance_included":false});
    if args.limit_items==1 && items.len()==1 && (args.offset_chars>0
        || serde_json::to_vec(&output)?.len()>MAX_OUTPUT_BYTES) {
        let serialized=items[0].to_string();
        let total_chars=serialized.chars().count();
        if args.offset_chars>total_chars { bail!("Compacted character cursor exceeds item"); }
        let content=serialized.chars().skip(args.offset_chars)
            .take(args.limit_chars).collect::<String>();
        let next=args.offset_chars+content.chars().count();
        return Ok(json!({"segment_id":id,"source_digest":entry["source_digest"],
            "source_kind":"compacted_item_page","item_index":args.offset_items,
            "content":content,"offset_chars":args.offset_chars,
            "next_offset_chars":next,"total_chars":total_chars,"has_more":next<total_chars,
            "reasoning_included":false,"revocable_guidance_included":false}));
    }
    if serde_json::to_vec(&output)?.len() > MAX_OUTPUT_BYTES {
        bail!("Compacted page exceeds 16 KiB; request fewer items");
    }
    Ok(output)
}
fn tool_result_page(record:&Value, call_id:&str, args:&Args)->Result<Value> {
    if record["type"]!="function_call_output" || record["call_id"]!=call_id {
        bail!("Tool result provenance conflict");
    }
    let raw=record["output"].as_str().context("Tool result unavailable")?;
    let total=raw.chars().count();
    if args.offset_chars>total { bail!("Tool result cursor exceeds source"); }
    let text=raw.chars().skip(args.offset_chars).take(args.limit_chars).collect::<String>();
    let next=args.offset_chars+text.chars().count();
    Ok(json!({"tool_call_id":call_id,"source_kind":"current_run_tool_result",
        "content":text,"offset_chars":args.offset_chars,"total_chars":total,
        "next_offset_chars":next,"has_more":next<total}))
}
async fn work(app: &App, sid: &str, run_id: &str, args: &Value) -> Result<Value> {
    let parsed: Args = serde_json::from_value(args.clone()).context("Invalid compacted recall arguments")?;
    validate(&parsed)?;
    let current = view(app, sid, run_id).await?;
    let session = current.app.store.get(&format!("session:{sid}")).await?
        .context("Current session unavailable")?;
    let entries = metadata(&session)?;
    let segment_entry = parsed.segment_id.as_deref().and_then(|id|
        entries.iter().find(|entry| entry["segment_id"] == id));
    let output = if let Some(entry) = segment_entry {
        let id = parsed.segment_id.as_deref().unwrap();
        let record = current.app.store.get(&format!("compaction_segment:{sid}:{id}")).await?
            .context("Compacted segment unavailable")?;
        segment_page(&record, entry, sid, &parsed)?
    } else if let Some(call_id) = parsed.tool_call_id.as_deref() {
        let run=current.app.run(run_id).await?;
        // Origin comes from the session-owned retention ledger, never a model-supplied run ID.
        let origin=crate::tool_output::origin_run_id(&run,call_id).unwrap_or(run_id);
        let origin_run=current.app.run(origin).await?;
        if origin_run["session_id"]!=sid || origin_run["agent_id"]!=run["agent_id"] {
            bail!("Tool recall origin is unavailable");
        }
        let name=current.app.store.tool_call_name(sid,origin,call_id).await?;
        let permitted = name.as_deref().is_some_and(crate::tool_output::eligible);
        let result = if permitted {
            let name = name.as_deref().unwrap();
            let agent=session_agent_id(&current.app,sid).await?;
            let (policy,_,visibility,state)=ability_controls(&current.app,sid).await?;
            let mode=session_mode(&current.app,sid).await?;
            let access=current.app.store.live_access(current.app.store.current_user_id(),&agent).await?;
            if live_tool_decision(&current.app,&access,&name,&policy,&visibility,&state,
                &run["loaded_abilities"],mode["mode"]=="read_only").permission
                != crate::access::Permission::Deny {
                if let Some(record)=current.app.store.tool_result(sid,origin,call_id).await? {
                    let raw = record["output"].as_str().context("Tool result unavailable")?;
                    if origin != run_id
                        && !crate::tool_output::verify_origin_output(&run,call_id,name,raw)
                    {
                        bail!("Archived tool result does not match retained provenance");
                    }
                    Some(tool_result_page(&record,call_id,&parsed)?)
                } else { None }
            } else { None }
        } else { None };
        result.unwrap_or_else(|| {
            let recent = entries.iter().rev().take(32).cloned().collect::<Vec<_>>();
            json!({"segments":recent,"count":recent.len(),"total_segments":entries.len(),
                "source_kind":"current_session_compaction_index",
                "note":"Supplied selectors matched nothing."})
        })
    } else {
        let recent = entries.iter().rev().take(32).cloned().collect::<Vec<_>>();
        json!({"segments":recent,"count":recent.len(),"total_segments":entries.len(),
            "source_kind":"current_session_compaction_index",
            "note":if parsed.segment_id.is_some() { Some("Supplied selectors matched nothing.") } else { None }})
    };
    if serde_json::to_vec(&output)?.len() > MAX_OUTPUT_BYTES {
        bail!("Compacted segment list exceeds 16 KiB");
    }
    if view(app, sid, run_id).await?.fingerprint != current.fingerprint {
        bail!("Compacted recall access changed during read");
    }
    // A concurrent compaction may replace the session index while a segment
    // is read. Confirm the selected provenance still belongs to this session.
    let latest = current.app.store.get(&format!("session:{sid}")).await?
        .context("Current session unavailable")?;
    if segment_entry.is_some() && metadata(&latest)?.iter().all(|entry|
        entry["segment_id"] != parsed.segment_id.as_deref().unwrap()) {
        bail!("Compacted segment index changed during read");
    }
    Ok(output)
}
pub async fn execute(app: &App, sid: &str, run_id: &str, name: &str,
    args: &Value, cancel: CancellationToken) -> Result<Value> {
    if !owns(name) { bail!("Unknown compacted recall tool"); }
    if cancel.is_cancelled() { bail!("tool execution cancelled"); }
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = work(app, sid, run_id, args) => result,
    };
    if cancel.is_cancelled() { bail!("tool execution cancelled"); }
    Ok(match result {
        Ok(output) => json!({"status":"success","output":output}),
        Err(error) => json!({"status":"error","error_kind":"compacted_recall_rejected",
            "output":error.to_string()}),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn list_and_page_are_bounded_and_preserve_provenance() {
        let source = vec![
            json!({"type":"message","role":"user","content":"before"}),
            json!({"type":"reasoning","encrypted_content":"SECRET"}),
            json!({"type":"message","role":"assistant","content":"after"})];
        let digest = crate::digest(&source);
        let entry = json!({"segment_id":"segment-1","source_digest":digest,
            "item_count":3,"created_at":1});
        let record = json!({"schema_version":1,"session_id":"owned","segment_id":"segment-1",
            "source_digest":digest,"source":source});
        let args = Args {segment_id:Some("segment-1".into()),offset_items:0,limit_items:3,
            tool_call_id:None,offset_chars:0,limit_chars:4096};
        let page = segment_page(&record,&entry,"owned",&args).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(),2);
        assert!(!page.to_string().contains("SECRET"));
        assert_eq!(page["total_items"],3);
        assert!(segment_page(&record,&entry,"foreign",&args).is_err());
        assert!(segment_page(&record,&entry,"owned",&Args {offset_items:4,..args}).is_err());
    }
    #[test]
    fn guidance_and_selectors_do_not_expand_recall_authority() {
        let mut source = vec![
            json!({"type":"function_call","name":"read_own_prompt","call_id":"one"}),
            json!({"type":"function_call_output","call_id":"one","output":"PROMPT SECRET"}),
            json!({"type":"message","role":"assistant","deepseek_reasoning":"THOUGHT","content":"visible"})];
        mask_guidance(&mut source);
        assert!(!json!(source).to_string().contains("PROMPT SECRET"));
        assert!(!json!(source).to_string().contains("THOUGHT"));
        assert!(serde_json::from_value::<Args>(json!({"session_id":"foreign"})).is_err());
        assert!(validate(&Args {segment_id:Some("../foreign".into()),
            offset_items:0,limit_items:1,tool_call_id:None,offset_chars:0,limit_chars:4096}).is_err());
    }
    async fn recall_fixture() -> (tempfile::TempDir, App, String, String) {
        let (root, app) = crate::runtime_tests::fixture().await;
        let mut tier = app.store.entitlement_tier("operator").await.unwrap().unwrap();
        let revision = tier.revision;
        tier.ability_permissions.insert("base".into(), crate::access::Permission::Auto);
        app.store.save_entitlement_tier(tier, revision).await.unwrap();
        let mut level = app.store.live_access("admin", "shared_default").await.unwrap().capability_level;
        level.ability_permissions.insert("base".into(), crate::access::Permission::Auto);
        app.store.save_capability_level("shared_default", level, 0).await.unwrap();
        let sid = crate::runtime_tests::conversation(&app).await;
        crate::runtime_tests::activate_codebase(&app, &sid).await;
        crate::session_ability_set(crate::State(app.clone()), crate::Path((sid.clone(), "base".into())),
            crate::Json(crate::SessionAbilityBody {state:"active".into(), expected_revision:1}))
            .await.unwrap();
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        run["loaded_abilities"] = json!(["base", "codebase_admin"]);
        app.save(&mut run).await.unwrap();
        let source = vec![json!({"type":"message","role":"user","content":"remember me"})];
        let digest = crate::digest(&source);
        let entry = json!({"segment_id":"listed-1","source_digest":digest,"item_count":1,"created_at":1});
        let mut session = app.store.get(&format!("session:{sid}")).await.unwrap().unwrap();
        session["compaction_segments"] = json!([entry]);
        app.store.put(&format!("session:{sid}"), &session).await.unwrap();
        app.store.put(&format!("compaction_segment:{sid}:listed-1"), &json!({
            "schema_version":1,"session_id":sid,"segment_id":"listed-1",
            "source_digest":digest,"source":source})).await.unwrap();
        (root, app, sid, rid)
    }
    async fn dispatch(app: &App, sid: &str, rid: &str, args: Value) -> Value {
        execute(app, sid, rid, "recall_compacted", &args, CancellationToken::new()).await.unwrap()
    }
    #[tokio::test]
    async fn unlisted_segment_falls_back_to_file_result_page() {
        let (_root, app, sid, rid) = recall_fixture().await;
        let mut tx = app.store.pool.begin().await.unwrap();
        app.store.insert_interaction(&mut tx,&sid,&rid,"model:1",&json!({
            "type":"function_call","call_id":"read-call","name":"file_read","arguments":"{}"})).await.unwrap();
        app.store.insert_interaction(&mut tx,&sid,&rid,"tool:read-call",&json!({
            "type":"function_call_output","call_id":"read-call","output":"abcdef"})).await.unwrap();
        tx.commit().await.unwrap();
        let result = dispatch(&app,&sid,&rid,json!({"segment_id":"not-listed",
            "tool_call_id":"read-call","offset_chars":2,"limit_chars":4,"limit_items":1})).await;
        assert_eq!(result["status"],"success", "{result}");
        assert_eq!(result["output"]["content"],"cdef");
        assert_eq!(result["output"]["source_kind"],"current_run_tool_result");
        let preferred = dispatch(&app,&sid,&rid,json!({"segment_id":"listed-1","tool_call_id":"read-call"})).await;
        assert_eq!(preferred["status"],"success", "{preferred}");
        assert_eq!(preferred["output"]["source_kind"],"compacted_provider_history");
    }
    #[tokio::test]
    async fn unmatched_selectors_return_listing_with_note() {
        let (_root, app, sid, rid) = recall_fixture().await;
        for args in [json!({"segment_id":"missing-segment"}),json!({"tool_call_id":"missing-call"})] {
            let result = dispatch(&app,&sid,&rid,args).await;
            assert_eq!(result["status"],"success", "{result}");
            assert_eq!(result["output"]["segments"][0]["segment_id"],"listed-1");
            assert_eq!(result["output"]["note"],"Supplied selectors matched nothing.");
        }
    }
    #[tokio::test]
    async fn both_unmatched_selectors_return_listing_with_note() {
        let (_root, app, sid, rid) = recall_fixture().await;
        let result = dispatch(&app,&sid,&rid,json!({"segment_id":"missing-segment","tool_call_id":"missing-call"})).await;
        assert_eq!(result["status"],"success", "{result}");
        assert_eq!(result["output"]["source_kind"],"current_session_compaction_index");
        assert_eq!(result["output"]["count"],1);
        assert_eq!(result["output"]["note"],"Supplied selectors matched nothing.");
    }
}
