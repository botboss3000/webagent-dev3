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
    let run = app.run(rid).await?;
    let pending = if run["pending"]["id"] == pending_id {
        &run["pending"]
    } else {
        run["batch"]
            .as_array()
            .and_then(|xs| xs.iter().find(|x| x["pending"]["id"] == pending_id))
            .map(|v| &v["pending"])
            .unwrap_or(&Value::Null)
    };
    if cancel.is_cancelled()
        || app.shutoff().await?
        || run["session_id"] != sid
        || run["status"] != "running"
        || run["generation"] != generation
        || pending["name"] != name
        || pending["state"] != "executing"
        || pending["arguments"] != *args
        || !app.store.participation_allows_run(&run).await?
    {
        return Err(anyhow!("Core invocation is no longer authorized"));
    }
    let agent = session_agent_id(app, sid).await?;
    if run["agent_id"].as_str().unwrap_or("shared_default") != agent {
        return Err(anyhow!("Run agent changed"));
    }
    let (policy, _, visibility, state) = ability_controls(app, sid).await?;
    let mode = session_mode(app, sid).await?;
    let access = shared_chat::live_access(&app.store, shared_chat::actor(app), sid, &agent).await?;
    let decision = live_tool_decision(
        app,
        &access,
        name,
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if !access.admitted()
        || decision.permission == access::Permission::Deny
        || decision.permission == access::Permission::Ask && pending["human_approved"] != true
    {
        return Err(anyhow!(
            "Core tool access changed; obtain fresh authorization"
        ));
    }
    app.store
        .core_native_fence(sid, rid, pending_id, generation)
        .await?;
    let selected = pending.clone();
    let mut view = run;
    view["pending"] = selected;
    Ok(view)
}

/// A wrapper cannot use another ability's implementation to bypass its live tool denial.
pub async fn dependency(app: &App, sid: &str, run: &Value, name: &str) -> Result<()> {
    let agent = session_agent_id(app, sid).await?;
    let (policy, _, visibility, state) = ability_controls(app, sid).await?;
    let mode = session_mode(app, sid).await?;
    let access = shared_chat::live_access(&app.store, shared_chat::actor(app), sid, &agent).await?;
    let decision = live_tool_decision(
        app,
        &access,
        name,
        &policy,
        &visibility,
        &state,
        &run["loaded_abilities"],
        mode["mode"] == "read_only",
    )
    .for_execution_mode(&mode);
    if !access.admitted()
        || decision.permission == access::Permission::Deny
        || decision.permission == access::Permission::Ask
            && run["pending"]["human_approved"] != true
    {
        return Err(anyhow!(
            "Required dependency tool is unavailable or needs fresh approval"
        ));
    }
    Ok(())
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
            .save_capability_level("shared_default", level, 0)
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
        let result = tools::core_app_control::execute(
            &app,
            &sid,
            &rid,
            "fixture",
            &pending,
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
        assert!(tools::core_app_control::execute(
            &app,
            &sid,
            &rid,
            "fixture",
            &pending,
            &args,
            CancellationToken::new()
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
