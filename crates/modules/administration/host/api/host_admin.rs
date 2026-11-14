//! Host Admin: the administrator's side of the root host helper. The helper (a root service
//! outside the app) holds host-action requests until an administrator decides them here; it
//! trusts decisions only from the app's own uid, and this adapter only forwards signed-in,
//! CSRF-checked administrator decisions. Routes sit under the administrator router.
use axum::extract::Path;
use axum::{Extension, Json};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::{bad, Api, App, State};

/// Largest helper reply read back.
const MAX_REPLY: usize = 2 * 1024 * 1024;

/// One request to the helper's peer-checked socket under the runtime root.
async fn helper(message: Value) -> Result<Value, crate::ApiError> {
    let path = webagent_configuration::paths::runtime().join("host/helper.sock");
    let unavailable = |_| bad("Host helper unavailable");
    let mut stream = UnixStream::connect(&path).await.map_err(unavailable)?;
    let mut line = serde_json::to_vec(&message).map_err(|_| bad("Invalid host request"))?;
    line.push(b'\n');
    stream.write_all(&line).await.map_err(unavailable)?;
    let mut reply = Vec::new();
    let mut reader = BufReader::new(stream.take(MAX_REPLY as u64));
    reader
        .read_until(b'\n', &mut reply)
        .await
        .map_err(unavailable)?;
    let value: Value =
        serde_json::from_slice(&reply).map_err(|_| bad("Host helper reply unreadable"))?;
    if value["status"] != "success" {
        return Err(bad(value["output"]
            .as_str()
            .unwrap_or("Host helper refused the request")));
    }
    Ok(value["output"].clone())
}

/// Pending requests (with their exact effect and undo class) and the recent decided ones.
pub(crate) async fn requests(State(_app): State<App>) -> Api {
    Ok(Json(helper(json!({"op": "list"})).await?))
}

/// Approve or deny one request: `{"decision": "approve" | "deny"}`. The helper records the
/// deciding administrator and runs an approved action itself.
pub(crate) async fn decide(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let decision = body["decision"].as_str().unwrap_or_default();
    if !matches!(decision, "approve" | "deny") {
        return Err(bad("Decision must be approve or deny"));
    }
    let admin = identity["_user_id"].as_str().unwrap_or_default();
    Ok(Json(
        helper(json!({"op": "decide", "id": id, "decision": decision, "admin": admin})).await?,
    ))
}

/// Open (or end, with 0 seconds) an allow-all window for one requester: `{"requester":
/// "<session or ssh:identity>", "seconds": n}`. The helper caps it and never lets it cover
/// boot, disk or irreversible actions.
pub(crate) async fn allow(
    State(_app): State<App>,
    Extension(identity): Extension<Value>,
    Json(body): Json<Value>,
) -> Api {
    let requester = body["requester"].as_str().unwrap_or_default();
    let seconds = body["seconds"]
        .as_i64()
        .ok_or_else(|| bad("Seconds required"))?;
    if requester.is_empty() || requester.len() > 200 {
        return Err(bad("Requester required"));
    }
    let admin = identity["_user_id"].as_str().unwrap_or_default();
    Ok(Json(
        helper(json!({"op": "allow", "session": requester, "seconds": seconds, "admin": admin}))
            .await?,
    ))
}
