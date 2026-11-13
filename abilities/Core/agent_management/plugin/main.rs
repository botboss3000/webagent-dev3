//! Agent Management plugin process for dev builds. One call per process, as JSON lines:
//! the app writes `{"protocol":2,"tool","arguments","context":{"request_id","now_ms"}}`;
//! the plugin may write `{"request":{"id","name","params","body"}}` and reads
//! `{"response":{"id","ok","value"|"error"}}` for each; it ends with
//! `{"result":{"ok","output"|"error"}}`.
use serde_json::{json, Value};
use std::future::Future;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use webagent_agent_management::{block_on, call, Call, Host};

/// Requests go out on stdout and their responses come back on stdin, one at a time.
struct Stdio {
    next: AtomicU64,
}

impl Stdio {
    fn exchange(&self, name: &str, params: Value, body: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        write(&json!({"request": {"id": id, "name": name, "params": params, "body": body}}))?;
        let reply = read()?;
        let response = &reply["response"];
        if response["id"] != id {
            return Err("Host response out of order".into());
        }
        if response["ok"] == true {
            Ok(response["value"].clone())
        } else {
            Err(response["error"].as_str().unwrap_or("Host request refused").to_owned())
        }
    }
}

impl Host for Stdio {
    fn request(&self, name: &str, params: Value, body: Value) -> impl Future<Output = Result<Value, String>> + Send {
        std::future::ready(self.exchange(name, params, body))
    }
}

fn read() -> Result<Value, String> {
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|_| "Host closed the call".to_owned())?;
    serde_json::from_str(&line).map_err(|_| "Malformed host message".to_owned())
}

fn write(message: &Value) -> Result<(), String> {
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, message).map_err(|_| "Output failed".to_owned())?;
    out.write_all(b"\n").and_then(|()| out.flush()).map_err(|_| "Output failed".to_owned())
}

fn main() {
    let result = read().and_then(|start| {
        if start["protocol"] != 2 {
            return Err("Unsupported plugin protocol".into());
        }
        let tool = start["tool"].as_str().ok_or("Missing tool")?;
        let request_id = start["context"]["request_id"].as_str().ok_or("Missing request id")?;
        let now_ms = start["context"]["now_ms"].as_u64().ok_or("Missing time")?;
        let host = Stdio { next: AtomicU64::new(0) };
        block_on(call(&host, Call { tool, arguments: &start["arguments"], request_id, now_ms }))
    });
    let message = match result {
        Ok(output) => json!({"result": {"ok": true, "output": output}}),
        Err(error) => json!({"result": {"ok": false, "error": error}}),
    };
    if write(&message).is_err() {
        std::process::exit(1);
    }
}
