//! Bounded recall of this run's caller-owned, encrypted compaction sources.
//! The model cannot name another session or user; the segment must be listed
//! on the current session and match its durable source digest.
use crate::{Call, NativeHost};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use tokio_util::sync::CancellationToken;
use webagent_access::Permission;
use webagent_context::retention as tool_output;
use webagent_host_core::{digest, key, StoreCore};

const MAX_OUTPUT_BYTES: usize = 16 * 1024;
/// The compacted-recall tool name.
pub const TOOLS: &[&str] = &["recall_compacted"];
/// Whether `name` is the compacted-recall tool.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "recall_compacted"
}
/// The compacted-recall tool schema.
#[must_use]
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
fn default_limit() -> usize {
    10
}
fn default_chars() -> usize {
    4096
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    #[serde(default)]
    segment_id: Option<String>,
    #[serde(default)]
    offset_items: usize,
    #[serde(default = "default_limit")]
    limit_items: usize,
    #[serde(default)]
    tool_call_id: Option<String>,
    #[serde(default)]
    offset_chars: usize,
    #[serde(default = "default_chars")]
    limit_chars: usize,
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn validate(args: &Args) -> Result<()> {
    if !(1..=20).contains(&args.limit_items)
        || args.offset_items > 4096
        || !(1..=4096).contains(&args.limit_chars)
        || args.offset_chars > 2_000_000
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
/// Run `id` from `store`.
async fn run(store: &StoreCore, id: &str) -> Result<Value> {
    store
        .get(&key("run", id))
        .await?
        .ok_or_else(|| anyhow!("Run not found"))
}
struct View {
    store: StoreCore,
    fingerprint: String,
}
async fn view(host: &dyn NativeHost, sid: &str, run_id: &str) -> Result<View> {
    if host.actor() != host.store().current_user_id() {
        bail!("Shared chat membership does not authorize caller-owned private records");
    }

    let agent = host.session_agent_id(sid).await?;
    let scoped = host.agent_store(&agent).await?;
    let run = run(&scoped, run_id).await?;
    if run["session_id"] != sid || run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        bail!("Run does not belong to current session and agent");
    }
    let (policy, revision, visibility, state) = host.scoped_ability_controls(&scoped, sid).await?;
    let mode = host.scoped_session_mode(&scoped, sid).await?;
    let access = host
        .scoped_live_access(&scoped, host.actor(), sid, &agent)
        .await?;
    if !access.admitted()
        || host
            .live_tool_decision(
                &access,
                "recall_compacted",
                &policy,
                &visibility,
                &state,
                &run["loaded_abilities"],
                mode["mode"] == "read_only",
            )
            .permission
            == Permission::Deny
    {
        bail!("Compacted recall unavailable under current access");
    }
    let fingerprint = digest(&json!([
        agent,
        policy,
        revision,
        visibility,
        state,
        mode,
        run["loaded_abilities"],
        access.context_fingerprint(),
        host.abilities_version()
    ]));
    Ok(View {
        store: scoped,
        fingerprint,
    })
}
fn metadata(session: &Value) -> Result<Vec<Value>> {
    let Some(entries) = session.get("compaction_segments") else {
        return Ok(Vec::new());
    };
    let entries = entries
        .as_array()
        .context("Invalid compacted segment index")?;
    let mut found = HashSet::new();
    let mut output = Vec::new();
    for entry in entries {
        let id = entry["segment_id"]
            .as_str()
            .context("Invalid compacted segment ID")?;
        let digest = entry["source_digest"]
            .as_str()
            .context("Invalid compacted source digest")?;
        let count = entry["item_count"]
            .as_u64()
            .context("Invalid compacted item count")?;
        let created = entry["created_at"]
            .as_u64()
            .context("Invalid compacted creation time")?;
        if !valid_id(id)
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || count > 1_000_000
            || !found.insert(id.to_owned())
        {
            bail!("Invalid compacted segment index");
        }
        output.push(json!({"segment_id":id,"source_digest":digest,
            "item_count":count,"created_at":created}));
    }
    Ok(output)
}
fn mask_guidance(source: &mut [Value]) {
    let mut sensitive_calls = HashSet::new();
    for item in source {
        if item["type"] == "function_call" {
            if matches!(
                item["name"].as_str(),
                Some("read_own_prompt" | "prompt_slot_read")
            ) {
                if let Some(id) = item["call_id"].as_str() {
                    sensitive_calls.insert(id.to_owned());
                }
            }
        } else if item["type"] == "function_call_output" {
            if item["call_id"]
                .as_str()
                .is_some_and(|id| sensitive_calls.remove(id))
            {
                item["output"] = json!({"status":"error","error_kind":"guidance_withheld",
                    "output":"Previously read prompt guidance is unavailable in compacted recall."})
                .to_string()
                .into();
            }
        }
        if let Some(object) = item.as_object_mut() {
            object.remove("deepseek_reasoning");
        }
    }
}
fn segment_page(record: &Value, entry: &Value, sid: &str, args: &Args) -> Result<Value> {
    let id = args.segment_id.as_deref().context("Segment ID required")?;
    if !matches!(record["schema_version"].as_u64(), Some(1 | 2))
        || record["session_id"] != sid
        || record["segment_id"] != id
        || record["source_digest"] != entry["source_digest"]
    {
        bail!("Compacted segment provenance conflict");
    }
    let mut source = record["source"]
        .as_array()
        .context("Compacted source unavailable")?
        .clone();
    if source.len() as u64 != entry["item_count"].as_u64().unwrap_or(u64::MAX)
        || digest(&source) != entry["source_digest"].as_str().unwrap_or("")
    {
        bail!("Compacted source digest conflict");
    }
    mask_guidance(&mut source);
    let total = source.len();
    if args.offset_items > total {
        bail!("Compacted item cursor exceeds source");
    }
    let end = args
        .offset_items
        .saturating_add(args.limit_items)
        .min(total);
    let items = source[args.offset_items..end]
        .iter()
        .filter(|item| item["type"] != "reasoning")
        .cloned()
        .collect::<Vec<_>>();
    let output = json!({"segment_id":id,"source_digest":entry["source_digest"],
        "source_kind":"compacted_provider_history","items":items,
        "offset_items":args.offset_items,"next_offset_items":end,"total_items":total,
        "has_more":end<total,"reasoning_included":false,"revocable_guidance_included":false});
    if args.limit_items == 1
        && items.len() == 1
        && (args.offset_chars > 0 || serde_json::to_vec(&output)?.len() > MAX_OUTPUT_BYTES)
    {
        let serialized = items[0].to_string();
        let total_chars = serialized.chars().count();
        if args.offset_chars > total_chars {
            bail!("Compacted character cursor exceeds item");
        }
        let content = serialized
            .chars()
            .skip(args.offset_chars)
            .take(args.limit_chars)
            .collect::<String>();
        let next = args.offset_chars + content.chars().count();
        return Ok(
            json!({"segment_id":id,"source_digest":entry["source_digest"],
            "source_kind":"compacted_item_page","item_index":args.offset_items,
            "content":content,"offset_chars":args.offset_chars,
            "next_offset_chars":next,"total_chars":total_chars,"has_more":next<total_chars,
            "reasoning_included":false,"revocable_guidance_included":false}),
        );
    }
    if serde_json::to_vec(&output)?.len() > MAX_OUTPUT_BYTES {
        bail!("Compacted page exceeds 16 KiB; request fewer items");
    }
    Ok(output)
}
fn tool_result_page(record: &Value, call_id: &str, args: &Args) -> Result<Value> {
    if record["type"] != "function_call_output" || record["call_id"] != call_id {
        bail!("Tool result provenance conflict");
    }
    let raw = record["output"]
        .as_str()
        .context("Tool result unavailable")?;
    let total = raw.chars().count();
    if args.offset_chars > total {
        bail!("Tool result cursor exceeds source");
    }
    let text = raw
        .chars()
        .skip(args.offset_chars)
        .take(args.limit_chars)
        .collect::<String>();
    let next = args.offset_chars + text.chars().count();
    Ok(
        json!({"tool_call_id":call_id,"source_kind":"current_run_tool_result",
        "content":text,"offset_chars":args.offset_chars,"total_chars":total,
        "next_offset_chars":next,"has_more":next<total}),
    )
}
async fn work(host: &dyn NativeHost, sid: &str, run_id: &str, args: &Value) -> Result<Value> {
    let parsed: Args =
        serde_json::from_value(args.clone()).context("Invalid compacted recall arguments")?;
    validate(&parsed)?;
    let current = view(host, sid, run_id).await?;
    let session = current
        .store
        .get(&format!("session:{sid}"))
        .await?
        .context("Current session unavailable")?;
    let entries = metadata(&session)?;
    let segment_entry = parsed
        .segment_id
        .as_deref()
        .and_then(|id| entries.iter().find(|entry| entry["segment_id"] == id));
    let output = if let Some(entry) = segment_entry {
        let id = parsed.segment_id.as_deref().unwrap();
        let record = current
            .store
            .get(&format!("compaction_segment:{sid}:{id}"))
            .await?
            .context("Compacted segment unavailable")?;
        segment_page(&record, entry, sid, &parsed)?
    } else if let Some(call_id) = parsed.tool_call_id.as_deref() {
        let run = run(&current.store, run_id).await?;
        // Origin comes from the session-owned retention ledger, never a model-supplied run ID.
        let known = tool_output::origin_run_id(&run, call_id);
        let discovered = if known.is_none() {
            host.tool_result_origin(&current.store, sid, call_id)
                .await?
        } else {
            None
        };
        let origin = known
            .or_else(|| discovered.as_ref().map(|(id, _)| id.as_str()))
            .unwrap_or(run_id);
        let origin_run = self::run(&current.store, origin).await?;
        if origin_run["session_id"] != sid || origin_run["agent_id"] != run["agent_id"] {
            bail!("Tool recall origin is unavailable");
        }
        let name = host
            .tool_call_name(&current.store, sid, origin, call_id)
            .await?;
        let permitted = name.as_deref().is_some_and(|name| {
            tool_output::eligible(name)
                || run["tool_result_projection"]["origins"][call_id]["tool"] == name
        });
        let result = if permitted {
            let name = name.as_deref().unwrap();
            let agent = host.scoped_session_agent_id(&current.store, sid).await?;
            let (policy, _, visibility, state) =
                host.scoped_ability_controls(&current.store, sid).await?;
            let mode = host.scoped_session_mode(&current.store, sid).await?;
            let access = host
                .scoped_live_access(&current.store, host.actor(), sid, &agent)
                .await?;
            if host
                .live_tool_decision(
                    &access,
                    name,
                    &policy,
                    &visibility,
                    &state,
                    &run["loaded_abilities"],
                    mode["mode"] == "read_only",
                )
                .permission
                != Permission::Deny
            {
                if let Some(record) = host
                    .tool_result(&current.store, sid, origin, call_id)
                    .await?
                {
                    let raw = record["output"]
                        .as_str()
                        .context("Tool result unavailable")?;
                    if origin != run_id
                        && discovered.is_none()
                        && !tool_output::verify_origin_output(&run, call_id, name, raw)
                    {
                        bail!("Archived tool result does not match retained provenance");
                    }
                    Some(tool_result_page(&record, call_id, &parsed)?)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };
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
    if view(host, sid, run_id).await?.fingerprint != current.fingerprint {
        bail!("Compacted recall access changed during read");
    }
    // A concurrent compaction may replace the session index while a segment
    // is read. Confirm the selected provenance still belongs to this session.
    let latest = current
        .store
        .get(&format!("session:{sid}"))
        .await?
        .context("Current session unavailable")?;
    if segment_entry.is_some()
        && metadata(&latest)?
            .iter()
            .all(|entry| entry["segment_id"] != parsed.segment_id.as_deref().unwrap())
    {
        bail!("Compacted segment index changed during read");
    }
    Ok(output)
}
/// Run `recall_compacted` at `call`'s position; rejections are tool results, not errors.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown compacted recall tool");
    }
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let result = tokio::select! {
        _ = cancel.cancelled() => bail!("tool execution cancelled"),
        result = work(host, call.sid, call.run_id, args) => result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
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
            json!({"type":"message","role":"assistant","content":"after"}),
        ];
        let digest = digest(&source);
        let entry = json!({"segment_id":"segment-1","source_digest":digest,
            "item_count":3,"created_at":1});
        let record = json!({"schema_version":1,"session_id":"owned","segment_id":"segment-1",
            "source_digest":digest,"source":source});
        let args = Args {
            segment_id: Some("segment-1".into()),
            offset_items: 0,
            limit_items: 3,
            tool_call_id: None,
            offset_chars: 0,
            limit_chars: 4096,
        };
        let page = segment_page(&record, &entry, "owned", &args).unwrap();
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert!(!page.to_string().contains("SECRET"));
        assert_eq!(page["total_items"], 3);
        assert!(segment_page(&record, &entry, "foreign", &args).is_err());
        assert!(segment_page(
            &record,
            &entry,
            "owned",
            &Args {
                offset_items: 4,
                ..args
            }
        )
        .is_err());
    }
    #[test]
    fn guidance_and_selectors_do_not_expand_recall_authority() {
        let mut source = vec![
            json!({"type":"function_call","name":"read_own_prompt","call_id":"one"}),
            json!({"type":"function_call_output","call_id":"one","output":"PROMPT SECRET"}),
            json!({"type":"message","role":"assistant","deepseek_reasoning":"THOUGHT","content":"visible"}),
        ];
        mask_guidance(&mut source);
        assert!(!json!(source).to_string().contains("PROMPT SECRET"));
        assert!(!json!(source).to_string().contains("THOUGHT"));
        assert!(serde_json::from_value::<Args>(json!({"session_id":"foreign"})).is_err());
        assert!(validate(&Args {
            segment_id: Some("../foreign".into()),
            offset_items: 0,
            limit_items: 1,
            tool_call_id: None,
            offset_chars: 0,
            limit_chars: 4096
        })
        .is_err());
    }
}
