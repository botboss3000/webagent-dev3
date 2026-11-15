//! Native User Files adapter. The actor's private database owns files; no host path is accepted.
use crate::{Call, NativeHost};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use webagent_abilities::user_files::{metadata, Repository};
use webagent_files::uploads::allowed_media;

#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(
        name,
        "save_file" | "list_user_files" | "read_user_file" | "attach_file"
    )
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &[
    "save_file",
    "list_user_files",
    "read_user_file",
    "attach_file",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Save {
    name: String,
    media_type: Option<String>,
    text: Option<String>,
    data: Option<String>,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    name: String,
    offset: Option<usize>,
    limit: Option<usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct List {
    offset: Option<u32>,
    limit: Option<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    name: String,
}
fn stable_uuid(input: &str) -> String {
    let hash = webagent_host_core::digest(&json!(input));
    let bytes = hex::decode(&hash[..32]).unwrap();
    Uuid::from_slice(&bytes).unwrap().to_string()
}
/// Saves, lists, reads or attaches the owner's private files.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if cancel.is_cancelled() {
        return Err(anyhow!("File operation cancelled"));
    }
    let run = host.authorize(call, name, args, &cancel).await?;
    let actor = host.actor();
    if actor != host.store().current_user_id() {
        return Err(anyhow!(
            "Use an owner-private conversation for private saved files"
        ));
    }
    let sid = call.sid;
    let user = host.user_store(actor).await?;
    let repo = Repository::new(&user.pool, &user);
    let output = match name {
        "save_file" => {
            let q: Save = serde_json::from_value(args.clone())?;
            webagent_abilities::user_files::name(&q.name)?;
            let bytes = match (q.text, q.data) {
                (Some(text), None) => text.into_bytes(),
                (None, Some(data)) => STANDARD
                    .decode(data)
                    .map_err(|_| anyhow!("Invalid file base64"))?,
                _ => return Err(anyhow!("Supply exactly one of text or data")),
            };
            let media = q.media_type.unwrap_or_else(|| "text/plain".into());
            if !allowed_media(&media) {
                return Err(anyhow!("Unsupported file media type"));
            }
            let limits = host.attachment_limits(sid).await?;
            if limits
                .max_file_size_mb
                .is_some_and(|n| bytes.len() > n.saturating_mul(1024 * 1024))
            {
                return Err(anyhow!("File exceeds configured attachment limit"));
            }
            let record = json!({"name":q.name,"media_type":media,"bytes":bytes.len(),"data":STANDARD.encode(&bytes),
                "sha256":hex::encode(Sha256::digest(&bytes))});
            repo.save(
                &record,
                q.expected_revision,
                &webagent_abilities::user_files::Invocation {
                    session_id: sid,
                    run_id: call.run_id,
                    pending_id: call.pending_id,
                    generation: run["generation"]
                        .as_str()
                        .ok_or_else(|| anyhow!("Run generation unavailable"))?,
                    arguments: args,
                },
            )
            .await?
        }
        "list_user_files" => {
            let q: List = serde_json::from_value(args.clone())?;
            let offset = q.offset.unwrap_or(0);
            let limit = q.limit.unwrap_or(50);
            let files = repo.list(offset, limit).await?;
            let next = if files.len() == limit as usize {
                Some(offset.saturating_add(limit))
            } else {
                None
            };
            json!({"files":files,"next_offset":next})
        }
        "read_user_file" => {
            let q: Read = serde_json::from_value(args.clone())?;
            let file = repo
                .read(&q.name)
                .await?
                .ok_or_else(|| anyhow!("User file not found"))?;
            let bytes = STANDARD.decode(file["data"].as_str().unwrap_or(""))?;
            let start = q.offset.unwrap_or(0).min(bytes.len());
            let limit = q.limit.unwrap_or(16384);
            if !(1..=65536).contains(&limit) {
                return Err(anyhow!("Read limit must be 1..65536"));
            }
            let end = start.saturating_add(limit).min(bytes.len());
            let page = &bytes[start..end];
            json!({"file":metadata(&file),"offset":start,"data":STANDARD.encode(page),"text":std::str::from_utf8(page).ok(),
                "next_offset":if end<bytes.len(){Some(end)}else{None}})
        }
        "attach_file" => {
            let q: Attach = serde_json::from_value(args.clone())?;
            let file = repo
                .read(&q.name)
                .await?
                .ok_or_else(|| anyhow!("User file not found"))?;
            let id = stable_uuid(&format!("{sid}:{}:{}", call.run_id, call.pending_id));
            let info = host
                .upload(
                    sid,
                    json!({"client_upload_id":id,"name":file["name"],"media_type":file["media_type"],"data":file["data"]}),
                )
                .await?;
            json!({"file":info,"url":format!("/api/v3/sessions/{sid}/attachments/{id}/content")})
        }
        _ => return Err(anyhow!("Unknown User Files tool")),
    };
    Ok(json!({"status":"success","output":output}))
}
