//! Orchestration tests covering several tools and the shared turn and relation helpers.
use super::*;

#[test]
fn start_tools_run_through_messaging_and_queue_ids_are_route_safe() {
    for name in [
        "start_spawn",
        "start_session",
        "delegate_to_agent",
        "message_session",
    ] {
        assert!(TOOLS.contains(&name) && !owns(name));
    }
    assert!(TOOLS.iter().filter(|name| owns(name)).count() == 7);
    assert!(stop_session::valid_queue_id("queued-1_A"));
    assert!(!stop_session::valid_queue_id("a/b"));
    assert!(!stop_session::valid_queue_id(&"a".repeat(129)));
}

#[test]
fn turns_pair_tool_results_and_clip_items() {
    let long = "x".repeat(300);
    let items = vec![
        (
            1,
            "r".into(),
            json!({"type":"message","role":"user","content":"hi"}),
        ),
        (
            2,
            "r".into(),
            json!({"type":"function_call","call_id":"c","name":"file_read","arguments":long}),
        ),
        (
            3,
            "r".into(),
            json!({"type":"function_call_output","call_id":"c","output":"done"}),
        ),
        (4, "r".into(), json!({"type":"reasoning"})),
        (
            5,
            "r".into(),
            json!({"type":"message","role":"assistant","content":[{"text":"ok"}]}),
        ),
    ];
    let turns = turns(&items, read_session::BRIEF_CHARS);
    assert_eq!(turns.len(), 3);
    assert_eq!(turns[0]["kind"], "user");
    assert_eq!(turns[1]["result"], "done");
    assert!(turns[1]["arguments"].as_str().unwrap().ends_with("[cut]"));
    assert_eq!(
        turns[2],
        json!({"turn":3,"run_id":"r","kind":"agent","text":"ok"})
    );
}

#[test]
fn relation_names_spawns_and_started_sessions() {
    assert_eq!(
        relation("p", &json!({"parent_session_id":"p"})),
        Some("spawn")
    );
    assert_eq!(
        relation("p", &json!({"started_by_session_id":"p"})),
        Some("started_here")
    );
    assert_eq!(relation("p", &json!({"parent_session_id":"q"})), None);
}
