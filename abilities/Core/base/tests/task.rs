//! Task module tests covering the plan artifact and chat components.
use super::present_chat_component::{component, ComponentArgs};
use super::*;
#[test]
fn plan_artifact_holds_the_plan_and_refuses_bad_steps() {
    let plan: Plan = serde_json::from_value(json!({"title":"Standup note","goal":"Daily note",
        "checklist":[{"label":"Write it"},{"label":"Schedule it","status":"done"}]}))
    .unwrap();
    let card = plan_artifact(&plan).unwrap();
    assert_eq!(card["title"], "Standup note");
    assert_eq!(
        card["sections"][1]["value"],
        "1. [ ] Write it\n2. [x] Schedule it"
    );
    let short: Plan = serde_json::from_value(json!({"checklist":[{"label":"x"}]})).unwrap();
    assert!(plan_artifact(&short).is_err());
    assert!(serde_json::from_value::<Plan>(json!({"checklist":[],"expected_revision":0})).is_err());
}
#[test]
fn components_are_bounded_and_declarative() {
    assert!(serde_json::from_value::<ComponentArgs>(json!({"type":"status","expected_revision":0,
        "data":{"sections":[{"label":"Progress","value":"Ready"}]},"lifecycle":{"script":"alert(1)"}})).is_err());
    assert!(component(
        serde_json::from_value(json!({"type":"approval","expected_revision":0,
        "data":{"message":"Please approve","status":"approved"}}))
        .unwrap(),
        "c",
    )
    .is_err());
    let (_, choice, _) = component(
        serde_json::from_value(
            json!({"type":"choice","expected_revision":0,"title":"For Bob",
        "data":{"question":"Which?","options":[{"label":"A"},"B"]}}),
        )
        .unwrap(),
        "c",
    )
    .unwrap();
    assert_eq!(choice["authority"], "user_reply");
    assert_eq!(choice["data"]["prompt"], "Which?");
    assert_eq!(choice["data"]["options"][1]["label"], "B");
    let (_, closed, _) = component(
        serde_json::from_value(json!({"type":"choice","expected_revision":1,"dismiss":true}))
            .unwrap(),
        "c",
    )
    .unwrap();
    assert_eq!(closed["lifecycle"]["dismissed"], true);
    assert!(component(
        serde_json::from_value(json!({"type":"choice","expected_revision":0,
        "data":{"options":["A"]}}))
        .unwrap(),
        "c",
    )
    .is_err());
    let (_, form, _) = component(
        serde_json::from_value(
            json!({"type":"form","expected_revision":0,"title":"Options to combine",
        "data":{"fields":[{"name":"pick","type":"multi","options":["A","B","C"]}]}}),
        )
        .unwrap(),
        "c",
    )
    .unwrap();
    assert_eq!(form["title"], "Options to combine");
    assert_eq!(form["data"]["fields"][0]["options"][2], "C");
    assert!(component(
        serde_json::from_value(json!({"type":"form","expected_revision":0,"title":"T",
        "data":{"fields":[{"name":"pick","type":"multi","options":[]}]}}))
        .unwrap(),
        "c",
    )
    .is_err());
}
