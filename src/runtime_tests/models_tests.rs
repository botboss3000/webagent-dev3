//! Runtime acceptance tests: Model selection, catalogs, providers, failover and provider recovery.
use super::*;

#[tokio::test]
async fn default_agent_model_is_saved_independently_of_global_settings() {
    let (_root, app) = fixture().await;
    let initial = agent_model(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(initial["model"], DEFAULT_AGENT_MODEL);
    let saved = agent_model_set(
        State(app.clone()),
        Path("shared_default".into()),
        Json(json!({"model":"gpt-6-sol"})),
    )
    .await
    .unwrap()
    .0;
    assert_eq!(saved["provider"], "openai_subscription");
    assert_eq!(saved["model"], "gpt-6-sol");
    assert_eq!(
        app.store.get("settings").await.unwrap().unwrap()["model"],
        "fixture"
    );
    assert_eq!(
        agent_model(State(app.clone()), Path("shared_default".into()))
            .await
            .unwrap()
            .0["model"],
        "gpt-6-sol"
    );
    assert_eq!(
        agent_model(State(app), Path("unknown".into()))
            .await
            .unwrap_err()
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn old_default_model_is_migrated_once_and_new_choices_are_preserved() {
    let (_root, app) = fixture().await;
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"gpt-5.4"}))
        .await
        .unwrap();
    migrate_default_agent_model(&app.store).await.unwrap();
    assert_eq!(
        app.store.agent_model().await.unwrap()["model"],
        DEFAULT_AGENT_MODEL
    );
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"gpt-6-sol"}))
        .await
        .unwrap();
    migrate_default_agent_model(&app.store).await.unwrap();
    assert_eq!(app.store.agent_model().await.unwrap()["model"], "gpt-6-sol");
}

#[tokio::test]
async fn vision_capable_candidates_receive_sent_images_and_others_keep_text() {
    use axum::routing::post;
    let (_root, mut app) = fixture().await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new().route("/responses", post(
        |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>, Json(body): Json<Value>| async move {
            requests.lock().await.push(body);
            let output = json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Seen."}]}]);
            let event = json!({"type":"response.completed","response":{"status":"completed","output":output}});
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    )).with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    for (sees, generation) in [(true, "vision"), (false, "text-only")] {
        let sid = conversation(&app).await;
        let aid = uuid();
        crate::api::upload(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_upload_id":aid,
            "name":"dot.png","media_type":"image/png","data":"iVBORw0KGgo="})),
        )
        .await
        .unwrap();
        let (_, Json(sent)) = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),
            "text":"What is this?","attachment_ids":[aid]})),
        )
        .await
        .unwrap();
        let id = sent["run_id"].as_str().unwrap().to_owned();
        // Hydrate as the scheduler would, and mark the candidate as the plan would for a vision entry.
        let mut run = app.run(&id).await.unwrap();
        run["input"] = json!(app.store.context(&sid).await.unwrap());
        run["context_hydration_pending"] = json!(false);
        run["selection_plan"]["candidates"][0]["vision"] = json!(sees);
        app.store.put(&key("run", &id), &run).await.unwrap();
        running(&app, &id, generation).await;
        execute_run(
            app.clone(),
            id.clone(),
            generation.into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
        // Session naming shares the mock provider; the chat call is the one carrying this message.
        let user = requests
            .lock()
            .await
            .drain(..)
            .flat_map(|request| request["input"].as_array().cloned().unwrap_or_default())
            .find(|item| item["role"] == "user" && item.to_string().contains("What is this?"))
            .unwrap();
        let image = json!({"type":"input_image","image_url":"data:image/png;base64,iVBORw0KGgo="});
        if sees {
            assert_eq!(
                user["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|part| **part == image)
                    .count(),
                1
            );
        } else {
            assert!(user
                .to_string()
                .contains("Image description is not available"));
            assert!(!user.to_string().contains("input_image"));
        }
        // Durable context keeps text either way; the image is call-scoped.
        let context = app.store.context(&sid).await.unwrap();
        assert!(!json!(context).to_string().contains("input_image"));
        assert!(!app.run(&id).await.unwrap()["input"]
            .to_string()
            .contains("input_image"));
    }
    server.abort();
}

#[tokio::test]
async fn agent_model_settings_normalize_without_losing_selected_model() {
    let (_root, app) = fixture().await;
    app.store
        .save_agent_model(&json!({"provider":"openai_subscription","model":"legacy-chosen"}))
        .await
        .unwrap();
    migrate_provider_selection(&app.store).await.unwrap();
    migrate_provider_selection(&app.store).await.unwrap();
    let value = app.store.agent_model().await.unwrap();
    assert_eq!(
        value,
        json!({"provider":"openai_subscription","model":"legacy-chosen"})
    );
    let read = agent_model(State(app.clone()), Path("shared_default".into()))
        .await
        .unwrap()
        .0;
    assert_eq!(read["provider"], "openai_subscription");
    assert_eq!(read["model"], "legacy-chosen");
}

