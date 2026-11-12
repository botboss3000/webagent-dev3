//! The local inference service's client. `webagent-v3 --inference-service` runs the embedding
//! model (crates/modules/inference/src/service.rs); the loop warms it at start-up and it stays running
//! (about 130 MB) so ranking is always fast; a crashed child restarts on the next request. The configuration composer asks it for
//! setting matches ([`ask`]) and the abilities catalog for tool and skill scores ([`rank`]).
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use axum::http::StatusCode;

use crate::{bad, ApiError};

/// Loading the model and embedding the catalog takes about a second; a request about 10 ms.
const STARTUP: Duration = Duration::from_secs(30);
const ANSWER: Duration = Duration::from_secs(10);
/// A first `rank` embeds the whole tool or skill catalog (a few hundred texts); later ones reuse it.
const RANK_ANSWER: Duration = Duration::from_secs(30);
/// The start-up warm-up loads the model and embeds the whole catalog from cold.
const WARM_ANSWER: Duration = Duration::from_secs(120);

struct Matcher {
    child: Child,
    stdin: ChildStdin,
    replies: Lines<BufReader<ChildStdout>>,
    next: u64,
}

fn matcher_slot() -> &'static Mutex<Option<Matcher>> {
    static SLOT: OnceLock<Mutex<Option<Matcher>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

#[cfg(not(test))]
fn matcher_command() -> std::io::Result<Command> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--inference-service");
    Ok(command)
}
/// Test scorer for `rank` on this thread (tokio tests run on one thread); `None` uses the stub
/// service, which never returns scores.
#[cfg(test)]
pub(crate) type RankStub = fn(&str, &[Vec<String>]) -> Option<Vec<f32>>;
#[cfg(test)]
thread_local! {
    pub(crate) static RANK_STUB: std::cell::Cell<Option<RankStub>> = const { std::cell::Cell::new(None) };
}
/// Stands in for the service: answers every request with no match.
#[cfg(test)]
fn matcher_command() -> std::io::Result<Command> {
    let mut command = Command::new("/bin/sh");
    command.args([
        "-c",
        r#"echo '{"ready":true}'; while read -r line; do id=${line#*'"id":'}; echo "{\"id\":${id%%[,\}]*},\"result\":{\"outcome\":\"no_match\"}}"; done"#,
    ]);
    Ok(command)
}

/// One matcher answer. Requests are answered one at a time, in order.
pub(crate) async fn ask(request: Value) -> Result<Value, ApiError> {
    ask_within(request, ANSWER).await
}

/// Each item's closest-text score against `query` (the matcher's `rank` operation), or `None`
/// when the matcher is unavailable, so callers keep a fallback.
pub(crate) async fn rank(query: &str, items: Vec<Vec<String>>) -> Option<Vec<f32>> {
    #[cfg(test)]
    if let Some(stub) = RANK_STUB.with(std::cell::Cell::get) {
        return stub(query, &items);
    }
    let result = ask_within(
        json!({"op": "rank", "query": query, "items": items}),
        RANK_ANSWER,
    )
    .await
    .ok()?;
    result["scores"]
        .as_array()?
        .iter()
        .map(|score| score.as_f64().map(|score| score as f32))
        .collect()
}

/// Embed `items` once so later [`rank`] calls find the model loaded and the texts cached;
/// `false` when the service did not answer.
pub(crate) async fn warm(items: Vec<Vec<String>>) -> bool {
    #[cfg(test)]
    if let Some(stub) = RANK_STUB.with(std::cell::Cell::get) {
        return stub("warm-up", &items).is_some();
    }
    ask_within(
        json!({"op": "rank", "query": "warm-up", "items": items}),
        WARM_ANSWER,
    )
    .await
    .is_ok()
}

async fn ask_within(request: Value, within: Duration) -> Result<Value, ApiError> {
    let unavailable = || {
        ApiError(
            StatusCode::SERVICE_UNAVAILABLE,
            "Local inference service unavailable".into(),
        )
    };
    let mut slot = matcher_slot().lock().await;
    if !slot
        .as_mut()
        .is_some_and(|m| matches!(m.child.try_wait(), Ok(None)))
    {
        *slot = Some(start().await.map_err(|_| unavailable())?);
    }
    let matcher = slot.as_mut().ok_or_else(unavailable)?;
    matcher.next += 1;
    let id = matcher.next;
    let mut request = request;
    request["id"] = json!(id);
    let answer = async {
        matcher
            .stdin
            .write_all(format!("{request}\n").as_bytes())
            .await
            .ok()?;
        loop {
            let reply: Value =
                serde_json::from_str(&matcher.replies.next_line().await.ok()??).ok()?;
            if reply["id"] == id {
                return Some(reply);
            }
        }
    };
    let reply = tokio::time::timeout(within, answer).await.ok().flatten();
    let Some(reply) = reply else {
        // A lost or late reply leaves the stream out of step: start over next time.
        *slot = None;
        return Err(unavailable());
    };
    match reply.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(bad(reply["error"]
            .as_str()
            .unwrap_or("Request not understood"))),
    }
}

async fn start() -> std::io::Result<Matcher> {
    let mut child = matcher_command()?
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let missing = || std::io::Error::other("inference service pipes unavailable");
    let stdin = child.stdin.take().ok_or_else(missing)?;
    let mut replies = BufReader::new(child.stdout.take().ok_or_else(missing)?).lines();
    let ready = tokio::time::timeout(STARTUP, replies.next_line()).await;
    let ready: Value = match ready {
        Ok(Ok(Some(line))) => serde_json::from_str(&line).unwrap_or_default(),
        _ => Value::Null,
    };
    if ready["ready"] != true {
        return Err(std::io::Error::other("inference service did not start"));
    }
    Ok(Matcher {
        child,
        stdin,
        replies,
        next: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn matcher_answers_through_the_service() {
        let answer = ask(json!({"text": "x", "page": "app"}))
            .await
            .map_err(|e| e.1);
        assert_eq!(answer, Ok(json!({"outcome": "no_match"})));
    }
}
