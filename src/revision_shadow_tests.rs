//! Stream B4 shadow: each record family an access decision reads moves the revision vector
//! when it is changed from another account or handle, and the shadow answer then changes
//! ("rebuild") on the next check with no disagreement. A missing revision reads as changed.
use super::*;
use crate::runtime_tests::{conversation, fixture};
use webagent_engine::runtime::revision_shadow::{self as shadow, Baseline, Observed, Outcome};

const AGENT: &str = "shared_default";

/// A step's order: the vector first, then the access inputs (`CallContext`).
async fn step(app: &App, sid: &str, requester: &str) -> Baseline {
    let engine = app.engine();
    let vector = engine.revision_vector(requester, sid, AGENT).await;
    let access = engine
        .live_access_in(requester, sid, AGENT, None)
        .await
        .unwrap();
    let context = engine
        .call_context(sid, AGENT, requester, &access)
        .await
        .expect("call context");
    let caller = shadow::caller_class(&context.access, vector.participant);
    Baseline {
        caller,
        candidate: None,
        vector,
        access: shadow::access_part(
            &context.access,
            context.mode_revision,
            &context.visibility,
            &context.ability_state,
            &context.mode,
        ),
        mode_revision: context.mode_revision,
        run_id: "fixture-run".into(),
        generation: "fixture-generation".into(),
        session_id: sid.into(),
        agent_id: AGENT.into(),
        requester: requester.into(),
    }
}

/// A check's order: today's full recomputation first, then the vector.
async fn check(app: &App, baseline: &Baseline) -> Outcome {
    let engine = app.engine();
    let sid = &baseline.session_id;
    let requester = &baseline.requester;
    let (mode_revision, visibility, state) =
        engine.ability_controls_for(sid, requester).await.unwrap();
    let access = engine
        .live_access_in(requester, sid, AGENT, None)
        .await
        .unwrap();
    let mode = engine.session_mode(sid).await.unwrap();
    let observed = Observed {
        model_authorized: None,
        mode_revision: Some(mode_revision),
        access: Some(shadow::access_part(
            &access,
            mode_revision,
            &visibility,
            &state,
            &mode,
        )),
    };
    let now = engine.revision_vector(requester, sid, AGENT).await;
    shadow::compare(baseline, &now, &observed)
}

/// Asserts the change moved `part` and was not a disagreement.
fn moved(outcome: Outcome, part: &str, family: &str) {
    match outcome {
        Outcome::Changed { moved, .. } => {
            assert!(
                moved.contains(&part),
                "{family}: moved {moved:?}, expected {part}"
            )
        }
        other => panic!("{family}: expected a moved revision, got {other:?}"),
    }
}

