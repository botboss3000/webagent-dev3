//! Tool-owned account authorization is never a participant or agent-policy grant.
use crate::*;
pub(crate) fn declaration(name: &str) -> Option<Value> {
    tools::schemas()
        .into_iter()
        .find(|s| s["name"] == name)
        .and_then(|s| s.get("authorization").cloned())
}
async fn binding(
    store: &store::Store,
    run: &Value,
    name: &str,
    args: &Value,
    requirement: &Value,
) -> Result<Value> {
    if requirement["kind"] != "credential_owner" {
        anyhow::bail!("Unsupported tool authorization declaration");
    }
    let kind = requirement["credential_kind"]
        .as_str()
        .ok_or_else(|| anyhow!("Credential kind missing"))?;
    if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        anyhow::bail!("Invalid credential kind");
    }
    let owner = run["requester_user_id"]
        .as_str()
        .unwrap_or(store.current_user_id());
    // The authenticated run requester selects the credential namespace. Model arguments cannot select it.
    let reference = format!("tool_credential:{owner}:{kind}");
    let credential = store
        .get(&reference)
        .await?
        .ok_or_else(|| anyhow!("The requesting user must configure their own credential"))?;
    if credential["owner_user_id"] != owner || credential["revoked"] == true {
        anyhow::bail!("Credential owner or status invalid");
    }
    let version = credential["version"]
        .as_u64()
        .ok_or_else(|| anyhow!("Credential version missing"))?;
    Ok(
        json!({"kind":"credential_owner","owner_user_id":owner,"credential_ref":reference,"credential_version":version,
        "session_id":run["session_id"],"run_id":run["id"],"tool":name,"arguments_digest":digest(args),
        "participation_generation":run["participation_generation"].as_u64().unwrap_or(0),
        "policy_version":run["pending"]["policy_version"],"mode_revision":run["pending"]["mode_revision"],
        "visibility_revision":run["pending"]["visibility_revision"],"ability_state_revision":run["pending"]["ability_state_revision"],
        "access_revision":run["pending"]["access_revision"]}),
    )
}
pub(crate) async fn resolve(
    store: &store::Store,
    run: &Value,
    name: &str,
    args: &Value,
) -> Result<Option<Value>> {
    match declaration(name) {
        Some(requirement) => Ok(Some(binding(store, run, name, args, &requirement).await?)),
        None => Ok(None),
    }
}
pub(crate) async fn valid(store: &store::Store, run: &Value, pending: &Value) -> Result<bool> {
    let name = pending["name"].as_str().unwrap_or("");
    let Some(requirement) = declaration(name) else {
        return Ok(pending["authorization"].is_null());
    };
    let selected=json!({"id":run["id"],"session_id":run["session_id"],"requester_user_id":run["requester_user_id"],"participation_generation":run["participation_generation"],"pending":pending});
    Ok(
        binding(store, &selected, name, &pending["arguments"], &requirement)
            .await
            .is_ok_and(|current| current == pending["authorization"]),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn approved_test_adapter_dispatches_only_with_fresh_owner_binding() {
        let (root, mut app) = crate::runtime_tests::fixture().await;
        let packages = root.path().join("test-packages");
        let base = packages.join("Core/base");
        std::fs::create_dir_all(&base).unwrap();
        let source =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities/Core/base");
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(source.join("base.json")).unwrap()).unwrap();
        manifest["tools"]
            .as_array_mut()
            .unwrap()
            .push(json!("test_owner_purchase"));
        std::fs::write(
            base.join("base.json"),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        std::fs::copy(source.join("base.skill.md"), base.join("base.skill.md")).unwrap();
        app.abilities = std::sync::Arc::new(abilities::Catalog::load(&packages).unwrap());
        // Install the development adapter grants only in this disposable fixture.
        let mut access_policy=app.store.read_access_policy().unwrap();
        access_policy["tiers"]["operator"]["tools"]["test_owner_purchase"]=json!("ask");
        access_policy["tiers"]["operator"]["ability_permissions"]["base"]=json!("auto");
        app.store.write_access_policy(&access_policy).unwrap();
        let levels=app.store.capability_levels("shared_default").await.unwrap();
        let mut level=levels.levels["administrator"].clone();
        level.ability_permissions.insert("base".into(),crate::access::Permission::Auto);
        app.store.save_capability_level("shared_default",level,levels.revision).await.unwrap();
        let mut policy = app.store.get("policy").await.unwrap().unwrap();
        policy["rules"]["test_owner_purchase"] = json!("require_approval");
        app.store.put("policy", &policy).await.unwrap();
        // The fixture tool must be visible through the ordinary ability gate.
        // Owner approval does not activate a hidden ability.
        let mut visibility=app.store.agent_abilities().await.unwrap();
        let revision=visibility["revision"].as_u64().unwrap();
        visibility["visibility"]["base"]=json!("visible");
        visibility["revision"]=json!(revision+1);
        app.store.save_agent_abilities(&visibility,revision).await.unwrap();
        let sid = crate::runtime_tests::conversation(&app).await;
        app.store.put(&format!("session_mode:{sid}"),
            &json!({"mode":"write","execution_mode":"auto","revision":0})).await.unwrap();
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        let owner = app.store.current_user_id().to_owned();
        app.store
            .put(
                &format!("tool_credential:{owner}:test"),
                &json!({"owner_user_id":owner,"version":1,"revoked":false}),
            )
            .await
            .unwrap();
        let mut run = app.run(&rid).await.unwrap();
        run["requester_user_id"] = json!(owner);
        let generation = uuid();
        assert!(app
            .store
            .claim_run(&mut run, "owner-adapter-test", &generation)
            .await
            .unwrap());
        let (policy, mode_revision, visibility, state) =
            ability_controls(&app, &sid).await.unwrap();
        let agent = run["agent_id"].as_str().unwrap_or("shared_default");
        let access = crate::shared_chat::live_access(&app.store, &owner, &sid, agent)
            .await
            .unwrap();
        let mode=session_mode(&app,&sid).await.unwrap();
        let effective=live_tool_decision(&app,&access,"test_owner_purchase",&policy,&visibility,&state,&run["loaded_abilities"],mode["mode"]=="read_only").for_execution_mode(&mode);
        assert_ne!(effective.permission,crate::access::Permission::Deny,"fixture ordinary tool gate: {effective:?}");
        assert!(app.run_model_authorized(&run).await,"fixture model authority");
        run["pending"] = json!({"id":"owner-dispatch","name":"test_owner_purchase","arguments":{},"call_id":"test-call","state":"executing","human_approved":false,
            "policy_version":policy["version"],"mode_revision":mode_revision,"visibility_revision":visibility["revision"],"ability_state_revision":state["revision"],"access_revision":access.revision});
        run["pending"]["authorization"] =
            resolve(&app.store, &run, "test_owner_purchase", &json!({}))
                .await
                .unwrap()
                .unwrap();
        app.store.put(&format!("run:{rid}"), &run).await.unwrap();
        let denied = tools::call_foreground(
            app.clone(),
            sid.clone(),
            rid.clone(),
            generation.clone(),
            "test_owner_purchase".into(),
            json!({}),
            "owner-dispatch".into(),
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(denied["status"], "denied");
        run["pending"]["human_approved"] = json!(true);
        app.store.put(&format!("run:{rid}"), &run).await.unwrap();
        let success = tools::call_foreground(
            app.clone(),
            sid.clone(),
            rid.clone(),
            generation.clone(),
            "test_owner_purchase".into(),
            json!({}),
            "owner-dispatch".into(),
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(success["status"], "success", "dispatch result: {success}");
        assert_eq!(success["output"]["approved_for"], owner);
        app.store
            .put(
                &format!("tool_credential:{owner}:test"),
                &json!({"owner_user_id":owner,"version":2,"revoked":true}),
            )
            .await
            .unwrap();
        let denied = tools::call_foreground(
            app,
            sid,
            rid,
            generation,
            "test_owner_purchase".into(),
            json!({}),
            "owner-dispatch".into(),
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(denied["status"], "denied");
    }
    #[tokio::test]
    async fn registered_development_tool_requires_owner_and_fresh_credential() {
        let (_root, app) = crate::runtime_tests::fixture().await;
        let sid = crate::runtime_tests::conversation(&app).await;
        let rid = crate::runtime_tests::queued(&app, &sid).await;
        app.store
            .put(
                "tool_credential:alice:test",
                &json!({"owner_user_id":"alice","version":1,"revoked":false}),
            )
            .await
            .unwrap();
        let mut run = app.run(&rid).await.unwrap();
        run["requester_user_id"] = json!("alice");
        let args = json!({});
        let authorization = resolve(&app.store, &run, "test_owner_purchase", &args)
            .await
            .unwrap()
            .unwrap();
        run["status"] = json!("waiting_approval");
        run["pending"] = json!({"id":"owner-checkpoint","name":"test_owner_purchase","arguments":args,"authorization":authorization,"state":"waiting"});
        app.store.put(&format!("run:{rid}"), &run).await.unwrap();
        let mut bob = app.clone();
        bob.chat_actor = Some("bob".into());
        let denied = crate::approval(
            State(bob),
            Path("owner-checkpoint".into()),
            Json(json!({"client_request_id":"wrong-owner","decision":"approve"})),
        )
        .await;
        assert!(denied.is_err());
        assert_eq!(app.run(&rid).await.unwrap()["status"], "waiting_approval");
        // Live credential mutation invalidates the previously displayed panel.
        app.store
            .put(
                "tool_credential:alice:test",
                &json!({"owner_user_id":"alice","version":2,"revoked":true}),
            )
            .await
            .unwrap();
        assert!(!valid(&app.store, &run, &run["pending"]).await.unwrap());
    }
    #[tokio::test]
    async fn owner_binding_never_uses_argument_owner_and_revocation_invalidates() {
        let root = tempfile::tempdir().unwrap();
        let store = store::Store::open(root.path()).await.unwrap();
        store
            .put(
                "tool_credential:alice:test",
                &json!({"owner_user_id":"alice","version":1,"revoked":false}),
            )
            .await
            .unwrap();
        let run = json!({"id":"r","session_id":"s","requester_user_id":"alice","participation_generation":3});
        let requirement = json!({"kind":"credential_owner","credential_kind":"test"});
        let value = binding(
            &store,
            &run,
            "purchase",
            &json!({"user_id":"bob"}),
            &requirement,
        )
        .await
        .unwrap();
        assert_eq!(value["owner_user_id"], "alice");
        assert_eq!(value["credential_ref"], "tool_credential:alice:test");
        let mut other = run.clone();
        other["requester_user_id"] = json!("bob");
        assert!(
            binding(&store, &other, "purchase", &json!({}), &requirement)
                .await
                .is_err()
        );
        store
            .put(
                "tool_credential:alice:test",
                &json!({"owner_user_id":"alice","version":2,"revoked":true}),
            )
            .await
            .unwrap();
        assert!(binding(&store, &run, "purchase", &json!({}), &requirement)
            .await
            .is_err());
    }
}
