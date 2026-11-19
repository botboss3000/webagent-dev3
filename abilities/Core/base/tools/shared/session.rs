//! Native reads are confined to the caller-owned session supplied by Rust.
//! No tool argument can select a different user, session, database or private interaction.
use crate::{Call, NativeHost};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    matches!(name, "read_attachment" | "search_this_session")
}
#[path = "../read_attachment/read_attachment.rs"]
mod read_attachment;
#[path = "../search_this_session/search_this_session.rs"]
mod search_this_session;
/// Argument types of this family's tools, checked against their tool.json by the crate test.
pub const ARGUMENT_TYPES: &[webagent_abilities::tool_arguments::ArgumentType] = &[
    webagent_abilities::argument_type!("read_attachment", read_attachment::AttachmentArgs),
    webagent_abilities::argument_type!("search_this_session", search_this_session::SearchArgs),
];
async fn work(host: &dyn NativeHost, sid: &str, name: &str, args: &Value) -> Result<Value> {
    // The store is already routed to the authenticated caller. Require this
    // session to exist there before any attachment or chat record is resolved.
    host.session_summary(host.store(), sid)
        .await
        .context("Current session unavailable")?;
    match name {
        "read_attachment" => read_attachment::run(host, sid, args).await,
        "search_this_session" => search_this_session::run(host, sid, args).await,
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
#[path = "../../tests/session.rs"]
mod tests;
