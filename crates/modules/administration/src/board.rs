//! The blocker board service as the app reaches it directly: in-app `build_work` note,
//! notes and note_remove normally go through the broker, which vouches for the task; when the
//! broker cannot be reached they go straight to the board, which knows the app by its Linux
//! user and records the signed-in user as the author (the task is then unverified).
use serde_json::{json, Value};
use std::path::PathBuf;

/// The board's socket, in the runtime folder the app container mounts.
pub fn socket() -> PathBuf {
    webagent_configuration::paths::runtime()
        .join("board")
        .join("board.sock")
}

/// The board request for one `build_work` note action from `user`, or None for any other action.
/// `broker_id` is the caller's own task; notes default to it, as they do through the broker.
pub fn request(args: &Value, user: &str) -> Option<Value> {
    let own = args["broker_id"]
        .as_str()
        .or_else(|| args["task_id"].as_str());
    let target = args["target_task_id"].as_str().or(own);
    let mut request = match args["action"].as_str()? {
        "note" => json!({"action":"post","to_task":target?,"text":args["reason"]}),
        "notes" => json!({"action":"list","task":target?,"notices":true,
            "archived":args["archived"].as_bool().unwrap_or(false)}),
        "note_remove" => json!({"action":"remove","id":args["note_id"],"kind":args["kind"]}),
        _ => return None,
    };
    if let Some(own) = own {
        if args["action"] == "note" {
            request["from_task"] = json!(own);
        }
    }
    if args["action"] == "note_remove" && args["reason"].is_string() {
        request["reason"] = args["reason"].clone();
    }
    request["user"] = json!(user);
    Some(request)
}

#[cfg(test)]
mod tests {
    use super::request;
    use serde_json::json;

    #[test]
    fn note_actions_map_to_board_requests() {
        let note = request(&json!({"action":"note","broker_id":"mine-task","target_task_id":"their-task","reason":"hi"}), "admin").unwrap();
        assert_eq!(
            note,
            json!({"action":"post","to_task":"their-task","from_task":"mine-task","text":"hi","user":"admin"})
        );
        let own = request(&json!({"action":"notes","broker_id":"mine-task"}), "admin").unwrap();
        assert_eq!(
            own,
            json!({"action":"list","task":"mine-task","notices":true,"archived":false,"user":"admin"})
        );
        let remove = request(
            &json!({"action":"note_remove","note_id":"abc123","kind":"clear","reason":"done"}),
            "admin",
        )
        .unwrap();
        assert_eq!(
            remove,
            json!({"action":"remove","id":"abc123","kind":"clear","reason":"done","user":"admin"})
        );
        assert!(request(&json!({"action":"finish","broker_id":"mine-task"}), "admin").is_none());
        assert!(request(&json!({"action":"notes"}), "admin").is_none());
    }
}
