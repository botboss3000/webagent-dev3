//! The app's side of one plugin call over JSON lines (protocol 2). The app writes the
//! call; the plugin may answer with numbered host requests, each answered in turn, and
//! ends with its result. The caller runs the process and bounds the whole call's time.
use serde_json::{json, Value};
use std::fmt;
use std::future::Future;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Protocol version this driver speaks.
pub const PROTOCOL: u64 = 2;
/// Largest line either side may send.
pub const MAX_LINE: usize = 2 * 1024 * 1024;
/// Host requests one call may make.
pub const MAX_REQUESTS: usize = 32;

/// Why a plugin call failed.
#[derive(Debug, Eq, PartialEq)]
pub enum PluginError {
    /// The plugin refused the call; its reason is shown to the model.
    Refused(String),
    /// The plugin broke the protocol, exceeded a bound or ended early.
    Protocol(&'static str),
}

impl fmt::Display for PluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(reason) => formatter.write_str(reason),
            Self::Protocol(reason) => write!(formatter, "Plugin protocol error: {reason}"),
        }
    }
}

impl std::error::Error for PluginError {}

/// One host request from the plugin.
pub struct Request {
    pub name: String,
    pub params: Value,
    pub body: Value,
}

/// Run one call: send `tool`, `arguments` and `context`, answer each host request with
/// `answer`, and return the plugin's output.
pub async fn drive<R, W, F, Fut>(
    mut reader: R,
    mut writer: W,
    tool: &str,
    arguments: &Value,
    context: &Value,
    mut answer: F,
) -> Result<Value, PluginError>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
    F: FnMut(Request) -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    let start =
        json!({"protocol": PROTOCOL, "tool": tool, "arguments": arguments, "context": context});
    send(&mut writer, &start).await?;
    let mut requests = 0;
    loop {
        let message = receive(&mut reader).await?;
        if let Some(result) = message.get("result") {
            return if result["ok"] == true {
                Ok(result["output"].clone())
            } else {
                let reason = result["error"]
                    .as_str()
                    .unwrap_or("Plugin refused the call");
                Err(PluginError::Refused(reason.chars().take(2000).collect()))
            };
        }
        let request = &message["request"];
        let (Some(id), Some(name)) = (request["id"].as_u64(), request["name"].as_str()) else {
            return Err(PluginError::Protocol("unexpected message"));
        };
        requests += 1;
        if requests > MAX_REQUESTS {
            return Err(PluginError::Protocol("too many host requests"));
        }
        let reply = answer(Request {
            name: name.to_owned(),
            params: request["params"].clone(),
            body: request["body"].clone(),
        })
        .await;
        let response = match reply {
            Ok(value) => json!({"response": {"id": id, "ok": true, "value": value}}),
            Err(error) => json!({"response": {"id": id, "ok": false, "error": error}}),
        };
        send(&mut writer, &response).await?;
    }
}

async fn send<W: AsyncWrite + Unpin>(writer: &mut W, message: &Value) -> Result<(), PluginError> {
    let mut line =
        serde_json::to_vec(message).map_err(|_| PluginError::Protocol("unencodable message"))?;
    if line.len() > MAX_LINE {
        return Err(PluginError::Protocol("message too large"));
    }
    line.push(b'\n');
    writer
        .write_all(&line)
        .await
        .map_err(|_| PluginError::Protocol("plugin input closed"))?;
    writer
        .flush()
        .await
        .map_err(|_| PluginError::Protocol("plugin input closed"))
}

async fn receive<R: AsyncBufRead + Unpin>(reader: &mut R) -> Result<Value, PluginError> {
    let mut line = Vec::new();
    let mut bounded = reader.take(MAX_LINE as u64 + 1);
    bounded
        .read_until(b'\n', &mut line)
        .await
        .map_err(|_| PluginError::Protocol("plugin output failed"))?;
    if line.is_empty() {
        return Err(PluginError::Protocol("plugin ended without a result"));
    }
    if line.len() > MAX_LINE {
        return Err(PluginError::Protocol("message too large"));
    }
    serde_json::from_slice(&line).map_err(|_| PluginError::Protocol("malformed message"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tokio::io::{duplex, BufReader};

    /// A scripted plugin: reads the call, sends `lines` (answering requests), returns what it read.
    async fn plugin(stream: tokio::io::DuplexStream, lines: Vec<Value>) -> Vec<Value> {
        let (read, mut write) = tokio::io::split(stream);
        let mut read = BufReader::new(read);
        let mut seen = vec![receive(&mut read).await.unwrap()];
        for line in lines {
            send(&mut write, &line).await.unwrap();
            if line.get("request").is_some() {
                // The app hangs up on a plugin it refuses.
                let Ok(reply) = receive(&mut read).await else {
                    break;
                };
                seen.push(reply);
            }
        }
        seen
    }

    async fn run(lines: Vec<Value>) -> (Result<Value, PluginError>, Vec<Value>, Vec<String>) {
        let (app, other) = duplex(64 * 1024);
        let script = tokio::spawn(plugin(other, lines));
        let (read, write) = tokio::io::split(app);
        let mut asked = Vec::new();
        let result = drive(
            BufReader::new(read),
            write,
            "get_agent",
            &json!({"agent_id":"a"}),
            &json!({"request_id":"r","now_ms":1}),
            |request: Request| {
                asked.push(request.name.clone());
                let reply = if request.name == "fail" {
                    Err("refused".to_owned())
                } else {
                    Ok(json!({"n":1}))
                };
                std::future::ready(reply)
            },
        )
        .await;
        (result, script.await.unwrap(), asked)
    }

    #[tokio::test]
    async fn requests_are_answered_in_turn_then_the_result_returns() {
        let (result, seen, asked) = run(vec![
            json!({"request":{"id":1,"name":"agents.list","params":{},"body":{}}}),
            json!({"request":{"id":2,"name":"fail","params":{},"body":{}}}),
            json!({"result":{"ok":true,"output":{"done":true}}}),
        ])
        .await;
        assert_eq!(result, Ok(json!({"done":true})));
        assert_eq!(asked, ["agents.list", "fail"]);
        assert_eq!(seen[0]["protocol"], PROTOCOL);
        assert_eq!(seen[0]["tool"], "get_agent");
        assert_eq!(
            seen[1],
            json!({"response":{"id":1,"ok":true,"value":{"n":1}}})
        );
        assert_eq!(
            seen[2],
            json!({"response":{"id":2,"ok":false,"error":"refused"}})
        );
    }

    #[tokio::test]
    async fn refusals_and_protocol_breaks_fail_the_call() {
        let (result, _, _) = run(vec![json!({"result":{"ok":false,"error":"Unknown tool"}})]).await;
        assert_eq!(result, Err(PluginError::Refused("Unknown tool".into())));
        let (result, _, _) = run(vec![json!({"hello":1})]).await;
        assert_eq!(result, Err(PluginError::Protocol("unexpected message")));
        let (result, _, _) = run(vec![]).await;
        assert_eq!(
            result,
            Err(PluginError::Protocol("plugin ended without a result"))
        );
        let many = (1..=MAX_REQUESTS as u64 + 1)
            .map(|id| json!({"request":{"id":id,"name":"agents.list"}}))
            .collect();
        let (result, _, asked) = run(many).await;
        assert_eq!(result, Err(PluginError::Protocol("too many host requests")));
        assert_eq!(asked.len(), MAX_REQUESTS);
    }
}
