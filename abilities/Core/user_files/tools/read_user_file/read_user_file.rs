//! read_user_file: byte-paged reads of one saved file.
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_abilities::user_files::{metadata, Repository};
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    name: String,
    offset: Option<usize>,
    limit: Option<usize>,
}
pub(super) async fn run(args: &Value, repo: &Repository<'_, StoreCore>) -> Result<Value> {
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
    Ok(
        json!({"file":metadata(&file),"offset":start,"data":STANDARD.encode(page),"text":std::str::from_utf8(page).ok(),
        "next_offset":if end<bytes.len(){Some(end)}else{None}}),
    )
}