#[tokio::test]
async fn unsupported_provider_is_rejected_without_changing_settings() {
    let (_root, app) = fixture().await;
    let original = json!({"model":"original","provider":"openai_subscription"});
    app.store.save_agent_model(&original).await.unwrap();
    for provider in [json!("unsupported"), json!(null), json!(7)] {
        let body = json!({"model":"replacement","provider":provider});
        assert_eq!(
            settings(State(app.clone()), Json(body.clone()))
                .await
                .unwrap_err()
                .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            agent_model_set(
                State(app.clone()),
                Path("shared_default".into()),
                Json(body)
            )
            .await
            .unwrap_err()
            .0,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(app.store.agent_model().await.unwrap(), original);
    }
}

#[tokio::test]
/// Verify an ability that requires a model role cannot be loaded until the roster appoints it.
async fn image_generation_stays_off_without_an_image_out_model() {
    use axum::routing::post;

    let (_root, mut app) = fixture().await;
    let mut levels = app.store.capability_levels("shared_default").await.unwrap();
    for id in ["operator", "member", "visitor"] {
        let mut level = levels.levels[id].clone();
        level
            .ability_permissions
            .insert("image_generation".into(), crate::access::Permission::Auto);
        levels = app
            .store
            .save_capability_level("shared_default", level, levels.revision)
            .await
            .unwrap();
    }
    set_tool_permission(&app, "generate_image", access::Permission::Auto).await;
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let mock = Router::new()
        .route(
            "/responses",
            post(
                |AxumState(requests): AxumState<Arc<Mutex<Vec<Value>>>>,
                 Json(body): Json<Value>| async move {
                    requests.lock().await.push(body);
                    let event = json!({"type":"response.completed","response":{"status":"completed",
                    "output":[{"type":"message","role":"assistant",
                        "content":[{"type":"output_text","text":"Done."}]}]}});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {event}\n\n"),
                    )
                },
            ),
        )
        .with_state(captured);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    // Whether the model call carried generate_image's schema.
    let offered = |app: App, generation: &'static str| {
        let requests = requests.clone();
        async move {
            requests.lock().await.clear();
            let sid = conversation(&app).await;
            let sent = send(
                State(app.clone()),
                Path(sid.clone()),
                Json(json!({"client_message_id":uuid(),"text":"draw a cat"})),
            )
            .await
            .unwrap();
            let id = sent.1 .0["run_id"].as_str().unwrap().to_owned();
            running(&app, &id, generation).await;
            execute_fixture_journey(
                app.clone(),
                id.clone(),
                generation.into(),
                CancellationToken::new(),
            )
            .await;
            let requests = requests.lock().await;
            assert!(!requests.is_empty());
            requests.iter().any(|body| {
                body["tools"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|tool| tool["name"] == "generate_image")
            })
        }
    };
    assert!(!offered(app.clone(), "role-missing").await);

    let mut catalog = app.store.model_catalog().await.unwrap();
    let standard = catalog.agent_models["shared_default"].roles
        [&model_selection::ModelRole::Standard]
        .entry_id
        .clone();
    let mut image = catalog.entries[&standard].clone();
    image.id = "image".into();
    image.capabilities.insert("image".into());
    image.revision = 1;
    catalog.entries.insert(image.id.clone(), image);
    let roster = catalog.agent_models.get_mut("shared_default").unwrap();
    roster.entry_ids.push("image".into());
    roster.roles.insert(
        model_selection::ModelRole::Image,
        model_selection::RoleAssignment::new("image"),
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    assert!(offered(app.clone(), "role-present").await);
    server.abort();
}
#[tokio::test]
async fn model_selection_admission_pins_agent_model_prompt_and_credentials() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    app.store
        .upsert_prompt_slot(
            "selection-isolation",
            "DEFAULT PRIVATE PROMPT",
            Some(5),
            None,
            None,
            0,
        )
        .await
        .unwrap();
    let agent = app.store.for_agent("agent-a").await.unwrap();
    sqlx::query("INSERT INTO agent_prompts(id,agent_id,slot_name,order_index,lock,merge_mode,content,revision,updated_at) VALUES('agent-a:selection-isolation','agent-a','selection-isolation',5,0,'replace',?,1,0)")
        .bind(agent.seal_agent(&json!("AGENT A PRIVATE PROMPT")).unwrap()).execute(&agent.agent_pool).await.unwrap();
    for (agent_id, expected_model, expected_token, expected_prompt, excluded) in [
        (
            "shared_default",
            "model-one",
            "token-one",
            "DEFAULT PRIVATE PROMPT",
            "AGENT A PRIVATE PROMPT",
        ),
        (
            "agent-a",
            "model-two",
            "token-two",
            "AGENT A PRIVATE PROMPT",
            "DEFAULT PRIVATE PROMPT",
        ),
    ] {
        let Json(session) = create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":agent_id})),
        )
        .await
        .unwrap();
        let sid = session["id"].as_str().unwrap().to_owned();
        assert_eq!(session_agent_id(&app, &sid).await.unwrap(), agent_id);
        let (_, Json(accepted)) = send(
            State(app.clone()),
            Path(sid.clone()),
            Json(json!({"client_message_id":uuid(),"text":"hello"})),
        )
        .await
        .unwrap();
        let run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
        assert_eq!(run["agent_id"], agent_id);
        assert_eq!(run["provider_selection"]["model"], expected_model);
        assert!(run["instructions"]
            .as_str()
            .unwrap_or_default()
            .contains(expected_prompt));
        assert!(!run["instructions"].as_str().unwrap().contains(excluded));
        let selection =
            provider::ProviderSelection::parse(&run["provider_selection"], "unused").unwrap();
        assert_eq!(
            app.tokens_for(&selection, &run).await.unwrap()["access_token"],
            expected_token
        );
        if agent_id == "agent-a" {
            catalog
                .agent_models
                .get_mut(agent_id)
                .unwrap()
                .roles
                .insert(
                    model_selection::ModelRole::Standard,
                    model_selection::RoleAssignment::new("one"),
                );
            catalog = app
                .store
                .save_model_catalog(&catalog, catalog.revision)
                .await
                .unwrap();
            assert_eq!(
                app.run_candidate(&run)
                    .await
                    .unwrap()
                    .unwrap()
                    .upstream_model,
                "model-two"
            );
            assert_eq!(
                app.tokens_for(&selection, &run).await.unwrap()["access_token"],
                "token-two"
            );
        }
    }
    assert_eq!(
        app.store
            .list_prompt_slots()
            .await
            .unwrap()
            .iter()
            .filter(|p| p["slot_name"] == "selection-isolation")
            .count(),
        1
    );
    assert_eq!(
        app.store
            .get_prompt_slot("selection-isolation")
            .await
            .unwrap()
            .unwrap()["content"],
        "DEFAULT PRIVATE PROMPT"
    );
}

