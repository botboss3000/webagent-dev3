//! Reply buttons: a choice card under the agent's message whose click sends the
//! label as the user's own next message.
use crate::{human_escalation::text, Call, NativeHost};
use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_host_core::digest;

/// Whether this family executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "offer_choices"
}
/// Tools this adapter executes; their schemas live in the ability's `tools/<tool>/tool.json`.
pub const TOOLS: &[&str] = &["offer_choices"];

fn choices(id: &str, prompt: &str, options: &[Value]) -> Result<Value> {
    if options.is_empty() || options.len() > 8 {
        bail!("Expected 1..8 options");
    }
    let mut clean = Vec::new();
    for (n, option) in options.iter().enumerate() {
        let label = text(
            option
                .as_str()
                .ok_or_else(|| anyhow!("Option must be text"))?,
            80,
        )?;
        if label.is_empty() || label.contains('\n') {
            bail!("Option labels must be one non-empty line");
        }
        clean.push(json!({"id":format!("option-{}",n+1),"label":label}));
    }
    // user_reply: the browser sends the clicked label as the user's own message
    // through normal admission; the card itself carries no authority.
    Ok(
        json!({"id":id,"type":"choice","title":"Choose","placement":"inline","data":{"prompt":text(prompt,200)?,"options":clean},
        "lifecycle":{},"authority":"user_reply"}),
    )
}

/// Run reply-button tool `name` under fresh run authority at `call`'s position.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    if !owns(name) {
        bail!("Unknown reply buttons tool");
    }
    let (sid, pending) = (call.sid, call.pending_id);
    let run = host.authorize(call, name, args, &cancel).await?;
    let id = format!("choices-{}", &digest(&json!(pending))[..24]);
    let spec = choices(
        &id,
        args["prompt"].as_str().unwrap_or(""),
        args["options"].as_array().map(Vec::as_slice).unwrap_or(&[]),
    )?;
    // A session lists at most 64 components; keep headroom for other displays.
    if host.chat_components(host.store(), sid).await?.len() >= 48 {
        bail!("This conversation has too many choice cards; ask in text instead");
    }
    let generation = run["generation"]
        .as_str()
        .ok_or_else(|| anyhow!("Run generation missing"))?;
    let output = host
        .write_artifact(Call { generation, ..call }, "component", &id, 0, &spec)
        .await?;
    Ok(json!({"status":"success","output":output}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn choices_are_bounded_reply_cards() {
        let spec = choices(
            "choices-1",
            "Pick a time",
            &[json!("10:00"), json!("11:30")],
        )
        .unwrap();
        assert_eq!(spec["id"], "choices-1");
        assert_eq!(spec["authority"], "user_reply");
        assert_eq!(spec["data"]["options"][1]["label"], "11:30");
        assert!(choices("c", "x", &[]).is_err());
        assert!(choices("c", "x", &[json!("a\nb")]).is_err());
        assert!(choices("c", "x", &vec![json!("a"); 9]).is_err());
    }
    #[test]
    fn schemas_match_owned_tools() {
        let names: Vec<_> = crate::schemas(TOOLS)
            .iter()
            .map(|s| s["name"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["offer_choices"]);
    }
}
