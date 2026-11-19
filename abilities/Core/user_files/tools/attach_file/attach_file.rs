//! attach_file: attaches a saved file to the current session.
use super::{Call, NativeHost};
use anyhow::{anyhow, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use webagent_abilities::user_files::Repository;
use webagent_host_core::StoreCore;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Attach {
    name: String,
}
fn stable_uuid(input: &str) -> String {
    let hash = webagent_host_core::digest(&json!(input));
    let bytes = hex::decode(&hash[..32]).unwrap();
    Uuid::from_slice(&bytes).unwrap().to_string()
}
pub(super) async fn run(
    host: &dyn NativeHost,
    call: Call<'_>,
    args: &Value,
    repo: &Repository<'_, StoreCore>,
) -> Result<Value> {
    let sid = call.sid;
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
    Ok(json!({"file":info,"url":format!("/api/v3/sessions/{sid}/attachments/{id}/content")}))
}
