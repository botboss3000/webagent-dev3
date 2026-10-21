//! Interpret normalized provider output without authorizing or publishing it.
use serde_json::{json,Value};

pub fn tool_calls(output: &[Value], control_name: &str) -> Vec<Value> {
    output.iter().filter(|item| item["type"] == "function_call" && item["name"] != control_name)
        .cloned().collect()
}

pub fn text(output: &[Value]) -> String {
    output.iter().filter(|item| item["type"] == "message")
        .flat_map(|item| item["content"].as_array().into_iter().flatten())
        .filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("")
}

pub fn continuation(output: &[Value], control_name: &str) -> bool {
    (!output.is_empty() && output.iter().all(|item| item["type"] == "reasoning"))
        || output.iter().any(|item| item["name"] == control_name)
}

pub fn message_exceeds(output: &[Value], max_bytes: usize) -> bool {
    output.iter().filter(|item| item["type"] == "message")
        .any(|item| item["content"].as_array()
            .map(|parts| parts.iter().filter_map(|part| part["text"].as_str())
                .map(str::len).sum::<usize>() > max_bytes)
            .unwrap_or(false))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_calls_preserve_order_and_exclude_private_controls() {
        let output = vec![json!({"type":"function_call","name":"read","call_id":"a"}),
            json!({"type":"function_call","name":"context_assets"}),
            json!({"type":"message","name":"read"}),
            json!({"type":"function_call","name":"write","call_id":"b"})];
        assert_eq!(tool_calls(&output,"context_assets"),vec![output[0].clone(),output[3].clone()]);
    }

    #[test]
    fn text_joins_message_parts_and_ignores_nontext_content() {
        let output = vec![json!({"type":"reasoning","content":[{"text":"private"}]}),
            json!({"type":"message","content":[{"text":"Hello "},{"image":"x"},{"text":"world"},{"text":3}]}),
            json!({"type":"message","content":null}),
            json!({"type":"message","content":[{"text":"!"}]})];
        assert_eq!(text(&output),"Hello world!");
        assert_eq!(text(&[]),"");
    }

    #[test]
    fn continuation_preserves_reasoning_and_control_rules() {
        assert!(!continuation(&[],"context_assets"));
        assert!(continuation(&[json!({"type":"reasoning"})],"context_assets"));
        assert!(!continuation(&[json!({"type":"reasoning"}),json!({"type":"message"})],"context_assets"));
        assert!(continuation(&[json!({"type":"function_call","name":"context_assets"})],"context_assets"));
        assert!(continuation(&[json!({"type":"unknown","name":"context_assets"})],"context_assets"));
        assert!(!continuation(&[json!({"type":"function_call","name":"read"})],"context_assets"));
    }

    #[test]
    fn message_limit_counts_utf8_bytes_and_combines_parts_per_message() {
        let output = vec![json!({"type":"message","content":[{"text":"é"},{"text":"ab"}]})];
        assert!(!message_exceeds(&output,4));
        assert!(message_exceeds(&output,3));
        let boundary = vec![json!({"type":"message","content":[{"text":"a".repeat(8192)},{"text":"b".repeat(8192)}]})];
        assert!(!message_exceeds(&boundary,16384));
        assert!(message_exceeds(&boundary,16383));
    }

    #[test]
    fn message_limit_does_not_combine_messages_or_nonmessage_text() {
        let output = vec![json!({"type":"message","content":[{"text":"abcd"}]}),
            json!({"type":"message","content":[{"text":"efgh"}]}),
            json!({"type":"reasoning","content":[{"text":"private-long-text"}]}),
            json!({"type":"message","content":false})];
        assert!(!message_exceeds(&output,4));
        assert!(!message_exceeds(&[],0));
    }
}

/// Project only safe model identity fields for public run views.
pub fn public_run_model(run: &Value) -> Value {
    if run["model"].is_object(){return json!({"entry_id":run["model"]["entry_id"],"model":run["model"]["model"]});}
    let index=run["failover_state"]["candidate_index"].as_u64().unwrap_or(0) as usize;
    let candidate=run["selection_plan"]["candidates"].as_array().and_then(|items|items.get(index));
    match candidate {
        Some(value)=>json!({"entry_id":value["entry_id"],"model":value["upstream_model"]}),
        None=>json!({"entry_id":null,"model":run["provider_selection"]["model"]}),
    }
}
