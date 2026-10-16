//! Exercise the actual confined capability executable, not only library mocks.
// @code-index {"version":1,"feature":"capability-stdio-verification","aliases":["live context","source correspondence"],"purpose":"Exercise the actual capability executable with bounded private frames, live replacement and rejection. Protocol assertions are fixed verification invariants.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Fixtures assert the fixed private-frame, version, EOF and source-correspondence contract. User settings must not disable these proofs or permit malformed service frames."}}
#![cfg(all(target_os = "linux", target_arch = "x86_64"))]
use serde_json::{json, Value};
use std::io::Write;
use std::process::{Command, Output, Stdio};
use webagent_capability_contracts::{Guidance, GuidanceKind, PrepareRequest, PreparedCapability};

/// Close stdin after bounded fixture frames so the service must terminate at EOF.
fn invoke(input: &[u8]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_webagent-capability-service"))
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("start real capability executable");
    // Rejected frames may close the pipe early; exit status is the contract below.
    let _ = child.stdin.take().expect("piped stdin").write_all(input);
    child.wait_with_output().expect("collect capability executable result")
}

/// The wrapper's operation name is prepare; capability.prepare is its registry ID.
fn frame(id: &str, request: &PrepareRequest) -> Value {
    json!({"version":1,"id":id,"run_key":"capability-stdio-fixture",
        "operation":"prepare","input":request})
}

/// Represent one fully resolved, authorized host snapshot without ambient access.
fn fixture() -> PrepareRequest {
    PrepareRequest {
        version: 1,
        source_fingerprint: "a".repeat(64),
        base_prompt: "FIRST LIVE MAIN\n".into(),
        live_slots: "FIRST LIVE SLOT\n".into(),
        guidance: vec![Guidance {
            id: "selectable:codebase_admin:code_verification".into(),
            kind: GuidanceKind::Selectable,
            body: "FIRST LIVE GUIDANCE".into(),
            selected: true,
            natural: false,
        }],
        permitted_schemas: vec!["file_read".into(), "file_write".into()],
        prior_schema_order: vec!["file_write".into()],
        menu: "FIRST TEMPORARY MENU".into(),
    }
}

/// Prove service equivalence and live replacement without restarting its process.
#[test]
fn executable_matches_library_for_two_fresh_inputs_on_one_pipe() {
    let first = fixture();
    let mut second = fixture();
    second.source_fingerprint = "b".repeat(64);
    second.base_prompt = "CURRENT LIVE MAIN\n".into();
    second.live_slots = "CURRENT LIVE SLOT\n".into();
    second.guidance[0].body = "CURRENT LIVE GUIDANCE".into();
    second.menu = "CURRENT TEMPORARY MENU".into();
    second.permitted_schemas = vec!["file_read".into()];
    second.prior_schema_order = vec!["file_write".into(), "file_read".into()];
    let mut wire = serde_json::to_vec(&frame("first", &first)).unwrap();
    wire.push(b'\n');
    wire.extend(serde_json::to_vec(&frame("second", &second)).unwrap());
    wire.push(b'\n');
    let output = invoke(&wire);
    assert!(output.status.success(), "capability service failed: {:?}", output.status);
    assert!(output.stderr.is_empty());
    let replies = output.stdout.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap()).collect::<Vec<_>>();
    assert_eq!(replies.len(), 2);
    for (reply, (id, request)) in replies.iter().zip([("first", &first), ("second", &second)]) {
        assert_eq!(reply["version"], 1);
        assert_eq!(reply["id"], id);
        assert_eq!(reply["result"]["status"], "success");
        assert_eq!(reply["result"]["implementation_version"], webagent_capability_context::IMPLEMENTATION_VERSION);
        let capability: PreparedCapability = serde_json::from_value(reply["result"]["capability"].clone()).unwrap();
        webagent_capability_contracts::validate_result(request, &capability).unwrap();
        let expected = webagent_capability_context::prepare(request).unwrap();
        assert_eq!(reply["result"]["capability"], serde_json::to_value(expected).unwrap());
    }
    let current = &replies[1]["result"]["capability"];
    assert_eq!(current["main"], "CURRENT LIVE MAIN\nCURRENT LIVE SLOT\n");
    assert_eq!(current["definitions"][0]["body"], "CURRENT LIVE GUIDANCE");
    assert_eq!(current["schema_order"], json!(["file_read"]));
    assert_eq!(current["temporary_menu"], "CURRENT TEMPORARY MENU");
    assert!(!current.to_string().contains("FIRST LIVE"));
    assert!(!current["main"].as_str().unwrap().contains("MENU"));
}

/// Reject invalid transport and contract input before publishing any capability.
#[test]
fn executable_rejects_malformed_frames_and_invalid_capability_inputs() {
    let valid = frame("invalid-fixture", &fixture());
    let mut rejected = vec![b"not json\n".to_vec(), serde_json::to_vec(&valid).unwrap()];
    for (key, value) in [("version", json!(2)), ("operation", json!("unknown")),
        ("id", json!("")), ("run_key", json!("")), ("unknown", json!(true))] {
        let mut invalid = valid.clone();
        invalid[key] = value;
        let mut wire = serde_json::to_vec(&invalid).unwrap();
        wire.push(b'\n'); rejected.push(wire);
    }
    for (key, value) in [("base_prompt", json!("")), ("source_fingerprint", json!("invalid")),
        ("unknown", json!(true)), ("permitted_schemas", json!(["file_read", "file_read"]))] {
        let mut invalid = valid.clone();
        invalid["input"][key] = value;
        let mut wire = serde_json::to_vec(&invalid).unwrap();
        wire.push(b'\n'); rejected.push(wire);
    }
    // The wrapper reads at most its frame limit plus one byte, then rejects.
    rejected.push(vec![b'x'; 16 * 1024 * 1024 + 1]);
    for wire in rejected {
        let output = invoke(&wire);
        assert!(!output.status.success(), "malformed frame was accepted");
        assert!(output.stdout.is_empty(), "rejected input must publish no capability");
    }
}
