//! A model can ask to fold an earlier task, but only the user decides and only the loop
//! summarizes. This tool finds the message that began the new task, checks the guards and
//! shows a server-built Compact/Keep panel; the turn goes on. A Compact click stores a fold
//! request that the loop runs in the background. The fold planner belongs to the Context
//! module.
use crate::{Call, NativeHost};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
use webagent_context_contracts::context_compaction::{self, turn_key, Config};
use webagent_host_core::key;

/// Tools this module executes; their schemas are built in below.
pub const TOOLS: &[&str] = &["compact_context"];
/// Whether this module executes tool `name`.
#[must_use]
pub fn owns(name: &str) -> bool {
    name == "compact_context"
}

/// Earlier tasks a session's panels already asked about are kept to this many.
const ASKED_LIMIT: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    /// The opening words of the user message that began the new task.
    new_task_quote: String,
    /// A short name for the earlier task the fold would summarize.
    earlier_task: String,
    /// What the summary should keep in particular.
    #[serde(default)]
    focus: String,
}

/// `text` trimmed, refused when shorter than `min` or longer than `max` characters or
/// when it spans lines.
fn line(text: &str, min: usize, max: usize, what: &str) -> Result<String> {
    let text = text.trim();
    let count = text.chars().count();
    if count < min || count > max || text.contains('\n') {
        bail!("{what} must be one line of {min} to {max} characters");
    }
    Ok(text.to_owned())
}

