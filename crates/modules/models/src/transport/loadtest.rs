//! The `loadtest` provider: the server answers a model call itself, for capacity tests.
//!
//! The user message since the last user turn programs the reply: each `TOOL <name> <json>`
//! line is one tool call, made in order on successive model turns, and `REPLY <text>` is the
//! final answer (default "OK"). `THINK <ms>` (at most 60 s) waits before each turn, to mimic
//! model latency. Nothing leaves the server.
use serde_json::{json, Value};

/// Most a scripted turn may wait.
const MAX_THINK_MS: u64 = 60_000;

fn text_of(message: &Value) -> String {
    match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// The assistant message for this turn, and how long to wait before answering.
pub fn reply(body: &Value) -> (Value, std::time::Duration) {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    let last_user = messages.iter().rposition(|m| m["role"] == "user");
    let script = last_user.map(|i| text_of(&messages[i])).unwrap_or_default();
    let mut tools = Vec::new();
    let mut answer = "OK".to_owned();
    let mut think = 0;
    for line in script.lines() {
        if let Some(rest) = line.strip_prefix("TOOL ") {
            let (name, arguments) = rest.split_once(' ').unwrap_or((rest, "{}"));
            tools.push((name.trim().to_owned(), arguments.trim().to_owned()));
        } else if let Some(rest) = line.strip_prefix("REPLY ") {
            answer = rest.to_owned();
        } else if let Some(rest) = line.strip_prefix("THINK ") {
            think = rest.trim().parse::<u64>().unwrap_or(0).min(MAX_THINK_MS);
        }
    }
    let done = last_user.map_or(0, |i| {
        messages[i + 1..]
            .iter()
            .filter(|m| m["role"] == "assistant" && m["tool_calls"].is_array())
            .count()
    });
    let wait = std::time::Duration::from_millis(think);
    match tools.get(done) {
        Some((name, arguments)) => (
            json!({"role":"assistant","content":null,"tool_calls":[{
                "id":format!("call_loadtest_{done}"),"type":"function",
                "function":{"name":name,"arguments":if arguments.is_empty() { "{}" } else { arguments }}}]}),
            wait,
        ),
        None => (json!({"role":"assistant","content":answer}), wait),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_calls_each_tool_once_then_replies() {
        let script = "THINK 5\nTOOL get_time {}\nTOOL web_read {\"url\":\"https://example.com\"}\nREPLY done";
        let mut body =
            json!({"messages":[{"role":"system","content":"s"},{"role":"user","content":script}]});
        let (first, wait) = reply(&body);
        assert_eq!(wait, std::time::Duration::from_millis(5));
        assert_eq!(first["tool_calls"][0]["function"]["name"], "get_time");
        body["messages"]
            .as_array_mut()
            .unwrap()
            .extend([first, json!({"role":"tool","content":"12:00"})]);
        let (second, _) = reply(&body);
        assert_eq!(
            second["tool_calls"][0]["function"]["arguments"],
            "{\"url\":\"https://example.com\"}"
        );
        body["messages"]
            .as_array_mut()
            .unwrap()
            .extend([second, json!({"role":"tool","content":"page"})]);
        let (last, _) = reply(&body);
        assert_eq!(last["content"], "done");
        assert!(last.get("tool_calls").is_none());
    }

    #[test]
    fn plain_message_gets_ok_and_think_is_capped() {
        let body =
            json!({"messages":[{"role":"user","content":[{"type":"text","text":"THINK 999999"}]}]});
        let (message, wait) = reply(&body);
        assert_eq!(message["content"], "OK");
        assert_eq!(wait, std::time::Duration::from_millis(MAX_THINK_MS));
    }
}
