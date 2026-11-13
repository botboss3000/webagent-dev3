//! Diagnostics as an ability plugin. It reads nothing itself: each call becomes one
//! declared host request (`diagnostics.*`, `review.*`) that the app answers for the call's
//! own session and run, checking administrator authority where the action needs it. The
//! app's finished tool result comes back as `{"tool_result": ...}` and reaches the agent
//! unchanged.
use serde_json::{json, Value};
use std::future::Future;

/// Tools this plugin runs.
pub const TOOLS: [&str; 2] = ["read_diagnostics", "agent_review"];

/// The app, as one plugin call sees it.
pub trait Host {
    /// Make the declared request `name` with `params` and a JSON `body`.
    fn request(
        &self,
        name: &str,
        params: Value,
        body: Value,
    ) -> impl Future<Output = Result<Value, String>> + Send;
}

/// One tool call and the facts the host supplies with it.
pub struct Call<'a> {
    pub tool: &'a str,
    pub arguments: &'a Value,
    /// Stable for this call and its retries.
    pub request_id: &'a str,
    pub now_ms: u64,
}

/// The host request one call makes: its name, params and body.
fn route(tool: &str, arguments: &Value) -> Result<(&'static str, Value, Value), String> {
    Ok(match tool {
        "read_diagnostics" if arguments["issues"] == true => {
            ("diagnostics.issues", Value::Null, arguments.clone())
        }
        "read_diagnostics" => (
            "diagnostics.progress",
            json!({"run_id": arguments.get("run_id").cloned().unwrap_or(Value::Null)}),
            Value::Null,
        ),
        "agent_review" => {
            let name = match arguments["action"].as_str() {
                Some("resolve") => "review.resolve",
                Some("open") => "review.open",
                _ => "review.note",
            };
            (name, Value::Null, arguments.clone())
        }
        _ => return Err("Unknown Diagnostics tool".into()),
    })
}

/// Run one tool call.
pub async fn call<H: Host + Sync>(host: &H, call: Call<'_>) -> Result<Value, String> {
    let (name, params, body) = route(call.tool, call.arguments)?;
    let result = host.request(name, params, body).await?;
    Ok(json!({"tool_result": result}))
}

/// Drives one call to completion; host requests answer synchronously over stdio.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = std::pin::pin!(future);
    let mut context = std::task::Context::from_waker(std::task::Waker::noop());
    loop {
        if let std::task::Poll::Ready(output) = future.as_mut().poll(&mut context) {
            return output;
        }
        std::thread::yield_now();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::sync::Mutex;

    /// Requests as `(name, params, body)`.
    type Seen = Vec<(String, Value, Value)>;

    /// Records each request and answers with a fixed result.
    struct Fake(Mutex<Seen>);

    impl Host for Fake {
        fn request(
            &self,
            name: &str,
            params: Value,
            body: Value,
        ) -> impl Future<Output = Result<Value, String>> + Send {
            self.0.lock().unwrap().push((name.to_owned(), params, body));
            std::future::ready(Ok(json!({"status":"denied","output":"x"})))
        }
    }

    fn run(tool: &str, arguments: Value) -> (Result<Value, String>, Seen) {
        let host = Fake(Mutex::new(Vec::new()));
        let result = block_on(call(
            &host,
            Call {
                tool,
                arguments: &arguments,
                request_id: "r",
                now_ms: 0,
            },
        ));
        (result, host.0.into_inner().unwrap())
    }

    #[test]
    fn each_call_is_one_request_and_relays_the_app_result() {
        let (result, seen) = run("read_diagnostics", json!({"run_id":"current"}));
        assert_eq!(
            result.unwrap(),
            json!({"tool_result":{"status":"denied","output":"x"}})
        );
        assert_eq!(
            seen,
            vec![(
                "diagnostics.progress".to_owned(),
                json!({"run_id":"current"}),
                Value::Null
            )]
        );
        let (_, seen) = run("read_diagnostics", json!({}));
        assert_eq!(seen[0].1, json!({"run_id":null}));
        let filter = json!({"issues":true,"scope":"all"});
        let (_, seen) = run("read_diagnostics", filter.clone());
        assert_eq!(seen[0].0, "diagnostics.issues");
        assert_eq!(seen[0].2, filter);
        for (action, name) in [
            (json!("resolve"), "review.resolve"),
            (json!("open"), "review.open"),
            (json!("note"), "review.note"),
            (Value::Null, "review.note"),
        ] {
            let arguments = json!({"action":action,"note":"n"});
            let (_, seen) = run("agent_review", arguments.clone());
            assert_eq!((seen[0].0.as_str(), &seen[0].2), (name, &arguments));
        }
    }

    #[test]
    fn unknown_tools_make_no_request() {
        let (result, seen) = run("file_read", json!({}));
        assert!(result.is_err());
        assert!(seen.is_empty());
    }
}
