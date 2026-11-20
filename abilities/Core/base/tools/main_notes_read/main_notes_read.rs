//! main_notes_read: one page of the notes' topics, or of one topic's entries.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};

/// `main_notes_read`'s arguments: one page of topics, or of one topic's entries.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ReadArgs {
    #[serde(default)]
    offset: Option<u64>,
    #[serde(default)]
    limit: Option<u64>,
    #[serde(default)]
    topic_id: Option<String>,
}

pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, args: &Value) -> Result<Value> {
    let page: ReadArgs = webagent_abilities::tool_arguments::read("main_notes_read", args)?;
    let state = host.main_notes_state(call.sid).await?;
    let offset = page.offset.unwrap_or(0) as usize;
    let limit = page.limit.unwrap_or(20) as usize;
    if !(1..=20).contains(&limit) || offset > 65536 {
        bail!("Invalid Main Notes page");
    }
    let rows: Vec<Value> = match page.topic_id.as_deref() {
        Some(topic) => state["topics"][topic]["entries"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, entry)| entry.clone())
            .collect(),
        None => state["topics"]
            .as_object()
            .into_iter()
            .flatten()
            .map(|(id, topic)| {
                json!({"id":id,"title":topic["title"],"scope":topic["scope"],
                    "approved_scope":topic["approved_scope"],"groups":topic["groups"],
                    "revision":topic["revision"],
                    "entry_count":topic["entries"].as_object().map_or(0, |e| e.len())})
            })
            .collect(),
    };
    let total = rows.len();
    let items: Vec<Value> = rows.into_iter().skip(offset).take(limit).collect();
    let next = offset + items.len();
    Ok(
        json!({"status":"success","source_kind":"untrusted_main_notes","output":{
        "revision":state["revision"],"notes_version":state["notes_version"],
        "settings":state["settings"],"session_id":state["session_id"],"items":items,
        "total":total,"next_offset":next,"has_more":next < total}}),
    )
}
