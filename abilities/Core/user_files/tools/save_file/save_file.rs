//! save_file: stores a text or base64 file in the owner's private records.
use super::{Call, NativeHost};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use webagent_abilities::user_files::Repository;
use webagent_files::uploads::allowed_media;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Save {
    name: String,
    media_type: Option<String>,
    text: Option<String>,
    data: Option<String>,
    expected_revision: u64,
}
pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    run: &Value,
    args: &Value,
    repo: &Repository<'_, StoreCore>,
) -> Result<Value> {
    let sid = call.sid;
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
    .await
}