#[tokio::test]
async fn each_access_record_family_moves_the_vector_without_a_disagreement() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    // Nothing changed: the context stands.
    let baseline = step(&app, &sid, "admin").await;
    assert_eq!(check(&app, &baseline).await, Outcome::Unchanged);

    // Account (the caller's `auth_epoch`), from another handle on the account store.
    let baseline = step(&app, &sid, "admin").await;
    let other = app.store.for_user("admin").await.unwrap();
    other
        .accounts_repository()
        .update_user_profile("admin", "admin-renamed", "Administrator")
        .await
        .unwrap();
    moved(check(&app, &baseline).await, "accounts", "account");

    // Agent metadata: tool permissions written through the access repository.
    let baseline = step(&app, &sid, "admin").await;
    let revision = app.store.agent_tool_permissions(AGENT).await.unwrap()["revision"]
        .as_u64()
        .unwrap();
    app.store
        .set_agent_tool_permissions(
            AGENT,
            revision,
            [("get_time".to_owned(), crate::access::Permission::Deny)].into(),
        )
        .await
        .unwrap();
    moved(check(&app, &baseline).await, "agent", "agent metadata");

    // Membership: another account joins the agent.
    let baseline = step(&app, &sid, "admin").await;
    let member = app
        .store
        .create_user_account("shadow-member", "long-hash-placeholder")
        .await
        .unwrap()["user_id"]
        .as_str()
        .unwrap()
        .to_owned();
    app.store
        .set_agent_membership(&member, AGENT, crate::access::Relationship::Member, 0)
        .await
        .unwrap();
    moved(check(&app, &baseline).await, "agent", "membership");

    // Ability toggle (agent ability settings) through the administrator handler.
    let baseline = step(&app, &sid, "admin").await;
    let abilities = app.store.agent_abilities().await.unwrap();
    let ability = app
        .abilities
        .all_schemas()
        .iter()
        .find_map(|schema| schema["ability_id"].as_str().map(str::to_owned));
    if let Some(ability) = ability {
        agent_ability_visibility_set(
            State(app.clone()),
            Path((AGENT.into(), ability)),
            Json(AbilityVisibilityBody {
                visibility: "off".into(),
                expected_revision: abilities["revision"].as_u64().unwrap_or(0),
            }),
        )
        .await
        .unwrap();
    } else {
        // No ability in this build's catalog: toggle the stored ability settings directly.
        let mut value = abilities.clone();
        value["visibility"]["shadow_fixture"] = json!("off");
        app.store
            .save_agent_abilities(&value, abilities["revision"].as_u64().unwrap_or(0))
            .await
            .unwrap();
    }
    moved(check(&app, &baseline).await, "agent", "ability toggle");

    // Session mode, through the session handler.
    let baseline = step(&app, &sid, "admin").await;
    session_mode_set(
        State(app.clone()),
        Path(sid.clone()),
        Json(SessionModeBody {
            mode: "plan".into(),
            expected_revision: Some(baseline.mode_revision),
            confirm_write: true,
        }),
    )
    .await
    .unwrap();
    moved(check(&app, &baseline).await, "session_mode", "session mode");

    // Session ability state.
    let baseline = step(&app, &sid, "admin").await;
    let mut state = app.engine().session_ability_state(&sid).await.unwrap();
    state["revision"] = json!(state["revision"].as_u64().unwrap_or(0) + 1);
    app.store
        .put(&format!("session_ability_state:{sid}"), &state)
        .await
        .unwrap();
    moved(
        check(&app, &baseline).await,
        "session_abilities",
        "session ability state",
    );

    // Session record: an activity-only write leaves the header (and the context) standing;
    // a status change moves it.
    let baseline = step(&app, &sid, "admin").await;
    let key = format!("session:{sid}");
    let mut session = app.store.get(&key).await.unwrap().unwrap();
    session["title"] = json!("Renamed for the shadow test");
    app.store.put(&key, &session).await.unwrap();
    assert_eq!(
        check(&app, &baseline).await,
        Outcome::Unchanged,
        "title only"
    );
    session["status"] = json!("archived");
    app.store.put(&key, &session).await.unwrap();
    // An archived session no longer resolves, so today's recomputation refuses; the vector
    // must have moved too.
    let now = app.engine().revision_vector("admin", &sid, AGENT).await;
    assert!(
        baseline.vector.moved(&now).contains(&"header"),
        "session status"
    );
    session["status"] = json!("active");
    app.store.put(&key, &session).await.unwrap();

    // Shared participant: another account joins the session.
    let baseline = step(&app, &sid, &member).await;
    assert_eq!(baseline.caller, "member");
    app.store.shared_join(&sid, &member).await.unwrap();
    moved(
        check(&app, &baseline).await,
        "participant",
        "shared participant",
    );

    // Model catalog.
    let baseline = step(&app, &sid, "admin").await;
    let catalog = app.store.model_catalog().await.unwrap();
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    moved(
        check(&app, &baseline).await,
        "model_catalog",
        "model catalog",
    );

    // A missing revision reads as changed.
    let baseline = step(&app, &sid, "admin").await;
    sqlx::query("DELETE FROM session_revisions WHERE session_id=?")
        .bind(&sid)
        .execute(&app.store.pool)
        .await
        .unwrap();
    moved(
        check(&app, &baseline).await,
        "session_mode",
        "missing revision",
    );
}

#[tokio::test]
async fn the_caller_class_names_the_administrator() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    assert_eq!(step(&app, &sid, "admin").await.caller, "administrator");
}

#[tokio::test]
async fn an_account_re_created_with_the_same_id_and_epoch_reads_as_moved() {
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let baseline = step(&app, &sid, "admin").await;
    // No code path deletes an account row; a restore or a hand edit can. The row comes back
    // byte for byte, `auth_epoch` included, inside one transaction (other rows refer to it).
    let pool = &app.store.app_pool;
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("PRAGMA defer_foreign_keys=ON")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("CREATE TEMP TABLE kept AS SELECT * FROM user_accounts WHERE user_id='admin'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DELETE FROM user_accounts WHERE user_id='admin'")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_accounts SELECT * FROM kept")
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("DROP TABLE kept")
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let now = app.engine().revision_vector("admin", &sid, AGENT).await;
    let (before, after) = (
        baseline.vector.principals.clone().unwrap(),
        now.principals.clone().unwrap(),
    );
    assert_eq!(before.caller_epoch, after.caller_epoch);
    assert_ne!(before.caller_instance, after.caller_instance);
    // Today's recomputation finds the same account, so the move has no effect.
    match check(&app, &baseline).await {
        Outcome::Changed { moved, effect } => {
            assert_eq!(moved, vec!["accounts"]);
            assert!(!effect);
        }
        other => panic!("expected a moved account, got {other:?}"),
    }
}

