//! Actual journal and live authority remain scoped by the Engine consumer.
use crate::*;
pub async fn restore(app:&App,sid:&str,agent:&str,run:&mut Value,policy:&tool_output::Policy)->Result<bool> {
 app.engine().restore_retained(sid,agent,run,policy).await
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn policy() -> tool_output::Policy {
        tool_output::Policy {
            enabled: true,
            scope: tool_output::Scope::Selected,
            tools: vec!["file_read".into()],
            promoted_chars: 1_024,
            ..tool_output::Policy::default()
        }
    }

    async fn allow_file_read(app: &App, sid: &str) {
        app.store
            .put(
                "policy",
                &json!({"version":1,"rules":{"file_read":"allow"}}),
            )
            .await
            .unwrap();
        crate::runtime_tests::activate_codebase(app, sid).await;
    }

    async fn journal_source(app: &App, sid: &str, agent: &str, call_id: &str, raw: &str) -> String {
        let run_id = crate::runtime_tests::queued(app, sid).await;
        let mut source = app.run(&run_id).await.unwrap();
        source["agent_id"] = json!(agent);
        let call = json!({"type":"function_call","call_id":call_id,
            "name":"file_read","arguments":"{}"});
        app.store
            .save_run(&mut source, "model_output", &json!({"output":[call]}))
            .await
            .unwrap();
        source["input"].as_array_mut().unwrap().push(json!({
            "type":"function_call_output","call_id":call_id,"output":raw
        }));
        app.store
            .save_run(
                &mut source,
                "tool_result",
                &json!({"run_id":run_id,
            "name":"file_read","result":{"status":"success","output":raw}}),
            )
            .await
            .unwrap();
        run_id
    }

    fn recalled(run_id: &str, call_id: &str, raw: &str, hash: Option<&str>) -> Value {
        let digest = hex::encode(Sha256::digest(raw.as_bytes()));
        json!({"input":[],"loaded_abilities":["codebase_admin"],
        "tool_context_state":{"entries":[{
            "call_id":call_id,"origin_run_id":run_id,"tool_name":"file_read",
            "output_sha256":hash.unwrap_or(&digest),"promoted":true,"last_used":1
        }]}})
    }

    #[tokio::test]
    async fn disabled_policy_does_not_restore_a_promoted_entry() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let mut run = json!({"input":[],"tool_context_state":{"entries":[{
            "call_id":"old","origin_run_id":"missing","tool_name":"file_read","promoted":true
        }]}});
        assert!(!restore(
            &app,
            &sid,
            "shared_default",
            &mut run,
            &tool_output::Policy::default()
        )
        .await
        .unwrap());
        assert!(run["input"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn matching_promoted_journal_restores_only_when_permission_is_auto() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        allow_file_read(&app, &sid).await;
        let raw = "matching indexed source";
        let call_id = "promoted-read";
        let origin = journal_source(&app, &sid, "shared_default", call_id, raw).await;

        let mut matching = recalled(&origin, call_id, raw, None);
        assert!(
            restore(&app, &sid, "shared_default", &mut matching, &policy())
                .await
                .unwrap()
        );
        assert_eq!(matching["input"].as_array().unwrap().len(), 2);
        assert_eq!(matching["input"][0]["call_id"], call_id);
        assert_eq!(matching["input"][1]["output"], raw);

        for permission in [access::Permission::Ask, access::Permission::Deny] {
            let current = app
                .store
                .agent_tool_permissions("shared_default")
                .await
                .unwrap();
            app.store
                .set_agent_tool_permissions(
                    "shared_default",
                    current["revision"].as_u64().unwrap(),
                    [("file_read".to_owned(), permission)].into(),
                )
                .await
                .unwrap();
            let mut blocked = recalled(&origin, call_id, raw, None);
            assert!(
                !restore(&app, &sid, "shared_default", &mut blocked, &policy())
                    .await
                    .unwrap()
            );
            assert!(blocked["input"].as_array().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn promoted_journal_from_another_session_or_agent_is_not_restored() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let foreign_sid = crate::runtime_tests::conversation(&app).await;
        allow_file_read(&app, &sid).await;
        let raw = "foreign source";
        let foreign_session_call = "foreign-session-read";
        let foreign_session_origin = journal_source(
            &app,
            &foreign_sid,
            "shared_default",
            foreign_session_call,
            raw,
        )
        .await;
        let mut foreign_session =
            recalled(&foreign_session_origin, foreign_session_call, raw, None);
        assert!(!restore(
            &app,
            &sid,
            "shared_default",
            &mut foreign_session,
            &policy()
        )
        .await
        .unwrap());
        assert!(foreign_session["input"].as_array().unwrap().is_empty());

        let foreign_agent_call = "foreign-agent-read";
        let foreign_agent_origin =
            journal_source(&app, &sid, "another_agent", foreign_agent_call, raw).await;
        let mut foreign_agent = recalled(&foreign_agent_origin, foreign_agent_call, raw, None);
        assert!(
            !restore(&app, &sid, "shared_default", &mut foreign_agent, &policy())
                .await
                .unwrap()
        );
        assert!(foreign_agent["input"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn promoted_journal_hash_mismatch_and_budget_excess_are_not_restored() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        allow_file_read(&app, &sid).await;
        let call_id = "budget-read";
        let raw = "x".repeat(1_025);
        let origin = journal_source(&app, &sid, "shared_default", call_id, &raw).await;

        let mut mismatch = recalled(&origin, call_id, &raw, Some(&"0".repeat(64)));
        assert!(
            !restore(&app, &sid, "shared_default", &mut mismatch, &policy())
                .await
                .unwrap()
        );
        assert!(mismatch["input"].as_array().unwrap().is_empty());

        let mut over_budget = recalled(&origin, call_id, &raw, None);
        assert!(
            !restore(&app, &sid, "shared_default", &mut over_budget, &policy())
                .await
                .unwrap()
        );
        assert!(over_budget["input"].as_array().unwrap().is_empty());
    }
}