#[tokio::test]
async fn model_selection_failover_is_ordered_durable_and_revocation_is_live() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let mut run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    let failure = provider::typed_failure(
        model_selection::FailureKind::ModelUnavailable,
        false,
        "Model unavailable",
    );
    model_failure(&mut run, &failure).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["provider_selection"]["model"], "model-one");
    assert_eq!(run["failover_state"]["history"][0]["to_entry_id"], "one");
    app.save(&mut run).await.unwrap();
    let mut restored = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(restored["failover_state"], run["failover_state"]);
    let selection =
        provider::ProviderSelection::parse(&restored["provider_selection"], "unused").unwrap();
    assert_eq!(
        app.tokens_for(&selection, &restored).await.unwrap()["access_token"],
        "token-one"
    );
    catalog.connections.get_mut("one").unwrap().revoked = true;
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    assert!(app.run_candidate(&restored).await.is_err());
    model_failure(&mut restored, &failure).unwrap();
    assert_eq!(restored["status"], "failed");
    // The chat notice names no model or provider; the operator row carries them.
    assert_eq!(
        restored["error"],
        webagent_engine::runtime::failure::MODEL_UNREACHABLE
    );
    let row = &restored["_activity_items"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()["item"];
    assert_eq!(row["type"], "model_unreachable");
    assert_eq!(row["model"], restored["provider_selection"]["model"]);
    let detail = row["detail"].as_str().unwrap();
    assert!(detail.contains("no safe retry or eligible backup model remains"));
    assert!(detail.contains("Last error: Model unavailable"));
}

/// Owner decision 2026-11-23: an agent's bridge model answers everyone the agent admits on its
/// owner's link (a member, a widget visitor), like any other model; someone the agent does not
/// admit and a revoked link are still refused, and a caller's own link for the same model on
/// the agent answers that caller.
#[tokio::test]
async fn an_agents_bridge_model_answers_everyone_the_agent_admits() {
    use webagent_accounts_host::AccountsStore;
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    for id in ["one", "two"] {
        catalog.connections.get_mut(id).unwrap().transport =
            model_selection::Transport::OpencodeBridge;
    }
    let catalog = app
        .store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let account = |name: &'static str| {
        let app = app.clone();
        async move {
            let account = app
                .store
                .create_user_account(name, "fixture-only-password-hash")
                .await
                .unwrap();
            let id = account["user_id"].as_str().unwrap().to_owned();
            app.store
                .update_account_access(&id, 1, crate::access::AccountStatus::Active, "free")
                .await
                .unwrap();
            id
        }
    };
    let member = account("bridge_member").await;
    let outsider = account("bridge_outsider").await;
    app.store
        .set_agent_membership(
            &member,
            "shared_default",
            crate::access::Relationship::Caller,
            0,
        )
        .await
        .unwrap();
    // A widget visitor is admitted exactly as the widget's guest route does it.
    app.store.set_registration_policy("open").unwrap();
    let guest = app.store.create_guest_account().await.unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.store
        .set_agent_membership(
            &guest,
            "shared_default",
            crate::access::Relationship::Caller,
            0,
        )
        .await
        .unwrap();
    let plan = catalog.resolve("shared_default").unwrap();
    let state = model_selection::FailoverState {
        candidate_index: 0,
        retries: 0,
        response_started: false,
        uncertain_tool_effect: false,
        history: Vec::new(),
    };
    let run = |user: &str| {
        json!({"id":"bridge-run","agent_id":"shared_default","session_id":"bridge-session",
            "requester_user_id":user,"selection_plan":plan,"failover_state":state})
    };
    let answering = |user: String| {
        let app = app.clone();
        let run = run(&user);
        async move {
            app.run_candidate(&run)
                .await
                .map(|c| c.unwrap().connection_id)
        }
    };
    for user in ["admin".to_owned(), member.clone(), guest.clone()] {
        assert_eq!(answering(user.clone()).await.unwrap(), "one", "{user}");
    }
    assert!(answering(outsider.clone()).await.is_err());
    // The member's own link for the same model on this agent answers the member only.
    let mut own = catalog.clone();
    let mut connection = own.connections["one"].clone();
    connection.id = "mine".into();
    connection.owner_id = member.clone();
    connection.credential_ref = "credential:mine".into();
    connection.revision = 1;
    let mut entry = own.entries["one"].clone();
    entry.id = "mine".into();
    entry.owner_id = member.clone();
    entry.connection_id = "mine".into();
    entry.revision = 1;
    own.connections.insert("mine".into(), connection);
    own.entries.insert("mine".into(), entry);
    own.agent_models
        .get_mut("shared_default")
        .unwrap()
        .entry_ids
        .push("mine".into());
    let mut own = app
        .store
        .save_model_catalog(&own, own.revision)
        .await
        .unwrap();
    assert_eq!(answering(member.clone()).await.unwrap(), "mine");
    assert_eq!(answering(guest.clone()).await.unwrap(), "one");
    assert_eq!(answering("admin".into()).await.unwrap(), "one");
    // A revoked agent link is refused for everyone, as for any model.
    own.connections.get_mut("one").unwrap().revoked = true;
    app.store
        .save_model_catalog(&own, own.revision)
        .await
        .unwrap();
    for user in [guest, "admin".to_owned()] {
        assert!(answering(user).await.is_err());
    }
}