#[tokio::test]
async fn a_platform_catalog_change_moves_the_vector_and_a_rotation_changes_the_grant() {
    use crate::account_catalog::{platform_id, Adoption, Origin, Source};
    use crate::platform_catalog::{Entry, Mutation};
    use std::collections::{BTreeMap, BTreeSet};
    let (_root, app) = fixture().await;
    let sid = conversation(&app).await;
    let user = app
        .store
        .create_user_account("platform_shadow", "fixture-only-password-hash")
        .await
        .unwrap();
    let user = user["user_id"].as_str().unwrap().to_owned();
    app.store
        .update_account_access(&user, 1, access::AccountStatus::Active, "free")
        .await
        .unwrap();
    app.store
        .assign_agent_membership("admin", &user, AGENT, access::Relationship::Member, true, 0)
        .await
        .unwrap();
    let entry = Entry {
        id: "model".into(),
        display_name: "Platform".into(),
        upstream_model: "fixture".into(),
        transport: crate::model_selection::Transport::Deepseek,
        capabilities: BTreeSet::from(["text".into()]),
    };
    let entries = BTreeMap::from([("model".to_owned(), entry)]);
    app.store
        .mutate_platform_roster(
            0,
            Mutation::Draft {
                entries: entries.clone(),
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
            &user,
            AGENT,
            &Adoption {
                client_request_id: "platform_shadow".into(),
                expected_roster_revision: before.agent_models[AGENT].revision,
                source: Source {
                    origin: Origin::Platform,
                    entry_id: platform_id("model"),
                },
                alias: "Shadow platform alias".into(),
            },
        )
        .await
        .unwrap();
    let mut catalog = app.store.model_catalog().await.unwrap();
    let id = adopted["entry_id"].as_str().unwrap();
    catalog.agent_models.get_mut(AGENT).unwrap().roles.insert(
        crate::model_selection::ModelRole::Standard,
        crate::model_selection::RoleAssignment::new(id),
    );
    app.store
        .save_model_catalog(&catalog, catalog.revision)
        .await
        .unwrap();
    let mut caller = app.clone();
    caller.store = app.store.for_user(&user).await.unwrap();
    let mut plan = catalog
        .resolve_role(AGENT, crate::model_selection::ModelRole::Standard)
        .unwrap();
    caller.pin_model_plan(&mut plan).await.unwrap();
    let run = json!({"agent_id":AGENT,"requester_user_id":&user,
        "selection_plan":plan,"failover_state":crate::model_selection::FailoverState::default()});
    assert!(caller.run_model_authorized(&run).await);
    let start = platform_baseline(&app, &sid, &user).await;
    assert_eq!(
        platform_check(&app, &caller, &run, &start).await,
        Outcome::Unchanged
    );
    // A roster write that withdraws nothing (a draft edit) moves the platform part only.
    let start = platform_baseline(&app, &sid, &user).await;
    app.store
        .mutate_platform_roster(3, Mutation::Draft { entries }, "admin")
        .await
        .unwrap();
    assert_eq!(
        platform_check(&app, &caller, &run, &start).await,
        Outcome::Changed {
            moved: vec!["platform"],
            effect: false
        }
    );
    // A rotated key refuses the pinned platform model: the vector moved with it, so the
    // shadow answer is "rebuild", never a disagreement.
    let start = platform_baseline(&app, &sid, &user).await;
    app.store
        .platform_credential_set("model", 4, Some("rotated-fixture-key"), "admin")
        .await
        .unwrap();
    assert_eq!(
        platform_check(&app, &caller, &run, &start).await,
        Outcome::Changed {
            moved: vec!["platform"],
            effect: true
        }
    );
}

/// A step baseline for `user`'s platform run (the grant was true at the step top). The vector
/// is read through the session owner's store, as the loop reads it for the run's session.
async fn platform_baseline(app: &App, sid: &str, user: &str) -> Baseline {
    let engine = app.engine();
    Baseline {
        vector: engine.revision_vector(user, sid, AGENT).await,
        access: String::new(),
        mode_revision: 0,
        run_id: "platform-run".into(),
        generation: "platform-generation".into(),
        session_id: sid.into(),
        agent_id: AGENT.into(),
        requester: user.into(),
        caller: "member",
        candidate: Some(0),
    }
}

/// The fence's order: the grant is recomputed first (as the caller), then the vector is read.
async fn platform_check(app: &App, caller: &App, run: &Value, baseline: &Baseline) -> Outcome {
    let observed = Observed {
        model_authorized: Some(caller.run_model_authorized(run).await),
        ..Observed::default()
    };
    let engine = app.engine();
    let now = engine
        .revision_vector(&baseline.requester, &baseline.session_id, AGENT)
        .await;
    shadow::compare(baseline, &now, &observed)
}
