//! Root compatibility for the Engine-owned typed failure evidence.
pub(crate) use webagent_engine::tools::failure::*;
#[cfg(test)] mod tests {
 use super::*;
 use serde_json::json;
    #[tokio::test]
    async fn host_authority_failures_are_typed_before_any_socket_request() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let other = crate::runtime_tests::conversation(&app).await;
        let error = super::super::administrator::host_fingerprint(&app, &other, &rid, "build_work")
            .await
            .unwrap_err();
        let failure = json!(error.downcast_ref::<Failure>().unwrap());
        assert_eq!(failure["stage"], "authorization");
        assert_eq!(failure["code"], "scope_changed");
        assert_eq!(failure["delivery"], "not_sent");
        app.store
            .put("control", &json!({"shutoff": true}))
            .await
            .unwrap();
        let error = super::super::administrator::host_fingerprint(&app, &sid, &rid, "build_work")
            .await
            .unwrap_err();
        assert_eq!(
            json!(error.downcast_ref::<Failure>().unwrap())["code"],
            "authority_stopped"
        );
    }
    #[tokio::test]
    async fn failure_evidence_survives_durable_run_reopen_without_recorder() {
        let (root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let mut run = app.run(&rid).await.unwrap();
        assert!(app
            .store
            .claim_run(
                &mut run,
                "diagnostic-fixture-loop",
                "diagnostic-fixture-generation"
            )
            .await
            .unwrap());
        run["pending"] = json!({"id":"diagnostic-attempt", "name":"build_work", "call_id":"diagnostic-call", "state":"executing", "arguments":{"request_id":"durable-diagnostic-request", "secret":"SECRET_CANARY"}});
        let error: anyhow::Error = Failure::new(
            Stage::Authorization,
            Code::LegacyAuthorizationRejected,
            Delivery::NotSent,
        )
        .into();
        record(&mut run, &error, None);
        let expected = run["latest_tool_failure"].clone();
        run["status"] = json!("recovery");
        run["uncertain"] = json!(true);
        app.save(&mut run).await.unwrap();
        drop(app);
        let reopened = crate::Store::open(root.path()).await.unwrap();
        let saved = reopened.get(&format!("run:{rid}")).await.unwrap().unwrap();
        assert_eq!(saved["status"], "recovery");
        assert_eq!(saved["uncertain"], true);
        assert_eq!(saved["pending"]["failure"], expected);
        assert_eq!(crate::run_diagnostics(&saved)["tool_failure"], expected);
        assert!(!expected.to_string().contains("SECRET_CANARY"));
    }
}