#[tokio::test]
// A cleared role is authoritative even while legacy credentials still exist.
async fn model_roles_standard_is_required_even_with_legacy_default_and_other_roles() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    for agent in ["shared_default", "agent-a"] {
        catalog
            .agent_models
            .get_mut(agent)
            .unwrap()
            .roles
            .remove(&model_selection::ModelRole::Standard);
    }
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    for agent in ["shared_default", "agent-a"] {
        let Json(session) = create_session(
            State(app.clone()),
            Json(json!({"client_request_id":uuid(),"agent_id":agent})),
        )
        .await
        .unwrap();
        let error = send(
            State(app.clone()),
            Path(session["id"].as_str().unwrap().into()),
            Json(json!({"client_message_id":uuid(),"text":"hello"})),
        )
        .await
        .unwrap_err();
        assert_eq!(error.0, StatusCode::CONFLICT);
        assert!(error.1.contains("Text model required"));
        assert!(app.store.active_runs().await.unwrap().is_empty());
    }
}

#[tokio::test]
// Custom admission pins one permitted entry without mutating durable appointments.
async fn model_roles_custom_uses_roster_without_reassigning_standard() {
    let (_root, app) = fixture().await;
    let catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello","model_entry_id":"one"})),
    )
    .await
    .unwrap();
    let run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(run["selection_plan"]["custom_entry_id"], "one");
    assert_eq!(run["provider_selection"]["model"], "model-one");
    // A composer pick brings its own aliases: "one" falls back to "two".
    let candidates: Vec<_> = run["selection_plan"]["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["entry_id"].as_str().unwrap())
        .collect();
    assert_eq!(candidates, ["one", "two"]);
    assert_eq!(app.store.model_catalog().await.unwrap(), catalog);
    assert_eq!(
        catalog.resolve("agent-a").unwrap().candidates[0].entry_id,
        "two"
    );
    let Json(other) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    assert!(send(
        State(app.clone()),
        Path(other["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello","model_entry_id":"not-in-roster"}))
    )
    .await
    .is_err());
}

