//! Recheck a native Core invocation against its persisted run and current caller policy.
use crate::*;
pub async fn check(
    app: &App,
    sid: &str,
    rid: &str,
    generation: &str,
    pending_id: &str,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<Value> {
    app.engine()
        .authorize_native(sid, rid, generation, pending_id, name, args, cancel)
        .await
}

/// A wrapper cannot use another ability's implementation to bypass its live tool denial.
pub async fn dependency(app: &App, sid: &str, run: &Value, name: &str) -> Result<()> {
    app.engine().authorize_dependency(sid, run, name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_view_request_rechecks_arguments_cancellation_and_live_scope() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let mut tier = app
            .store
            .entitlement_tier("operator")
            .await
            .unwrap()
            .unwrap();
        tier.ability_permissions
            .insert("app_control".into(), access::Permission::Auto);
        let revision = tier.revision;
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
            .insert("app_control".into(), access::Permission::Auto);
        app.store
            .save_capability_level("shared_default", level, app.store.capability_levels("shared_default").await.unwrap().revision)
            .await
            .unwrap();
        let grants = app
            .store
            .agent_ability_permissions("shared_default")
            .await
            .unwrap();
        let mut permissions: std::collections::BTreeMap<String, access::Permission> =
            serde_json::from_value(grants["ability_permissions"].clone()).unwrap();
        permissions.insert("app_control".into(), access::Permission::Auto);
        app.store
            .set_agent_ability_permissions(
                "shared_default",
                grants["revision"].as_u64().unwrap(),
                permissions,
            )
            .await
            .unwrap();
        let mut policy = policy_default();
        policy["rules"]["set_app_view"] = json!("allow");
        app.store.put("policy", &policy).await.unwrap();
        let sid = crate::runtime_tests::conversation(&app).await;
        // Session creation defaults to Ask; this write test explicitly selects a write-capable fixture mode.
        app.store
            .put(
                &format!("session_mode:{sid}"),
                &json!({"mode":"write","revision":1}),
            )
            .await
            .unwrap();
        app.store
            .put(
                &format!("session_ability_state:{sid}"),
                &json!({"revision":1,"active":["base","app_control"],"suppressed":[]}),
            )
            .await
            .unwrap();
        let rid = uuid();
        let pending = uuid();
        let args = json!({"view":"files"});
        let mut run = json!({"id":rid,"session_id":sid,"agent_id":"shared_default","status":"running","generation":"fixture","loaded_abilities":["app_control"],"pending":{"id":pending,"name":"set_app_view","state":"executing","arguments":args,"human_approved":true}});
        app.store.put(&key("run", &rid), &run).await.unwrap();
        let (policy, _, visibility, state) = ability_controls(&app, &sid).await.unwrap();
        let access = shared_chat::live_access(&app.store, "admin", &sid, "shared_default")
            .await
            .unwrap();
        let decision = live_tool_decision(
            &app,
            &access,
            "set_app_view",
            &policy,
            &visibility,
            &state,
            &run["loaded_abilities"],
            false,
        );
        assert_ne!(
            decision.permission,
            access::Permission::Deny,
            "{:?}",
            decision.reason
        );
        let result = tools::native::app_control::execute(
            &tools::native_host::Host(&app),
            tools::native::Call {
                sid: &sid,
                run_id: &rid,
                generation: "fixture",
                pending_id: &pending,
            },
            "set_app_view",
            &args,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result["output"]["requested"], true);
        let projected = tools::core_app_control::projection(&app, &sid)
            .await
            .unwrap();
        assert_eq!(projected["request"]["destination"], "app:files");
        let mut outsider = app.clone();
        outsider.chat_actor = Some("other-user".into());
        assert!(tools::core_app_control::projection(&outsider, &sid)
            .await
            .unwrap()
            .is_null());
        assert!(check(
            &app,
            &sid,
            &rid,
            "fixture",
            &pending,
            "set_app_view",
            &json!({"view":"settings"}),
            &CancellationToken::new()
        )
        .await
        .is_err());
        assert!(check(
            &app,
            &sid,
            &rid,
            "stale-generation",
            &pending,
            "set_app_view",
            &args,
            &CancellationToken::new()
        )
        .await
        .is_err());
        let single = run["pending"].clone();
        run["pending"] = Value::Null;
        run["batch"] = json!([{"state":"executing","pending":single}]);
        app.store.put(&key("run", &rid), &run).await.unwrap();
        let normalized = check(
            &app,
            &sid,
            &rid,
            "fixture",
            &pending,
            "set_app_view",
            &args,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(normalized["pending"]["id"], pending);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert!(check(
            &app,
            &sid,
            &rid,
            "fixture",
            &pending,
            "set_app_view",
            &args,
            &cancelled
        )
        .await
        .is_err());
        run["status"] = json!("cancelled");
        app.store.put(&key("run", &rid), &run).await.unwrap();
        assert!(tools::native::app_control::execute(
            &tools::native_host::Host(&app),
            tools::native::Call {
                sid: &sid,
                run_id: &rid,
                generation: "fixture",
                pending_id: &pending,
            },
            "set_app_view",
            &args,
            CancellationToken::new(),
        )
        .await
        .is_err());
        assert_eq!(
            tools::core_app_control::projection(&app, &sid)
                .await
                .unwrap()["revision"],
            projected["revision"]
        );
    }
}
