//! Native reads are confined to the caller-owned session supplied by Rust.
//! No tool argument can select a different user, session, database or private interaction.
use crate::{text_search, Call, NativeHost};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use tokio_util::sync::CancellationToken;
use webagent_files::{attachment_key, uploads};
use webagent_host_core::StoreCore;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(name, "read_attachment" | "search_this_session")
}
/// The model-facing schemas of this family's tools.
#[must_use]
pub fn schemas() -> Vec<Value> {
    [
        ("read_attachment", "Read a bounded Unicode-character page of a current-session text attachment", json!({"attachment_id":{"type":"string","format":"uuid"},"offset_chars":{"type":"integer","minimum":0},"limit_chars":{"type":"integer","minimum":1,"maximum":4096}}), vec!["attachment_id"]),
        ("search_this_session", "Search this conversation's user and assistant messages by keywords; words match by stem (garden finds gardening), so add related words to the query (feline cat). Nearby matches merge into one excerpt", json!({"query":{"type":"string","minLength":1,"maxLength":500},"limit":{"type":"integer","minimum":1,"maximum":20}}), vec!["query"]),
    ].into_iter().map(|(name,description,properties,required)|json!({
        "type":"function","name":name,"description":description,
        "parameters":{"type":"object","properties":properties,"required":required,"additionalProperties":false}
    })).collect()
}
fn attachment_limit() -> usize {
    4096
}
/// Chat rows `search_this_session` reads per page and in total.
const SEARCH_PAGE: i64 = 50;
const SEARCH_ROWS: usize = 5000;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AttachmentArgs {
    attachment_id: String,
    #[serde(default)]
    offset_chars: usize,
    #[serde(default = "attachment_limit")]
    limit_chars: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
}
fn attachment_page(record: Value, args: &AttachmentArgs) -> Result<Value> {
    if !(1..=4096).contains(&args.limit_chars) {
        bail!("Invalid attachment page bounds");
    }
    let media = record["media_type"].as_str().unwrap_or("");
    let textual = media.starts_with("text/")
        || media.starts_with("application/") && media != "application/pdf";
    if !textual
        && record["text"].as_str().is_some_and(|text| !text.is_empty())
        && record["described_by"].as_str().is_none()
    {
        bail!("Binary content lacks native extraction provenance");
    }
    let text=record["text"].as_str().filter(|s|!s.is_empty()).unwrap_or("File metadata only; content extraction is unavailable for this media type. Do not guess its contents.");
    let total = text.chars().count();
    if args.offset_chars > total {
        bail!("Attachment cursor exceeds text length");
    }
    let page: String = text
        .chars()
        .skip(args.offset_chars)
        .take(args.limit_chars)
        .collect();
    let next = args.offset_chars + page.chars().count();
    Ok(
        json!({"id":record["id"],"run_id":record["run_id"],"name":record["name"],
        "media_type":record["media_type"],"bytes":record["bytes"],"sha256":record["sha256"],
        "text":page,"offset_chars":args.offset_chars,"next_offset_chars":next,
        "total_chars":total,"has_more":next<total,"source_kind":"untrusted_attachment"}),
    )
}
fn search_args(args: &SearchArgs) -> Result<BTreeSet<String>> {
    if args.query.trim().is_empty()
        || args.query.chars().count() > 500
        || args.query.chars().any(char::is_control)
        || args.limit.is_some_and(|limit| !(1..=20).contains(&limit))
    {
        bail!("Invalid session search arguments");
    }
    let terms = text_search::query_terms(&args.query);
    if terms.is_empty() {
        bail!("Session search needs at least one word that is not a common stopword");
    }
    Ok(terms)
}
/// Ranked excerpts of the published user and assistant rows in `rows`, oldest first.
fn search_rows(
    rows: &[Value],
    terms: &BTreeSet<String>,
    mut limits: text_search::Limits,
) -> Vec<Value> {
    // One conversation: the per-session limit does not apply.
    limits.per_session = limits.total;
    let docs: Vec<text_search::Doc<'_>> = rows
        .iter()
        .rev()
        // Search only published user/assistant chat. Private tool evidence never
        // enters this helper, and unknown row roles cannot expand that contract.
        .filter(|row| matches!(row["role"].as_str(), Some("user" | "assistant")))
        .filter_map(|row| {
            Some(text_search::Doc {
                group: "",
                meta: json!({"seq":row["seq"],"id":row["id"],"run_id":row["run_id"],
                    "role":row["role"],"created_at":row["created_at"]}),
                text: row["content"].as_str()?,
            })
        })
        .collect();
    text_search::search(terms, &docs, &limits)
}
/// The sent attachment `id` of session `sid`, as the attachment API serves it: the
/// encrypted record only for its owning session, never an unsent draft.
async fn sent_attachment(store: &StoreCore, sid: &str, id: &str) -> Option<Value> {
    if !uploads::upload_id_ok(id) {
        return None;
    }
    let record = store.get(&attachment_key(id)).await.ok()??;
    uploads::sent_view(&record, sid)
}
async fn work(host: &dyn NativeHost, sid: &str, name: &str, args: &Value) -> Result<Value> {
    // The store is already routed to the authenticated caller. Require this
    // session to exist there before any attachment or chat record is resolved.
    host.session_summary(host.store(), sid)
        .await
        .context("Current session unavailable")?;
    match name {
        "read_attachment" => {
            let query: AttachmentArgs =
                serde_json::from_value(args.clone()).context("Invalid attachment arguments")?;
            if !(1..=4096).contains(&query.limit_chars) {
                bail!("Invalid attachment page bounds");
            }
            // Same encrypted lookup and exact session/provenance checks as the API.
            let record = sent_attachment(host.store(), sid, &query.attachment_id)
                .await
                .ok_or_else(|| anyhow::anyhow!("Attachment unavailable in current session"))?;
            if let Some(rid) = record["run_id"].as_str() {
                if !host.model_message_allowed(host.store(), sid, rid).await? {
                    bail!("Attachment excluded from agent context");
                }
            }
            attachment_page(record, &query)
        }
        "search_this_session" => {
            let query: SearchArgs =
                serde_json::from_value(args.clone()).context("Invalid session search arguments")?;
            let terms = search_args(&query)?;
            // The settings only bound excerpt sizes and counts; without readable ability
            // controls (no account record) the defaults apply and the read stays authorized.
            let settings = host
                .ability_controls(sid)
                .await
                .map(|(_, _, visibility, _)| visibility["settings"]["agent_management"].clone())
                .unwrap_or_default();
            let mut limits = text_search::Limits::from_settings(&settings);
            if let Some(limit) = query.limit {
                limits.total = limit.min(limits.total);
            }
            let mut rows = Vec::new();
            let mut after = 0;
            let mut complete = false;
            while rows.len() < SEARCH_ROWS {
                let page = host
                    .model_chat_page(host.store(), sid, after, SEARCH_PAGE)
                    .await
                    .context("Current-session chat unavailable")?;
                let messages = page["messages"]
                    .as_array()
                    .context("Chat page unavailable")?;
                for message in messages {
                    let seq = message["seq"].as_i64().context("Chat cursor unavailable")?;
                    if seq <= after {
                        bail!("Chat cursor order conflict");
                    }
                    after = seq;
                    rows.push(message.clone());
                }
                if messages.is_empty() || page["has_more"] != true {
                    complete = true;
                    break;
                }
            }
            Ok(
                json!({"results":search_rows(&rows,&terms,limits),"scanned":rows.len(),
                "scan_complete":complete,"limits":limits.describe(),"source_kind":"public_chat"}),
            )
        }
        _ => bail!("Unknown session read tool"),
    }
}
/// Run session read tool `name`; the session is always `call`'s, never an argument.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    let result = tokio::select! {
        _=cancel.cancelled()=>bail!("tool execution cancelled"),
        result=work(host,call.sid,name,args)=>result,
    };
    if cancel.is_cancelled() {
        bail!("tool execution cancelled");
    }
    Ok(match result {
        Ok(output) => json!({"status":"success","output":output}),
        Err(error) => {
            json!({"status":"error","error_kind":"session_read_rejected","output":error.to_string()})
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_attachment_pages_have_character_cursors_and_type_limits() {
        let record = json!({"id":"id","run_id":"run","name":"n","media_type":"text/plain",
            "bytes":8,"sha256":"digest","text":"aé🙂z"});
        let args = AttachmentArgs {
            attachment_id: "id".into(),
            offset_chars: 1,
            limit_chars: 2,
        };
        let page = attachment_page(record.clone(), &args).unwrap();
        assert_eq!(page["text"], "é🙂");
        assert_eq!(page["next_offset_chars"], 3);
        assert_eq!(page["has_more"], true);
        assert_eq!(page["total_chars"], 4);
        assert!(attachment_page(
            record.clone(),
            &AttachmentArgs {
                offset_chars: 5,
                ..args
            }
        )
        .is_err());
        let mut binary = record;
        binary["media_type"] = json!("image/png");
        assert!(attachment_page(
            binary,
            &AttachmentArgs {
                attachment_id: "id".into(),
                offset_chars: 0,
                limit_chars: 2
            }
        )
        .is_err());
    }
    #[test]
    fn attachment_reader_has_no_builtin_maximum_offset() {
        let record = json!({"id":"id","run_id":"run","name":"n","media_type":"text/plain",
            "bytes":26214402,"sha256":"digest","text":"x".repeat(26214401)+"Z"});
        let page = attachment_page(
            record,
            &AttachmentArgs {
                attachment_id: "id".into(),
                offset_chars: 26214401,
                limit_chars: 1,
            },
        )
        .unwrap();
        assert_eq!(page["text"], "Z");
        assert_eq!(page["has_more"], false);
    }
    #[test]
    fn search_ranks_public_rows_and_validates_arguments() {
        let rows = [
            json!({"seq":1,"id":"m1","run_id":"r","role":"tool","created_at":1,"content":"gardening secret"}),
            json!({"seq":2,"id":"m2","run_id":"r","role":"user","created_at":2,"content":"Planning the garden beds"}),
            json!({"seq":3,"id":"m3","run_id":"r","role":"assistant","created_at":3,"content":"Gardening tips: water early"}),
        ];
        let args = SearchArgs {
            query: "gardens".into(),
            limit: None,
        };
        let terms = search_args(&args).unwrap();
        let limits = text_search::Limits::from_settings(&json!({"search_results_per_session":1}));
        let hits = search_rows(&rows, &terms, limits);
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|hit| hit["role"] != "tool"));
        assert!(hits[0]["excerpt"].as_str().unwrap().contains("arden"));
        for bad in [
            json!({"query":""}),
            json!({"query":"the"}),
            json!({"query":"x","limit":0}),
        ] {
            let parsed: SearchArgs = serde_json::from_value(bad).unwrap();
            assert!(search_args(&parsed).is_err());
        }
        assert!(
            serde_json::from_value::<SearchArgs>(json!({"query":"x","session_id":"foreign"}))
                .is_err()
        );
        assert!(serde_json::from_value::<AttachmentArgs>(
            json!({"attachment_id":"x","user_id":"foreign"})
        )
        .is_err());
    }
}