#[tokio::test]
// Compaction owns a separate pinned plan and retry state across storage recovery.
async fn model_roles_system_is_pinned_and_failover_is_separate_after_recovery() {
    let (root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let id = accepted["run_id"].as_str().unwrap();
    let mut run = app.run(id).await.unwrap();
    let standard = run["selection_plan"].clone();
    let standard_state = run["failover_state"].clone();
    assert_eq!(run["system_selection_plan"]["role"], "system");
    assert_eq!(
        system_dispatch_run(&run).unwrap()["provider_selection"]["model"],
        "model-one"
    );
    catalog
        .agent_models
        .get_mut("agent-a")
        .unwrap()
        .roles
        .insert(
            model_selection::ModelRole::System,
            model_selection::RoleAssignment::new("two"),
        );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    system_model_failure(
        &mut run,
        &provider::typed_failure(
            model_selection::FailureKind::ModelUnavailable,
            false,
            "unavailable",
        ),
    )
    .unwrap();
    assert_eq!(run["selection_plan"], standard);
    assert_eq!(run["failover_state"], standard_state);
    assert_eq!(run["system_failover_state"]["candidate_index"], 1);
    assert_eq!(
        system_dispatch_run(&run).unwrap()["provider_selection"]["model"],
        "model-two"
    );
    app.save(&mut run).await.unwrap();
    let reopened = Store::open(root.path()).await.unwrap();
    let restored = reopened.get(&key("run", id)).await.unwrap().unwrap();
    assert_eq!(
        restored["system_selection_plan"],
        run["system_selection_plan"]
    );
    assert_eq!(
        restored["system_failover_state"],
        run["system_failover_state"]
    );
    assert_eq!(restored["selection_plan"], standard);
    catalog = app.store.model_catalog().await.unwrap();
    catalog.connections.get_mut("two").unwrap().credential_ref = "rotated-system-credential".into();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    assert!(app
        .run_candidate(&system_dispatch_run(&restored).unwrap())
        .await
        .is_err());
}

#[tokio::test]
async fn model_selection_unsafe_failures_do_not_change_candidate() {
    let (_root, app) = fixture().await;
    let catalog = selection_fixture(&app).await;
    let plan = catalog.resolve("agent-a").unwrap();
    for (kind, started, uncertain) in [
        (model_selection::FailureKind::MalformedRequest, false, false),
        (model_selection::FailureKind::Cancelled, false, false),
        (model_selection::FailureKind::PolicyRejected, false, false),
        (model_selection::FailureKind::Transient, true, false),
        (model_selection::FailureKind::Transient, false, true),
    ] {
        let mut run = json!({"agent_id":"agent-a","selection_plan":plan,"failover_state":model_selection::FailoverState::default(),
            "provider_selection":{"provider":"openai_subscription","model":"model-two"},"uncertain":uncertain});
        model_failure(
            &mut run,
            &provider::typed_failure(kind, started, "Sanitized failure"),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert_eq!(run["failover_state"]["candidate_index"], 0);
        assert_eq!(run["provider_selection"]["model"], "model-two");
    }
}

#[tokio::test]
async fn model_catalog_api_uses_profile_admin_role_and_enforces_csrf_and_retries() {
    let (_root, app) = fixture().await;
    let token = uuid();
    app.store
        .put(
            &format!("auth:{}", digest(&json!(token))),
            &json!({"csrf":"catalog-csrf","expires":now()+3600,
        "generation":"fixture-generation","user_id":"admin"}),
        )
        .await
        .unwrap();
    let request = |method: &str, path: &str, cookie: bool, csrf: bool, body: Value| {
        let mut r = Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json");
        if cookie {
            r = r.header("cookie", format!("v3_session={token}"));
        }
        if csrf {
            r = r.header("x-csrf-token", "catalog-csrf");
        }
        r.body(Body::from(body.to_string())).unwrap()
    };
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            false,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            true,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    // Catalog compare-and-swap rejects a stale browser tab without replacing roles.
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap();
    let stale = body["catalog"].clone();
    let response = router(app.clone())
        .oneshot(request(
            "PUT",
            "/api/v3/models/catalog",
            true,
            true,
            stale.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let response = router(app.clone())
        .oneshot(request("PUT", "/api/v3/models/catalog", true, true, stale))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let new_agent = json!({"client_request_id":"catalog-agent","name":"Second Agent"});
    let response = router(app.clone())
        .oneshot(request(
            "POST",
            "/api/v3/models/agents",
            true,
            false,
            new_agent.clone(),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut created = Value::Null;
    for _ in 0..2 {
        let response = router(app.clone())
            .oneshot(request(
                "POST",
                "/api/v3/models/agents",
                true,
                true,
                new_agent.clone(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body: Value = serde_json::from_slice(
            &axum::body::to_bytes(response.into_body(), 65536)
                .await
                .unwrap(),
        )
        .unwrap();
        if created.is_null() {
            created = body.clone();
            // Creating initial ownership must preserve the creator's authenticated session.
            let path = format!(
                "/api/v3/models/catalog?agent_id={}",
                body["id"].as_str().unwrap()
            );
            let response = router(app.clone())
                .oneshot(request("GET", &path, true, false, Value::Null))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }
        assert_eq!(body, created);
    }
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents WHERE id=?")
        .bind(created["id"].as_str().unwrap())
        .fetch_one(
            &app.store
                .for_agent(created["id"].as_str().unwrap())
                .await
                .unwrap()
                .agent_pool,
        )
        .await
        .unwrap();
    assert_eq!(count, 1);
    let response = router(app.clone())
        .oneshot(request(
            "POST",
            "/api/v3/models/agents",
            true,
            true,
            json!({"client_request_id":"catalog-agent","name":"Different Agent"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    let mut profile = app.store.user_profile("admin").await.unwrap().unwrap();
    profile["is_admin"] = json!(false);
    sqlx::query("UPDATE user_profiles SET value=? WHERE user_id='admin'")
        .bind(app.store.seal(&profile).unwrap())
        .execute(&app.store.app_pool)
        .await
        .unwrap();
    let response = router(app.clone())
        .oneshot(request(
            "GET",
            "/api/v3/models/catalog",
            true,
            false,
            Value::Null,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn deepseek_pins_transport_and_mixed_provider_backup() {
    let (_root, app) = fixture().await;
    let mut catalog = selection_fixture(&app).await;
    catalog.connections.get_mut("two").unwrap().transport = model_selection::Transport::Deepseek;
    catalog.entries.get_mut("two").unwrap().upstream_model = "deepseek-chat".into();
    app.store
        .put("credential:two", &json!({"api_key":"test-key-no-network"}))
        .await
        .unwrap();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello"})),
    )
    .await
    .unwrap();
    let mut run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    assert_eq!(run["provider_selection"]["provider"], "deepseek");
    let selection =
        provider::ProviderSelection::parse(&run["provider_selection"], "unused").unwrap();
    assert_eq!(
        app.tokens_for(&selection, &run).await.unwrap()["api_key"],
        "test-key-no-network"
    );
    let wrong = provider::ProviderSelection {
        provider: provider::ProviderId::OpenaiSubscription,
        model: "deepseek-chat".into(),
    };
    assert!(app.tokens_for(&wrong, &run).await.is_err());
    model_failure(
        &mut run,
        &provider::typed_failure(
            model_selection::FailureKind::ModelUnavailable,
            false,
            "unavailable",
        ),
    )
    .unwrap();
    assert_eq!(run["provider_selection"]["provider"], "openai_subscription");
    assert_eq!(run["provider_selection"]["model"], "model-one");
}

#[tokio::test]
async fn platform_adoption_pins_independent_credentials_and_denies_rotation() {
    use crate::account_catalog::{platform_id, Adoption, Origin, Source};
    use crate::platform_catalog::{Entry, Mutation};
    use std::collections::{BTreeMap, BTreeSet};
    let (_root, app) = fixture().await;
    let user = app
        .store
        .create_user_account("platform_runner", "fixture-only-password-hash")
        .await
        .unwrap();
    let user = user["user_id"].as_str().unwrap();
    app.store
        .update_account_access(user, 1, access::AccountStatus::Active, "free")
        .await
        .unwrap();
    app.store
        .assign_agent_membership(
            "admin",
            user,
            "shared_default",
            access::Relationship::Member,
            true,
            0,
        )
        .await
        .unwrap();
    let entry = Entry {
        id: "model".into(),
        display_name: "Platform".into(),
        upstream_model: "fixture".into(),
        transport: model_selection::Transport::Deepseek,
        capabilities: BTreeSet::from(["text".into()]),
    };
    app.store
        .mutate_platform_roster(
            0,
            Mutation::Draft {
                entries: BTreeMap::from([("model".into(), entry)]),
            },
            "admin",
        )
        .await
        .unwrap();
    app.store
        .platform_credential_set("model", 1, Some("platform-fixture-key"), "admin")
        .await
        .unwrap();
    app.store
        .mutate_platform_roster(2, Mutation::Publish { rollback: None }, "admin")
        .await
        .unwrap();
    let before = app.store.model_catalog().await.unwrap();
    let adopted = app
        .store
        .adopt_byo_source(
            user,
            "shared_default",
            &Adoption {
                client_request_id: "platform_once".into(),
                expected_roster_revision: before.agent_models["shared_default"].revision,
                source: Source {
                    origin: Origin::Platform,
                    entry_id: platform_id("model"),
                },
                alias: "My catalog alias".into(),
            },
        )
        .await
        .unwrap();
    let mut catalog = app.store.model_catalog().await.unwrap();
    assert_eq!(
        catalog.agent_models["shared_default"].roles,
        before.agent_models["shared_default"].roles
    );
    assert!(app
        .store
        .account_model_preferences(user)
        .await
        .unwrap()
        .selected_platform_ids
        .contains(&platform_id("model")));
    let id = adopted["entry_id"].as_str().unwrap();
    assert_ne!(
        id,
        before.agent_models["shared_default"].roles[&model_selection::ModelRole::Standard].entry_id
    );
    catalog
        .agent_models
        .get_mut("shared_default")
        .unwrap()
        .roles
        .insert(
            model_selection::ModelRole::Standard,
            model_selection::RoleAssignment::new(id),
        );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let mut caller = app.clone();
    caller.store = app.store.for_user(user).await.unwrap();
    let mut plan = catalog
        .resolve_role("shared_default", model_selection::ModelRole::Standard)
        .unwrap();
    caller.pin_model_plan(&mut plan).await.unwrap();
    let run = json!({"agent_id":"shared_default","selection_plan":plan,"failover_state":model_selection::FailoverState::default()});
    let selection = provider::ProviderSelection {
        provider: provider::ProviderId::Deepseek,
        model: "fixture".into(),
    };
    assert_eq!(
        caller.tokens_for(&selection, &run).await.unwrap()["api_key"],
        "platform-fixture-key"
    );
    assert!(!serde_json::to_string(&run)
        .unwrap()
        .contains("platform-fixture-key"));
    app.store
        .platform_credential_set("model", 3, Some("rotated-fixture-key"), "admin")
        .await
        .unwrap();
    assert!(caller.tokens_for(&selection, &run).await.is_err());
    assert!(!caller.run_model_authorized(&run).await);
}
#[tokio::test]
async fn transient_retries_back_off_and_honor_retry_after() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    let wait = |run: &Value| run["provider_retry_at"].as_u64().unwrap() - crate::now();
    let error = provider::typed_failure(model_selection::FailureKind::Transient, false, "busy");
    model_failure(&mut run, &error).unwrap();
    assert!((1..=3).contains(&wait(&run)));
    model_failure(&mut run, &error).unwrap();
    assert!((7..=10).contains(&wait(&run)));
    let mut run = app.run(&id).await.unwrap();
    let asked = provider::typed_failure_after(
        model_selection::FailureKind::Transient,
        false,
        "busy",
        Some(30),
    );
    model_failure(&mut run, &asked).unwrap();
    assert!((29..=30).contains(&wait(&run)));
}

/// A bridge with no client online took nothing: with no alias left the run resumes by itself
/// once, then stops as resumable instead of failing.
#[tokio::test]
async fn offline_provider_resumes_once_then_stops_resumable() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    let error = webagent_models::transport::offline_failure("Bridge client offline");
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["auto_resumes"], 1);
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "stopped");
    assert_eq!(run["provider_recovery"]["safe_to_resume"], true);
    // The same failure kind from a provider that is running stays a plain failure.
    let mut run = app.run(&id).await.unwrap();
    let gone = provider::typed_failure(
        model_selection::FailureKind::ModelUnavailable,
        false,
        "model removed",
    );
    model_failure(&mut run, &gone).unwrap();
    assert_eq!(run["status"], "failed");
}

/// A session stays on the alias that answered; the next major reorder tries the first model.
#[tokio::test]
async fn backup_stick_starts_on_the_alias_and_a_reorder_returns_to_the_first_model() {
    use webagent_engine::runtime::failure::{apply_stick, first_model_again, stick_to_save};
    let (_root, app) = fixture().await;
    selection_fixture(&app).await;
    let Json(session) = create_session(
        State(app.clone()),
        Json(json!({"client_request_id":uuid(),"agent_id":"agent-a"})),
    )
    .await
    .unwrap();
    let (_, Json(accepted)) = send(
        State(app.clone()),
        Path(session["id"].as_str().unwrap().into()),
        Json(json!({"client_message_id":uuid(),"text":"hello","model_entry_id":"one"})),
    )
    .await
    .unwrap();
    let mut run = app.run(accepted["run_id"].as_str().unwrap()).await.unwrap();
    // Nothing to save or return from while the first model serves.
    assert!(stick_to_save(&mut run).is_none());
    assert!(!first_model_again(&mut run));
    assert!(!apply_stick(&mut run, &Value::Null));
    assert!(!apply_stick(&mut run, &json!({"from":"two","to":"one"})));
    assert!(apply_stick(&mut run, &json!({"from":"one","to":"two"})));
    assert_eq!(run["failover_state"]["candidate_index"], 1);
    assert_eq!(run["provider_selection"]["model"], "model-two");
    // The applied stick is already saved; a fresh switch is saved once.
    assert!(stick_to_save(&mut run).is_none());
    run["backup_stick"] = Value::Null;
    assert_eq!(
        stick_to_save(&mut run),
        Some(json!({"from":"one","to":"two"}))
    );
    assert!(stick_to_save(&mut run).is_none());
    assert!(first_model_again(&mut run));
    assert_eq!(run["failover_state"]["candidate_index"], 0);
    assert_eq!(run["provider_selection"]["model"], "model-one");
    // Once per run: a second reorder leaves a run that fell back again on its alias.
    run["failover_state"]["candidate_index"] = json!(1);
    assert!(!first_model_again(&mut run));
    let rows = run["_activity_items"].as_array().unwrap();
    assert_eq!(rows[0]["item"]["reason"], "backup_in_use");
    assert_eq!(rows[1]["item"]["reason"], "first_model_again");
}

#[tokio::test]
async fn provider_recovery_exhaustion_persists_and_explicit_resume_is_idempotent() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let mut run = app.run(&id).await.unwrap();
    let error = provider::typed_failure(
        model_selection::FailureKind::Transient,
        false,
        "Provider connection failed",
    );
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["failover_state"]["retries"], 1);
    app.save(&mut run).await.unwrap();
    assert!(!app
        .store
        .claim_run(&mut run, "other-loop", "too-early")
        .await
        .unwrap());
    // A second same-model retry follows the first, after a longer backoff.
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["failover_state"]["retries"], 2);
    // Retries spent: one automatic resume after the configured wait, with a fresh budget.
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "queued");
    assert_eq!(run["auto_resumes"], 1);
    assert_eq!(run["failover_state"]["retries"], 0);
    assert_eq!(run["attempt"], 0);
    assert!(run["provider_recovery"].is_null());
    let row = &run["_activity_items"].as_array().unwrap().last().unwrap()["item"];
    assert_eq!(
        (row["type"].as_str(), row["auto_resume"].as_bool()),
        (Some("retry_scheduled"), Some(true))
    );
    let settings = webagent_models::backup_policy::BackupPolicy::default();
    assert!(
        run["provider_retry_at"].as_u64().unwrap() + 1
            >= crate::now() + settings.auto_resume_after_seconds
    );
    for _ in 0..settings.same_model_retries {
        model_failure(&mut run, &error).unwrap();
        assert_eq!(run["status"], "queued");
    }
    model_failure(&mut run, &error).unwrap();
    assert_eq!(run["status"], "stopped");
    assert_eq!(run["provider_recovery"]["safe_to_resume"], true);
    assert_eq!(
        run["error"],
        webagent_engine::runtime::failure::MODEL_UNREACHABLE
    );
    // Each scheduled retry, plus the operator's "model unreachable" row.
    assert_eq!(run["retry_rows"], 2 * settings.same_model_retries + 2);
    app.save(&mut run).await.unwrap();
    let mut reopened = app.clone();
    reopened.store = Store::open(root.path()).await.unwrap();
    let restored = reopened.run(&id).await.unwrap();
    assert_eq!(restored["provider_recovery"], run["provider_recovery"]);
    recover(&reopened, true).await.unwrap();
    assert_eq!(reopened.run(&id).await.unwrap()["status"], "stopped");
    let body = json!({"client_request_id":"provider-resume"});
    command(
        State(reopened.clone()),
        Path((id.clone(), "resume".into())),
        Json(body.clone()),
    )
    .await
    .unwrap();
    let mut resumed = reopened.run(&id).await.unwrap();
    assert_eq!(resumed["failover_state"]["retries"], 0);
    assert_eq!(resumed["attempt"], 0);
    assert_eq!(resumed["selection_plan"], run["selection_plan"]);
    model_failure(&mut resumed, &error).unwrap();
    reopened.save(&mut resumed).await.unwrap();
    command(
        State(reopened.clone()),
        Path((id.clone(), "resume".into())),
        Json(body),
    )
    .await
    .unwrap();
    assert_eq!(
        reopened.run(&id).await.unwrap()["failover_state"]["retries"],
        1
    );
    control(
        State(reopened.clone()),
        Json(json!({"client_request_id":uuid(),"shutoff":true})),
    )
    .await
    .unwrap();
    assert!(command(
        State(reopened.clone()),
        Path((id, "resume".into())),
        Json(json!({"client_request_id":uuid()}))
    )
    .await
    .is_err());
}

#[tokio::test]
async fn provider_recovery_never_marks_partial_or_uncertain_or_contract_failures_safe() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    let baseline = app.run(&id).await.unwrap();
    for kind in [
        model_selection::FailureKind::MalformedRequest,
        model_selection::FailureKind::Cancelled,
        model_selection::FailureKind::PolicyRejected,
        model_selection::FailureKind::Protocol,
    ] {
        let mut run = baseline.clone();
        model_failure(
            &mut run,
            &provider::typed_failure(kind, false, "fixture failure"),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert!(run["provider_recovery"].is_null());
    }
    for (started, uncertain) in [(true, false), (false, true)] {
        let mut run = baseline.clone();
        run["uncertain"] = json!(uncertain);
        model_failure(
            &mut run,
            &provider::typed_failure(
                model_selection::FailureKind::Transient,
                started,
                "fixture failure",
            ),
        )
        .unwrap();
        assert_eq!(run["status"], "failed");
        assert!(run["provider_recovery"].is_null());
    }
    let mut run = baseline.clone();
    let chat_state = run["failover_state"].clone();
    let error = provider::typed_failure(
        model_selection::FailureKind::Transient,
        false,
        "summary connection failure",
    );
    // Two rounds of retries with one automatic resume between them, then the stop.
    for _ in 0..2 * (webagent_models::backup_policy::BackupPolicy::default().same_model_retries + 1)
    {
        system_model_failure(&mut run, &error).unwrap();
    }
    assert_eq!(run["status"], "stopped");
    assert_eq!(run["provider_recovery"]["system"], true);
    app.save(&mut run).await.unwrap();
    command(
        State(app.clone()),
        Path((id.clone(), "resume".into())),
        Json(json!({"client_request_id":uuid()})),
    )
    .await
    .unwrap();
    let resumed = app.run(&id).await.unwrap();
    assert_eq!(resumed["system_failover_state"]["retries"], 0);
    assert_eq!(resumed["failover_state"], chat_state);
}

#[tokio::test]
async fn provider_recovery_retries_stream_failure_and_publishes_one_final_reply() {
    use axum::{routing::post, Router};
    use std::sync::atomic::AtomicUsize;
    let (_root, mut app) = fixture().await;
    let count = Arc::new(AtomicUsize::new(0));
    let seen = count.clone();
    let mock = Router::new().route("/responses", post(move || {
        let seen = seen.clone();
        async move {
            let event = if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                json!({"type":"response.failed","response":{"status":"failed","error":{"code":"upstream_connection_failed","message":"private provider details"}}})
            } else {
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Recovered reply"}]}]}})
            };
            ([("content-type","text/event-stream")], format!("data: {event}\n\n"))
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, mock).await.unwrap();
    });
    app.provider = provider::Provider::for_test(format!("http://{address}/responses"));
    let sid = conversation(&app).await;
    let id = queued(&app, &sid).await;
    running(&app, &id, "first-attempt").await;
    execute_run(
        app.clone(),
        id.clone(),
        "first-attempt".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let retry = app.run(&id).await.unwrap();
    assert_eq!(retry["status"], "queued");
    assert_eq!(retry["failover_state"]["retries"], 1);
    assert!(!retry["error"]
        .as_str()
        .unwrap()
        .contains("private provider details"));
    // execute_run's existing bounded delay has elapsed; the durable claim still
    // enforces cooldown independently of that process-local sleep.
    running(&app, &id, "second-attempt").await;
    execute_run(
        app.clone(),
        id.clone(),
        "second-attempt".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(app.run(&id).await.unwrap()["status"], "completed");
    assert_eq!(count.load(Ordering::SeqCst), 2);
    let (chat, _) = app.store.chat_snapshot(&sid).await.unwrap();
    assert_eq!(
        chat.iter().filter(|row| row["role"] == "assistant").count(),
        1
    );
    server.abort();
}
