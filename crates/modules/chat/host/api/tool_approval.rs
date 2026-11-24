//! The tool approval decision: Approve or Deny on a waiting call's card, optionally saving
//! the approval for later matching calls ("Remember this approval").
use super::{
    access::saved_approvals, anyhow, bad, conflict, digest, json, key, public_run, required,
    session_ability_state, session_agent_abilities, session_mode, status, Api, ApiError, App, Json,
    Path, State, StatusCode, SystemTime, Uuid, Value, UNIX_EPOCH,
};

pub(crate) async fn approval(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(body): Json<Value>,
) -> Api {
    let req = required(&body, "client_request_id", 128)?;
    let decision = required(&body, "decision", 20)?;
    if !matches!(decision, "approve" | "deny") {
        return Err(bad("Invalid approval decision"));
    }
    let _guard = app.gate.lock().await;
    let receipt_key = format!(
        "receipt:approval:{id}:{}:{req}",
        crate::shared_chat::actor(&app)
    );
    if let Some(r) = app.store.get(&receipt_key).await? {
        if r["digest"] != digest(&body) {
            return Err(conflict("Identifier reused"));
        }
        return Ok(Json(r["result"].clone()));
    }
    let view = app
        .store
        .active_runs()
        .await?
        .into_iter()
        .find(|r| r["pending"]["id"] == id)
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "Approval not found".into()))?;
    let mut run = app
        .run(
            view["id"]
                .as_str()
                .ok_or_else(|| anyhow!("Run without an ID"))?,
        )
        .await?;
    if status(&run) != "waiting_approval" {
        return Err(conflict("Approval is no longer pending"));
    }
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow!("Run without a session"))?;
    if !webagent_actions::outcome::may_confirm_new_intent(
        &run,
        app.store.current_user_id(),
        crate::shared_chat::actor(&app),
    ) {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Only the requesting user may confirm an additional matching action".into(),
        ));
    }
    if let Some(owner) = run["pending"]["authorization"]["owner_user_id"].as_str() {
        if owner != crate::shared_chat::actor(&app) {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Only the credential owner may authorize this tool".into(),
            ));
        }
    }
    if !crate::credential_authorization::valid(&app.store, &run, &run["pending"]).await? {
        run["pending"]["state"] = json!("cancelled");
        run["pending"]["decision"] = Value::Null;
        run["pending"]["human_approved"] = json!(false);
        run["status"] = json!("stopped");
        app.save(&mut run).await?;
        return Err(conflict("Credential authorization changed"));
    }
    if !app.store.participation_allows_run(&run).await? {
        return Err(conflict("Agent is inactive or participation changed"));
    }
    if crate::session_messaging::owns(run["pending"]["name"].as_str().unwrap_or("")) {
        if crate::shared_chat::actor(&app) != app.store.current_user_id() {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "Only the private owner can approve messaging".into(),
            ));
        }
        let name = run["pending"]["name"]
            .as_str()
            .ok_or_else(|| anyhow!("Pending call without a name"))?;
        if decision == "approve" {
            let fresh =
                crate::session_messaging::binding(&app, &run, name, &run["pending"]["arguments"])
                    .await
                    .map_err(|_| conflict("Messaging destination changed"))?;
            if fresh != run["pending"]["messaging_binding"] {
                return Err(conflict(
                    "Messaging destination changed; request a fresh approval",
                ));
            }
        }
        if body
            .get("remember_messaging_route")
            .is_some_and(|v| !v.is_boolean())
        {
            return Err(bad("Invalid remember route choice"));
        }
        if body["remember_messaging_route"] == true {
            if decision != "approve" || run["pending"]["messaging_can_remember"] != true {
                return Err(bad("This approval cannot grant a route"));
            }
        }
    } else if body.get("remember_messaging_route").is_some() {
        return Err(bad("Messaging route choice is not applicable"));
    }
    // "Continue in Ask mode" on an Auto request approves the call with Ask as its target:
    // the mode tool then enters Ask instead, so the agent continues with approvals on.
    let to_ask = match body.get("alternate_mode") {
        None => false,
        Some(mode) if mode == "ask" => {
            if decision != "approve"
                || run["pending"]["name"] != "set_execution_mode"
                || run["pending"]["arguments"]["mode"] != "auto"
            {
                return Err(bad(
                    "Alternate mode applies only when approving a switch to Auto",
                ));
            }
            true
        }
        Some(_) => return Err(bad("Invalid alternate mode")),
    };
    // "Remember this approval": only on a call whose card offered it, chosen by the chat owner
    // for the owner's own run (see webagent_access::saved_approvals).
    let remember = match body.get("remember") {
        None | Some(Value::Null) => None,
        Some(choice) => {
            let choice = saved_approvals::Choice::parse(choice)
                .map_err(|_| bad("Invalid remember choice"))?;
            if decision != "approve" || !run["pending"]["remember"].is_object() {
                return Err(bad("This approval cannot be remembered"));
            }
            let owner = app.store.current_user_id();
            if crate::shared_chat::actor(&app) != owner
                || run["requester_user_id"]
                    .as_str()
                    .is_some_and(|requester| requester != owner)
            {
                return Err(ApiError(
                    StatusCode::FORBIDDEN,
                    "Only the chat owner can save approvals for their own runs".into(),
                ));
            }
            if choice.agent_wide && run["pending"]["remember"]["agent_wide"] != true {
                return Err(bad("Agent-wide saved approvals are switched off"));
            }
            Some(choice)
        }
    };
    let agent_id = run["agent_id"].as_str().unwrap_or("shared_default");
    let access = crate::shared_chat::live_access(
        &app.store,
        run["requester_user_id"]
            .as_str()
            .unwrap_or(app.store.current_user_id()),
        sid,
        agent_id,
    )
    .await
    .map_err(|_| conflict("Access changed; approval is invalid"))?;
    if !access.admitted() || !app.run_model_authorized(&run).await {
        run["status"] = json!("stopped");
        run["pending"]["decision"] = Value::Null;
        app.save(&mut run).await?;
        return Err(conflict("Access changed; approval is invalid"));
    }
    let mode_revision = session_mode(&app, sid).await?["revision"]
        .as_u64()
        .unwrap_or(0);
    let visibility = session_agent_abilities(&app, sid).await?;
    let ability_state = session_ability_state(&app, sid).await?;
    if run["pending"]["access_revision"] != json!(access.revision)
        || run["pending"]["mode_revision"].as_u64().unwrap_or(0) != mode_revision
        || run["pending"]["visibility_revision"].as_u64().unwrap_or(0)
            != visibility["revision"].as_u64().unwrap_or(0)
        || run["pending"]["ability_state_revision"]
            .as_u64()
            .unwrap_or(0)
            != ability_state["revision"].as_u64().unwrap_or(0)
    {
        if !run["pending"]["authorization"].is_null() {
            run["pending"]["state"] = json!("cancelled");
            run["pending"]["decision"] = Value::Null;
            run["pending"]["human_approved"] = json!(false);
            run["status"] = json!("stopped");
            app.save(&mut run).await?;
            return Err(conflict(
                "Policy or session mode changed; owner authorization cancelled",
            ));
        }
        // BATCH-APPROVAL-REVOCATION: a changed policy invalidates only this
        // call. Its next pass gets a new pending ID and fresh revisions.
        let stale_id = run["pending"]["id"].clone();
        if let Some(items) = run["batch"].as_array_mut() {
            for item in items {
                if item["pending"]["id"] == stale_id {
                    item["state"] = json!("new");
                    item["pending"] = Value::Null;
                    item["result"] = Value::Null;
                    break;
                }
            }
            run["pending"] = Value::Null;
        } else {
            run["pending"]["state"] = json!("requested");
            run["pending"]["decision"] = Value::Null;
        }
        run["status"] = json!("queued");
        app.save(&mut run).await?;
        return Err(conflict(
            "Policy or session mode changed; a new gate decision is required",
        ));
    }
    // The saved approval is written with the decision, from the fresh policy's switches.
    let saved = match remember {
        None => None,
        Some(choice) => {
            let settings = access.saved_approvals;
            let fingerprint = run["pending"]["remember"]["fingerprint"]
                .as_str()
                .filter(|fingerprint| !fingerprint.is_empty())
                .ok_or_else(|| bad("This approval cannot be remembered"))?;
            if !settings.enabled || (choice.agent_wide && !settings.agent_wide) {
                return Err(conflict("Saved approvals are switched off"));
            }
            let saved_key = saved_approvals::key(agent_id);
            let current = app.store.get(&saved_key).await?.unwrap_or(Value::Null);
            let created_ms = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                });
            let entry = saved_approvals::NewEntry {
                id: &Uuid::new_v4().to_string(),
                choice,
                tool: run["pending"]["name"].as_str().unwrap_or(""),
                args: &run["pending"]["arguments"],
                session_id: sid,
                agent_id,
                owner: app.store.current_user_id(),
                fingerprint,
                created_ms,
            }
            .to_value();
            Some((saved_key, saved_approvals::add(&current, entry)))
        }
    };
    if crate::session_messaging::owns(run["pending"]["name"].as_str().unwrap_or("")) {
        run["pending"]["remember_messaging_route"] =
            json!(body["remember_messaging_route"] == true);
    }
    run["pending"]["decision"] = json!(decision);
    if to_ask {
        run["pending"]["alternate_mode"] = json!("ask");
    }
    run["status"] = json!(if app.shutoff().await? {
        "stopped"
    } else {
        "queued"
    });
    let result = public_run(&run);
    app.store
        .atomic_docs_event(
            [
                (
                    key(
                        "run",
                        run["id"].as_str().expect("stored runs carry their ID"),
                    ),
                    run.clone(),
                ),
                (receipt_key, json!({"digest":digest(&body),"result":result})),
            ]
            .into_iter()
            .chain(saved)
            .collect(),
            run["session_id"]
                .as_str()
                .expect("stored runs carry their session"),
            "run_status",
            &result,
        )
        .await?;
    Ok(Json(result))
}
