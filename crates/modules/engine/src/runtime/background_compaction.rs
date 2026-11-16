//! Background context summaries: at most one per session. The job's state lives in its own
//! store key, so it never races run saves; this process-local registry tells a live job from
//! one a restart cut off.
use crate::context_compaction::{Boundary, Config, Plan, PlanKind};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use tokio::sync::watch;

static JOBS: OnceLock<Mutex<HashMap<String, watch::Receiver<bool>>>> = OnceLock::new();

fn jobs() -> std::sync::MutexGuard<'static, HashMap<String, watch::Receiver<bool>>> {
    JOBS.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner())
}

/// Store key holding a session's job: `state` is `running`, `done`, `failed` or `idle`.
pub fn key(sid: &str) -> String {
    format!("context_compaction_job:{sid}")
}

/// A live job's completion signal; `None` when this process runs no job for the session.
pub fn running(sid: &str) -> Option<watch::Receiver<bool>> {
    jobs().get(sid).cloned()
}

/// Record a job as live before it is spawned.
pub fn register(sid: &str) -> watch::Sender<bool> {
    let (sender, receiver) = watch::channel(false);
    jobs().insert(sid.to_owned(), receiver);
    sender
}

/// Signal waiters after the job's state is stored, then forget it.
pub fn finish(sid: &str, sender: watch::Sender<bool>) {
    let _ = sender.send(true);
    jobs().remove(sid);
}

/// The summary text of a provider response, or `None` when it called a tool or wrote nothing.
pub fn summary_text(response: &Value) -> Option<String> {
    let output = response["output"].as_array()?;
    if output.iter().any(|item| item["type"] == "function_call") {
        return None;
    }
    let text = output
        .iter()
        .filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|item| item["text"].as_str())
        .collect::<String>();
    (!text.trim().is_empty()).then_some(text)
}

/// Provider-reported cache reads, by the same fields usage accounting reads.
pub fn cached_tokens(response: &Value) -> Option<u64> {
    let usage = &response["usage"];
    usage["input_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| usage["prompt_tokens_details"]["cached_tokens"].as_u64())
        .or_else(|| usage["prompt_cache_hit_tokens"].as_u64())
        .or_else(|| usage["cached_input_tokens"].as_u64())
}

/// The private message that turns a copy of the running request into a summary request.
/// History is never edited (that would break the cached prefix), so the range is named by
/// what the model can see: the earlier blocks and the item where the range ends.
pub fn fork_message(plan: &Plan, history: &[Value], config: &Config) -> String {
    let start = match plan.first {
        0 => "from the start of the conversation".to_owned(),
        1 => "after the earlier summary block at the start".to_owned(),
        n => format!("after the {n} earlier summary blocks at the start"),
    };
    let end = match plan.boundary {
        Boundary::CompletedToolExchange => {
            let call = history[plan.drop_until - 1]["call_id"]
                .as_str()
                .unwrap_or_default();
            format!("through the result of tool call {call}")
        }
        Boundary::PriorTurn => {
            let next = history[plan.drop_until]["content"]
                .as_str()
                .unwrap_or_default();
            let opening: String = next.chars().take(80).collect();
            format!("up to, but not including, the user message that begins \"{opening}\"")
        }
    };
    let template = if plan.kind == PlanKind::Fold && !config.block_prompt.is_empty() {
        &config.block_prompt
    } else {
        &config.summary_prompt
    };
    let instructions = template
        .replace(
            "{summary_characters}",
            &(config.summary_tokens * config.characters_per_token).to_string(),
        )
        .replace(
            "{source}",
            &format!("(not repeated here: the conversation items above, {start}, {end})"),
        );
    format!(
        "Context maintenance, not a user request: pause the task and write one summary block \
         of earlier conversation items. Do not call any tools and do not continue the task in \
         this reply.\n\n{instructions}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tool_call_or_empty_answer_is_not_a_summary() {
        let text = json!({"output":[{"type":"message","content":[{"text":"block"}]}]});
        assert_eq!(summary_text(&text).as_deref(), Some("block"));
        let call = json!({"output":[{"type":"function_call","name":"file_read"}]});
        assert!(summary_text(&call).is_none());
        assert!(summary_text(&json!({"output":[]})).is_none());
    }

    #[test]
    fn the_registry_tells_a_live_job_from_a_finished_one() {
        let sender = register("bg-test");
        let mut receiver = running("bg-test").unwrap();
        finish("bg-test", sender);
        assert!(running("bg-test").is_none());
        assert!(*receiver.borrow_and_update());
    }
}
