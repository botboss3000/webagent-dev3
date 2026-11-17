//! read_attachment: character-paged reads of this session's sent attachments.
use super::NativeHost;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_files::{attachment_key, uploads};
use webagent_host_core::StoreCore;

fn attachment_limit() -> usize {
    4096
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachmentArgs {
    attachment_id: String,
    #[serde(default)]
    offset_chars: usize,
    #[serde(default = "attachment_limit")]
    limit_chars: usize,
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
/// The sent attachment `id` of session `sid`, as the attachment API serves it: the
/// encrypted record only for its owning session, never an unsent draft.
async fn sent_attachment(store: &StoreCore, sid: &str, id: &str) -> Option<Value> {
    if !uploads::upload_id_ok(id) {
        return None;
    }
    let record = store.get(&attachment_key(id)).await.ok()??;
    uploads::sent_view(&record, sid)
}
pub(super) async fn run(host: &dyn NativeHost, sid: &str, args: &Value) -> Result<Value> {
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
}
