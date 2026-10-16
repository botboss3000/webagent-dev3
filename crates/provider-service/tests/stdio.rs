//! Exercise the actual isolated private-pipe executable against the linked planner.
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Actual provider-service subprocess tests compare typed library results and reject malformed routing input without a listener connection.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Reference coverage, provider identity, replay rules and transport authority are fixed protocol invariants."}}
use serde_json::{json, Value};
use std::io::{ErrorKind, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use webagent_provider_contracts::{
    materialize, ProviderIdV1, RequestV1, ResultV1, CONTRACT_VERSION,
};

/// Clear ambient provider configuration and close stdin so the real service must terminate.
fn invoke(frames: &[Value]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_webagent-provider-service"))
        .env_clear()
        .current_dir("/")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn isolated provider service");
    {
        let mut input = child.stdin.take().unwrap();
        for frame in frames {
            serde_json::to_writer(&mut input, frame).unwrap();
            input.write_all(b"\n").unwrap();
        }
    }
    child.wait_with_output().unwrap()
}

/// Wrap a typed request in the engine service's fixed versioned prepare envelope.
fn frame(id: usize, request: Value) -> Value {
    json!({"version":1,"id":format!("fixture-{id}"),"run_key":"fixture-run",
        "operation":"prepare","input":request})
}

/// Compare actual isolated pipe replies and reconstruction with all three transport families' linked plans.
#[test]
fn actual_stdio_profile_and_projection_equal_linked_implementation() {
    let mut requests = Vec::new();
    for provider in [
        ProviderIdV1::OpenaiSubscription,
        ProviderIdV1::Deepseek,
        ProviderIdV1::Openrouter,
    ] {
        requests.push(RequestV1::Profile {
            version: CONTRACT_VERSION, provider, model: "fixture".into(),
        });
        requests.push(RequestV1::Project {
            version: CONTRACT_VERSION, provider, model: "fixture".into(),
            instructions: "Live base instructions\nwith exact spacing. ".into(),
            input: vec![
                json!({"role":"user","content":[
                    {"type":"input_text","text":"Inspect image"},
                    {"type":"input_image","image_url":"data:image/png;base64,AAAA"}
                ]}),
                json!({"type":"message","role":"assistant",
                    "content":[{"type":"output_text","text":"Checking"}],
                    "deepseek_reasoning":"retained reasoning"}),
                json!({"type":"function_call","call_id":"call-1","name":"inspect",
                    "arguments":"{ \"path\": \"a\" }","deepseek_reasoning":"retained reasoning"}),
                json!({"type":"function_call_output","call_id":"call-1",
                    "output":"exact result\n  spacing"}),
                json!({"role":"developer","content":"Later live guidance"}),
            ],
            tools: vec![json!({"type":"function","name":"inspect",
                "description":"Inspect a fixture","parameters":{"type":"object",
                "properties":{"path":{"type":"string"}},"required":["path"]}})],
        });
    }
    let frames = requests.iter().enumerate()
        .map(|(id, request)| frame(id, serde_json::to_value(request).unwrap()))
        .collect::<Vec<_>>();
    let output = invoke(&frames);
    assert!(output.status.success(), "isolated service must accept valid frames");
    assert!(output.stderr.is_empty());
    let replies = output.stdout.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(replies.len(), requests.len());
    for (id, (request, reply)) in requests.iter().zip(replies).enumerate() {
        assert_eq!(reply["version"], 1);
        assert_eq!(reply["id"], format!("fixture-{id}"));
        assert_eq!(reply["result"]["status"], "success");
        assert_eq!(reply["result"]["implementation_version"],
            webagent_provider_projection::IMPLEMENTATION_VERSION);
        let actual: ResultV1 = serde_json::from_value(reply["result"]["projection"].clone()).unwrap();
        let expected = webagent_provider_projection::project(request).unwrap();
        assert_eq!(actual, expected);
        if matches!(request, RequestV1::Project { .. }) {
            // Reference equality must also reconstruct identical exact host-owned content.
            assert_eq!(materialize(request, &actual).unwrap(),
                materialize(request, &expected).unwrap());
        }
    }
}

/// Reject invalid envelopes and source shapes; a parent-owned listener detects forbidden routing effects.
#[test]
fn malformed_invocations_cannot_acquire_endpoint_or_header_authority() {
    // The listener is owned by this test, never supplied as a granted service capability.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let valid = json!({"kind":"profile","version":1,
        "provider":"openai_subscription","model":"fixture"});
    let mut injected = valid.clone();
    injected["endpoint"] = json!(format!("http://{address}/must-not-connect"));
    injected["headers"] = json!({"authorization":"fixture-only"});
    let malformed = [
        frame(0, injected),
        json!({"version":2,"id":"fixture","run_key":"fixture","input":valid}),
        json!({"version":1,"id":"fixture","run_key":"fixture","operation":"dispatch",
            "input":{"kind":"profile","version":1,"provider":"deepseek","model":"fixture"}}),
        frame(0, json!({"kind":"project","version":1,"provider":"deepseek","model":"fixture",
            "instructions":"fixture","input":[{"type":"function_call","call_id":"c",
            "name":"inspect","arguments":{}}],"tools":[]})),
    ];
    for invocation in malformed {
        let output = invoke(&[invocation]);
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "no success response for malformed invocation");
        assert!(output.stderr.is_empty(), "service errors must not expose private input");
        assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock,
            "rejected routing fields must never cause ambient network activity");
    }
}
