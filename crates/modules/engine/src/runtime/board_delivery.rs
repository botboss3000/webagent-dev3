//! Blocker-board notes delivered into the in-app chat that owns their broker task.
//!
//! The board service lists open notes on app-owned tasks the app has not delivered yet;
//! recovery shows each in its owning chat as a background notice (which wakes an idle agent)
//! and marks it delivered with an outcome. A chat the user stopped keeps its notes on its stop
//! record for the next message instead of starting a run; for a stopped, inactive or deleted
//! chat the board answers the sender automatically. Notices are keyed by note, so a retry
//! after a failed mark never shows a note twice.
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Board-note wakes allowed per chat in one hour; later notes wait and go out together.
pub const WAKES_PER_HOUR: usize = 5;
/// Notes held on one stop record at most; the oldest drop first.
const MAX_HELD: usize = 20;

/// One request to the board service socket in the runtime folder.
pub async fn call(request: &Value) -> Result<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let path = webagent_configuration::paths::runtime()
        .join("board")
        .join("board.sock");
    let exchange = async {
        let mut stream = webagent_platform::ipc::connect(&path).await?;
        stream
            .write_all((request.to_string() + "\n").as_bytes())
            .await?;
        let mut line = String::new();
        BufReader::new(stream)
            .take(256 * 1024)
            .read_line(&mut line)
            .await?;
        anyhow::Ok(serde_json::from_str::<Value>(&line)?)
    };
    let reply = tokio::time::timeout(Duration::from_secs(5), exchange).await??;
    if reply["status"] != "success" {
        return Err(anyhow!(
            "board: {}",
            reply["error"].as_str().unwrap_or("refused")
        ));
    }
    Ok(reply["output"].clone())
}

/// Record one wake at `now` in a chat's `{"at":[..]}` record unless it already had
/// `WAKES_PER_HOUR` in the last hour. Returns whether the wake is allowed.
pub fn allow(wakes: &mut Value, now: u64) -> bool {
    let mut at: Vec<u64> = wakes["at"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_u64)
        .filter(|t| now.saturating_sub(*t) < 3600)
        .collect();
    let allowed = at.len() < WAKES_PER_HOUR;
    if allowed {
        at.push(now);
    }
    wakes["at"] = json!(at);
    allowed
}

/// Keep a note on a stopped chat's stop record; the next message's stop note lists it.
pub fn hold(record: &mut Value, note: &Value) {
    if !record["board_notes"].is_array() {
        record["board_notes"] = json!([]);
    }
    let held = record["board_notes"].as_array_mut().expect("array");
    held.push(summary(note));
    let extra = held.len().saturating_sub(MAX_HELD);
    held.drain(..extra);
}

/// The background notice for one note.
pub fn notice(note: &Value) -> Value {
    let author = note["author"].as_str().unwrap_or("someone");
    json!({"source":"board","title":format!("Board note from {author}"),"status":"note",
        "reply":note["text"],"note":summary(note)})
}

/// The fields a chat needs to read and answer a note.
fn summary(note: &Value) -> Value {
    json!({"id":note["id"],"to_task":note["to_task"],"from_task":note["from_task"],
        "author":note["author"],"session":note["session"],"verified":note["verified"] == true,
        "auto":note["auto"] == true,"text":note["text"]})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn five_wakes_an_hour_then_wait() {
        let mut wakes = json!({});
        for i in 0..WAKES_PER_HOUR as u64 {
            assert!(allow(&mut wakes, 1000 + i));
        }
        assert!(!allow(&mut wakes, 2000));
        assert!(allow(&mut wakes, 1000 + 3600));
    }

    #[test]
    fn held_notes_stay_bounded_and_notice_carries_the_note() {
        let mut record = json!({"delivered":false});
        for i in 0..MAX_HELD + 3 {
            hold(&mut record, &json!({"id":format!("n{i}"),"text":"x"}));
        }
        let held = record["board_notes"].as_array().unwrap();
        assert_eq!((held.len(), held[0]["id"].as_str()), (MAX_HELD, Some("n3")));
        let n = notice(&json!({"id":"a1","author":"t480/codex","text":"release?","verified":true}));
        assert_eq!(
            (n["source"].as_str(), n["note"]["verified"].as_bool()),
            (Some("board"), Some(true))
        );
    }
}