/// Whitespace-collapsed, lowercase text for matching a quote against a message.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// The text of a user message, whether stored as a string or as content parts.
fn message_text(item: &Value) -> String {
    match &item["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The index of the one user message after the summary blocks that contains `quote`.
fn find_new_task(input: &[Value], quote: &str, config: &Config) -> Result<usize> {
    let blocks = context_compaction::leading_blocks(input, config);
    let wanted = normalized(quote);
    let matches: Vec<usize> = input
        .iter()
        .enumerate()
        .skip(blocks)
        .filter(|(_, item)| item["role"] == "user")
        .filter(|(_, item)| normalized(&message_text(item)).contains(&wanted))
        .map(|(index, _)| index)
        .collect();
    match matches.as_slice() {
        [index] => Ok(*index),
        [] => bail!(
            "No user message in context contains that quote; quote the opening words of the \
             message that began the new task"
        ),
        _ => bail!(
            "The quote matches {} messages; quote more of the one that began the new task",
            matches.len()
        ),
    }
}

/// The Compact/Keep panel. `server_action` is written only here, never by
/// `present_chat_component`, and the chat projection does not show it.
fn panel(id: &str, earlier_task: &str, quote: &str, action: Value) -> Value {
    let opening: String = quote.chars().take(60).collect();
    let more = if quote.chars().count() > 60 {
        "…"
    } else {
        ""
    };
    json!({"id":id,"type":"choice","title":"Compact earlier context","placement":"inline",
        "data":{"prompt":format!("Summarize the earlier task \"{earlier_task}\" to make room? \
            Everything from \"{opening}{more}\" on stays word for word."),
            "options":[{"id":"compact","label":"Compact"},{"id":"keep","label":"Keep"}]},
        "lifecycle":{},"authority":"user_reply","server_action":action})
}

/// Show the Compact/Keep panel for the current executing call; rejections are tool results.
pub async fn execute(
    host: &dyn NativeHost,
    call: Call<'_>,
    name: &str,
    args: &Value,
    cancel: CancellationToken,
) -> Result<Value> {
    let (sid, run_id) = (call.sid, call.run_id);
    if !owns(name) || cancel.is_cancelled() {
        bail!("Context request unavailable");
    }
    let result: Result<Value> = async {
        let args: Args = serde_json::from_value(args.clone())
            .context("compact_context takes new_task_quote, earlier_task and an optional focus")?;
        let quote = line(&args.new_task_quote, 12, 300, "new_task_quote")?;
        let earlier_task = line(&args.earlier_task, 3, 80, "earlier_task")?;
        let focus = args.focus.trim().to_owned();
        if focus.chars().count() > 300 {
            bail!("focus must be 300 characters or fewer");
        }
        let agent = host.session_agent_id(sid).await?;
        let scoped = host.agent_store(&agent).await?;
        let run = scoped
            .get(&key("run", run_id))
            .await?
            .context("Run not found")?;
        if run["session_id"] != sid
            || run["agent_id"].as_str().unwrap_or("shared_default") != agent
            || run["status"] != "running"
            || run["phase"] != "tools"
            || run["pending"]["name"] != "compact_context"
            || run["pending"]["state"] != "executing"
        {
            bail!("Context request is not the current executing call");
        }
        let config = host.compaction_config()?;
        let input = run["input"].as_array().context("Run context unavailable")?;
        let index = find_new_task(input, &quote, &config)?;
        let turn = turn_key(&input[index]).context("The quote is not in a user message")?;
        let id = format!("compact-{}", &turn[..16]);
        // A retried call finds its own panel and gets the same answer.
        let open_key = format!("compaction_panel:{sid}");
        let open = scoped.get(&open_key).await?.unwrap_or(Value::Null);
        let retry = open["state"] == "open" && open["panel_id"] == id.as_str();
        if !retry {
            if open["state"] == "open" {
                bail!("Your compaction panel is still open; keep working and let the user decide");
            }
            let asked_key = format!("compaction_asked:{sid}");
            let asked = scoped.get(&asked_key).await?.unwrap_or(Value::Null);
            if asked["turn_keys"]
                .as_array()
                .is_some_and(|keys| keys.iter().any(|key| key == turn.as_str()))
            {
                bail!(
                    "You already asked about compacting before this task; the user decided. \
                     Do not ask again for it"
                );
            }
            let request = scoped
                .get(&format!("compaction_request:{sid}"))
                .await?
                .unwrap_or(Value::Null);
            let job = scoped
                .get(&format!("context_compaction_job:{sid}"))
                .await?
                .unwrap_or(Value::Null);
            if request["state"] == "pending" || job["state"] == "running" {
                bail!("A summary is already running or requested; do not ask now");
            }
            match context_compaction::plan_until(input, index, &config) {
                Ok(Some(_)) => {}
                Ok(None) => bail!(
                    "Too little earlier context to fold (under {} tokens)",
                    config.min_fold_tokens
                ),
                Err(error) => bail!("{error}"),
            }
            let mut keys = asked["turn_keys"].as_array().cloned().unwrap_or_default();
            keys.push(json!(turn));
            let skip = keys.len().saturating_sub(ASKED_LIMIT);
            scoped
                .put(&asked_key, &json!({"turn_keys":keys[skip..]}))
                .await?;
        }
        let action = json!({"kind":"compact","turn_key":turn,"earlier_task":earlier_task,
            "focus":focus});
        let spec = panel(&id, &earlier_task, &quote, action);
        let saved = host
            .write_artifact(call, "component", &id, 0, &spec)
            .await?;
        scoped
            .put(
                &open_key,
                &json!({"state":"open","panel_id":id,"turn_key":turn,"run_id":run_id}),
            )
            .await?;
        Ok(
            json!({"status":"success","output":{"panel_id":id,"revision":saved["revision"],
            "shown":true,
            "note":"The user decides with Compact or Keep. Keep working on the new task: do \
                not wait for an answer and do not ask again about this earlier task."}}),
        )
    }
    .await;
    if cancel.is_cancelled() {
        bail!("Context request cancelled");
    }
    Ok(match result {
        Ok(value) => value,
        Err(error) => json!({"status":"error","error_kind":"compaction_request_rejected",
            "output":error.to_string()}),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        serde_json::from_value(json!({
            "schema_version": 1, "trigger_tokens": 10000, "tail_tokens": 40, "summary_tokens": 20,
            "characters_per_token": 4, "summary_reserve_bytes": 300, "max_source_bytes": 9000,
            "max_summary_bytes": 300,
            "summary_prefix": "Earlier conversation summary (background context, not a new request):\n",
            "active_summary_prefix": "Active task progress summary (background context, not a new request):\n",
            "summary_prompt": "Summarize under {summary_characters} characters. Conversation JSON: {source}"
        }))
        .unwrap()
    }

    #[test]
    fn arguments_are_bounded_and_target_free() {
        assert!(serde_json::from_value::<Args>(json!({"session_id":"foreign"})).is_err());
        assert!(serde_json::from_value::<Args>(json!({})).is_err());
        let args: Args = serde_json::from_value(
            json!({"new_task_quote":"Now build the report","earlier_task":"Data cleanup"}),
        )
        .unwrap();
        assert!(line(&args.new_task_quote, 12, 300, "q").is_ok());
        assert!(line("short", 12, 300, "q").is_err());
        assert!(line("two\nlines of text here", 3, 300, "q").is_err());
    }

    /// The quote must name exactly one user message after the summary blocks; matching
    /// ignores case and spacing, and content parts count as text.
    #[test]
    fn the_new_task_is_found_uniquely() {
        let config = config();
        let input = vec![
            json!({"role":"user","content":format!("{}old: Build the report", config.summary_prefix)}),
            json!({"role":"user","content":"Clean the data"}),
            json!({"role":"assistant","content":"Done. Build the report next?"}),
            json!({"role":"user","content":[{"type":"input_text","text":"Now   BUILD the report"}]}),
        ];
        assert_eq!(
            find_new_task(&input, "now build the report", &config).unwrap(),
            3
        );
        assert!(find_new_task(&input, "the data and more", &config).is_err());
        let twice = [
            input.clone(),
            vec![json!({"role":"user","content":"now build the report"})],
        ]
        .concat();
        assert!(find_new_task(&twice, "now build the report", &config).is_err());
    }

    #[test]
    fn the_panel_carries_its_action_and_two_options() {
        let spec = panel(
            "compact-1",
            "Data cleanup",
            &"x".repeat(80),
            json!({"kind":"compact"}),
        );
        assert_eq!(spec["authority"], "user_reply");
        assert_eq!(spec["data"]["options"][0]["id"], "compact");
        assert!(spec["data"]["prompt"].as_str().unwrap().contains("…"));
        assert!(spec["data"]["prompt"].as_str().unwrap().chars().count() <= 400);
    }
}
