//! main_notes_write: one contribution to the notes against their expected revision.
use crate::{Call, NativeHost};
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use webagent_chat::main_chat::{identifier, Contribution};

/// `main_notes_write`'s arguments: one contribution against the expected revision.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct WriteArgs {
    request_id: String,
    expected_revision: u64,
    contribution: Contribution,
}

pub(super) async fn run(host: &dyn NativeHost, call: Call<'_>, args: &Value) -> Result<Value> {
    let write: WriteArgs = webagent_abilities::tool_arguments::read("main_notes_write", args)?;
    if !identifier(&write.request_id) {
        bail!("Stable request ID required");
    }
    // A retry with the same request and arguments replays the first result.
    let fingerprint = webagent_chat::digest(&json!([call.sid, args]));
    let result = host
        .main_notes_contribute(
            call.sid,
            write.expected_revision,
            &write.request_id,
            &fingerprint,
            write.contribution,
        )
        .await?;
    Ok(json!({"status":"success","output":result}))
}
