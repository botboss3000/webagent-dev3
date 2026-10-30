//! Root-fixture tests of compacted recall; the tool lives in `webagent_native_tools::recall`.
#![cfg(test)]
use crate::App;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;
async fn recall_fixture() -> (tempfile::TempDir, App, String, String) {
    let (root, app) = crate::runtime_tests::fixture().await;
    let mut tier = app
        .store
        .entitlement_tier("operator")
        .await
        .unwrap()
        .unwrap();
    let revision = tier.revision;
    tier.ability_permissions
        .insert("base".into(), crate::access::Permission::Auto);
    app.store
        .save_entitlement_tier(tier, revision)
        .await
        .unwrap();
    let mut level = app
        .store
        .live_access("admin", "shared_default")
        .await
        .unwrap()
        .capability_level;
    level
        .ability_permissions
        .insert("base".into(), crate::access::Permission::Auto);
    app.store
        .save_capability_level(
            "shared_default",
            level,
            app.store
                .capability_levels("shared_default")
                .await
                .unwrap()
                .revision,
        )
        .await
        .unwrap();
    let sid = crate::runtime_tests::conversation(&app).await;
    crate::runtime_tests::activate_codebase(&app, &sid).await;
    crate::session_ability_set(
        crate::State(app.clone()),
        crate::Path((sid.clone(), "base".into())),
        crate::Json(crate::SessionAbilityBody {
            state: "active".into(),
            expected_revision: 1,
        }),
    )
    .await
    .unwrap();
    let rid = crate::runtime_tests::queued(&app, &sid).await;
    let mut run = app.run(&rid).await.unwrap();
    run["loaded_abilities"] = json!(["base", "codebase_admin"]);
    app.save(&mut run).await.unwrap();
    let source = vec![json!({"type":"message","role":"user","content":"remember me"})];
    let digest = crate::digest(&source);
    let entry =
        json!({"segment_id":"listed-1","source_digest":digest,"item_count":1,"created_at":1});
    let mut session = app
        .store
        .get(&format!("session:{sid}"))
        .await
        .unwrap()
        .unwrap();
    session["compaction_segments"] = json!([entry]);
    app.store
        .put(&format!("session:{sid}"), &session)
        .await
        .unwrap();
    app.store
        .put(
            &format!("compaction_segment:{sid}:listed-1"),
            &json!({
        "schema_version":1,"session_id":sid,"segment_id":"listed-1",
        "source_digest":digest,"source":source}),
        )
        .await
        .unwrap();
    (root, app, sid, rid)
}
async fn dispatch(app: &App, sid: &str, rid: &str, args: Value) -> Value {
    let call = crate::tools::native::Call {
        sid,
        run_id: rid,
        generation: "",
        pending_id: "",
    };
    crate::tools::native::recall::execute(
        &crate::tools::native_host::Host(app),
        call,
        "recall_compacted",
        &args,
        CancellationToken::new(),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn unlisted_segment_falls_back_to_file_result_page() {
    let (_root, app, sid, rid) = recall_fixture().await;
    let mut tx = app.store.pool.begin().await.unwrap();
    app.store
        .insert_interaction(
            &mut tx,
            &sid,
            &rid,
            "model:1",
            &json!({
        "type":"function_call","call_id":"read-call","name":"file_read","arguments":"{}"}),
        )
        .await
        .unwrap();
    app.store
        .insert_interaction(
            &mut tx,
            &sid,
            &rid,
            "tool:read-call",
            &json!({
        "type":"function_call_output","call_id":"read-call","output":"abcdef"}),
        )
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let result = dispatch(
        &app,
        &sid,
        &rid,
        json!({"segment_id":"not-listed",
        "tool_call_id":"read-call","offset_chars":2,"limit_chars":4,"limit_items":1}),
    )
    .await;
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(result["output"]["content"], "cdef");
    assert_eq!(result["output"]["source_kind"], "current_run_tool_result");
    let preferred = dispatch(
        &app,
        &sid,
        &rid,
        json!({"segment_id":"listed-1","tool_call_id":"read-call"}),
    )
    .await;
    assert_eq!(preferred["status"], "success", "{preferred}");
    assert_eq!(
        preferred["output"]["source_kind"],
        "compacted_provider_history"
    );
}
#[tokio::test]
async fn unmatched_selectors_return_listing_with_note() {
    let (_root, app, sid, rid) = recall_fixture().await;
    for args in [
        json!({"segment_id":"missing-segment"}),
        json!({"tool_call_id":"missing-call"}),
    ] {
        let result = dispatch(&app, &sid, &rid, args).await;
        assert_eq!(result["status"], "success", "{result}");
        assert_eq!(result["output"]["segments"][0]["segment_id"], "listed-1");
        assert_eq!(
            result["output"]["note"],
            "Supplied selectors matched nothing."
        );
    }
}
#[tokio::test]
async fn both_unmatched_selectors_return_listing_with_note() {
    let (_root, app, sid, rid) = recall_fixture().await;
    let result = dispatch(
        &app,
        &sid,
        &rid,
        json!({"segment_id":"missing-segment","tool_call_id":"missing-call"}),
    )
    .await;
    assert_eq!(result["status"], "success", "{result}");
    assert_eq!(
        result["output"]["source_kind"],
        "current_session_compaction_index"
    );
    assert_eq!(result["output"]["count"], 1);
    assert_eq!(
        result["output"]["note"],
        "Supplied selectors matched nothing."
    );
}
