//! A session's compaction checkpoint (the compacted history and the interaction cursor it
//! covers) lives in its own record, `session_checkpoint:{sid}`, so the session record that
//! every check reads stays small. The session record keeps `checkpoint_through`.
//! Sessions compacted before the move keep `checkpoint_input` in the session record until
//! their next compaction; readers use it when the separate record is absent or belongs to
//! another cursor (a reset writes the session's own fields and deletes the record).
use serde_json::Value;

pub fn key(sid: &str) -> String {
    format!("session_checkpoint:{sid}")
}

/// The checkpoint history for `session`: the separate `record` when it carries the
/// session's current cursor, else the legacy field in the session record.
pub fn input(session: &Value, record: Option<&Value>) -> Vec<Value> {
    let source = match record {
        Some(record) if record["checkpoint_through"] == session["checkpoint_through"] => record,
        _ => session,
    };
    source["checkpoint_input"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn record_is_used_only_for_the_current_cursor() {
        let record = json!({"checkpoint_input":[{"n":2}],"checkpoint_through":7});
        let moved = json!({"checkpoint_through":7});
        assert_eq!(input(&moved, Some(&record)), vec![json!({"n":2})]);
        // Reset after the move: the session's own fields win.
        let reset = json!({"checkpoint_input":[],"checkpoint_through":0});
        assert!(input(&reset, Some(&record)).is_empty());
        // Never moved: the legacy field.
        let legacy = json!({"checkpoint_input":[{"n":1}],"checkpoint_through":3});
        assert_eq!(input(&legacy, None), vec![json!({"n":1})]);
        assert!(input(&moved, None).is_empty());
    }
}
