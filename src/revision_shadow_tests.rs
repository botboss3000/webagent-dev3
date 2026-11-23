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
    Baseline {
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
    moved(check(&app, &baseline).await, "session", "session mode");

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
        "session",
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
        baseline.vector.moved(&now).contains(&"session"),
        "session status"
    );
    session["status"] = json!("active");
    app.store.put(&key, &session).await.unwrap();

    // Shared participant: another account joins the session.
    let baseline = step(&app, &sid, &member).await;
    app.store.shared_join(&sid, &member).await.unwrap();
    moved(
        check(&app, &baseline).await,
        "session",
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
    moved(check(&app, &baseline).await, "session", "missing revision");
}

#[tokio::test]
async fn the_access_policy_file_moves_the_vector() {
    let (root, app) = fixture().await;
    let sid = conversation(&app).await;
    let path = webagent_access::policy_file::policy_path(root.path());
    // `live_access` reads this file, so the fixture has it.
    let text = std::fs::read_to_string(&path).expect("fixture access policy file");
    let baseline = step(&app, &sid, "admin").await;
    let mut policy: Value = serde_json::from_str(&text).unwrap();
    policy["revision"] = json!(policy["revision"].as_u64().unwrap_or(0) + 1);
    std::fs::write(&path, serde_json::to_vec(&policy).unwrap()).unwrap();
    moved(check(&app, &baseline).await, "policy", "access policy");
}
