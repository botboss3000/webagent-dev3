use super::*;

fn vector() -> RevisionVector {
    RevisionVector {
        principals: Some(Principals {
            owner_id: "owner".into(),
            caller_epoch: 1,
            owner_epoch: 2,
            caller_instance: 11,
            owner_instance: 12,
        }),
        agent: Some(agent(3)),
        session: Some(SessionRevisions {
            record: 4,
            mode: 5,
            abilities: 6,
        }),
        header: Some(SessionHeader {
            status: Some("active".into()),
            agent_id: Some("agent".into()),
            participation: "{}".into(),
        }),
        participant: Some(false),
        model_catalog: Some(7),
        platform: Some(8),
        policy: Some("digest".into()),
    }
}

fn agent(epoch: i64) -> AgentRevision {
    AgentRevision {
        instance: "instance".into(),
        epoch,
    }
}

fn baseline() -> Baseline {
    Baseline {
        vector: vector(),
        access: "access".into(),
        mode_revision: 9,
        run_id: "run".into(),
        generation: "generation".into(),
        session_id: "session".into(),
        agent_id: "agent".into(),
        requester: "user".into(),
        caller: "member",
        candidate: Some(0),
    }
}

fn agreeing() -> Observed {
    Observed {
        model_authorized: Some(true),
        mode_revision: Some(9),
        access: Some("access".into()),
    }
}

#[test]
fn an_unchanged_vector_with_equal_inputs_stands() {
    assert_eq!(
        compare(&baseline(), &vector(), &agreeing()),
        Outcome::Unchanged
    );
    // Parts the check did not reach are not compared.
    assert_eq!(
        compare(&baseline(), &vector(), &Observed::default()),
        Outcome::Unchanged
    );
}

#[test]
fn each_moved_part_is_named_and_a_missing_value_counts_as_moved() {
    // The session record counter alone moves nothing: the header stands for the record.
    let mut now = vector();
    now.session.as_mut().unwrap().record += 5;
    assert_eq!(compare(&baseline(), &now, &agreeing()), Outcome::Unchanged);
    let cases: [(&str, fn(&mut RevisionVector)); 15] = [
        ("header", |v| {
            v.header.as_mut().unwrap().status = Some("archived".into())
        }),
        ("participant", |v| v.participant = Some(true)),
        ("header", |v| v.header = None),
        ("accounts", |v| {
            v.principals.as_mut().unwrap().caller_epoch += 1
        }),
        // A re-created account or agent: same counter, new instance.
        ("accounts", |v| {
            v.principals.as_mut().unwrap().caller_instance += 1
        }),
        ("accounts", |v| {
            v.principals.as_mut().unwrap().owner_instance += 1
        }),
        ("agent", |v| v.agent = Some(agent(4))),
        ("agent", |v| {
            v.agent.as_mut().unwrap().instance = "other".into()
        }),
        ("session_mode", |v| v.session.as_mut().unwrap().mode += 1),
        ("session_abilities", |v| {
            v.session.as_mut().unwrap().abilities += 1
        }),
        ("model_catalog", |v| v.model_catalog = None),
        ("platform", |v| v.platform = Some(9)),
        ("platform", |v| v.platform = None),
        ("policy", |v| v.policy = Some("other".into())),
        ("accounts", |v| v.principals = None),
    ];
    for (part, change) in cases {
        let mut now = vector();
        change(&mut now);
        assert_eq!(
            compare(&baseline(), &now, &agreeing()),
            Outcome::Changed {
                moved: vec![part],
                effect: false
            }
        );
    }
    // Missing on both sides is still moved: nothing unreadable ever stands.
    let mut before = baseline();
    before.vector.agent = None;
    let mut now = vector();
    now.agent = None;
    assert!(matches!(
        compare(&before, &now, &agreeing()),
        Outcome::Changed { .. }
    ));
}

#[test]
fn a_changed_input_under_an_unchanged_vector_is_a_disagreement() {
    let observed = Observed {
        model_authorized: Some(false),
        mode_revision: Some(10),
        access: Some("other".into()),
    };
    assert_eq!(
        compare(&baseline(), &vector(), &observed),
        Outcome::Disagreement {
            parts: vec!["model", "mode", "access"]
        }
    );
    let mut now = vector();
    now.agent = Some(agent(99));
    assert_eq!(
        compare(&baseline(), &now, &observed),
        Outcome::Changed {
            moved: vec!["agent"],
            effect: true
        }
    );
}

#[test]
fn record_counts_each_outcome_and_keeps_a_content_free_example() {
    let before = COUNTS[Kind::ToolCheck.index()].values();
    record(Kind::ToolCheck, &baseline(), &vector(), &agreeing());
    let mut moved = vector();
    moved.agent = Some(agent(4));
    record(Kind::ToolCheck, &baseline(), &moved, &agreeing());
    let wrong = Observed {
        access: Some("other".into()),
        ..agreeing()
    };
    record(Kind::ToolCheck, &baseline(), &vector(), &wrong);
    let after = COUNTS[Kind::ToolCheck.index()].values();
    // Other tests may count in parallel: at least these three.
    assert!(after[0] >= before[0] + 3);
    assert!(after[1] > before[1] && after[2] > before[2] && after[3] > before[3]);
    assert!(after[4] > before[4]);
    let example = PENDING_EXAMPLE.lock().unwrap()[Kind::ToolCheck.index()]
        .clone()
        .unwrap();
    assert_eq!(example["parts"], json!(["access"]));
    assert_eq!(example["run_id"], "run");
    let text = example.to_string();
    assert!(
        !text.contains("\"access\":\"access\""),
        "no access digest copied: {text}"
    );
}

#[tokio::test]
async fn flush_adds_only_new_counts_and_reports_both_kinds() {
    let root = tempfile::tempdir().unwrap();
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(root.path().join("app.db"))
                .create_if_missing(true),
        )
        .await
        .unwrap();
    let empty = report(&pool).await;
    assert_eq!(empty["kinds"]["fence"]["checks"], 0);
    assert_eq!(empty["kinds"]["tool_check"]["disagreements"], 0);
    record(Kind::Fence, &baseline(), &vector(), &agreeing());
    write(&pool).await.unwrap();
    let first = report(&pool).await["kinds"]["fence"]["checks"]
        .as_i64()
        .unwrap();
    assert!(first >= 1);
    // A second write without new checks adds nothing.
    let counted = COUNTS[Kind::Fence.index()].values()[0] as i64;
    write(&pool).await.unwrap();
    assert_eq!(
        report(&pool).await["kinds"]["fence"]["checks"]
            .as_i64()
            .unwrap(),
        counted
    );
    assert_eq!(report(&pool).await["decides"], "full recomputation");
    // Per-part and per-caller counts (no other test records this kind or writes).
    let mut moved = vector();
    moved.platform = Some(9);
    let refused = Observed {
        model_authorized: Some(false),
        ..Observed::default()
    };
    record(Kind::ModelReturn, &baseline(), &moved, &refused);
    record(Kind::ModelReturn, &baseline(), &vector(), &agreeing());
    write(&pool).await.unwrap();
    let report = report(&pool).await;
    let kind = &report["kinds"]["model_return"];
    assert_eq!(kind["checks"], 2);
    assert_eq!(kind["parts"]["platform"]["changed"], 1);
    assert_eq!(kind["parts"]["platform"]["changed_with_effect"], 1);
    assert_eq!(kind["parts"]["agent"]["changed"], 0);
    assert_eq!(kind["callers"]["member"]["checks"], 2);
    assert_eq!(kind["callers"]["member"]["changed_with_effect"], 1);
    assert_eq!(kind["callers"]["guest"]["checks"], 0);
    for kind in KINDS {
        assert!(report["kinds"][kind.name()]["parts"]["candidate"].is_object());
    }
}

#[test]
fn a_moved_candidate_is_a_changed_subject_not_a_disagreement() {
    let refused = Observed {
        model_authorized: Some(false),
        ..Observed::default()
    };
    // Failover moved the run to another candidate after the step top.
    assert_eq!(
        compare_at(&baseline(), &vector(), &refused, Some(1)),
        Outcome::Changed {
            moved: vec!["candidate"],
            effect: true
        }
    );
    // The same candidate, or none known, is compared as before.
    assert_eq!(
        compare_at(&baseline(), &vector(), &agreeing(), Some(0)),
        Outcome::Unchanged
    );
    assert_eq!(
        compare_at(&baseline(), &vector(), &refused, None),
        Outcome::Disagreement {
            parts: vec!["model"]
        }
    );
}

#[test]
fn detail_counts_each_moved_part_and_the_caller_class() {
    let mut detail = Detail::new();
    let mut now = vector();
    now.platform = Some(9);
    now.session.as_mut().unwrap().mode += 1;
    add_detail(
        &mut detail,
        Kind::Fence,
        "owner",
        &compare(&baseline(), &now, &agreeing()),
    );
    add_detail(
        &mut detail,
        Kind::Fence,
        "owner",
        &Outcome::Disagreement {
            parts: vec!["access"],
        },
    );
    add_detail(&mut detail, Kind::Fence, "owner", &Outcome::Unchanged);
    assert_eq!(detail[&(0, "caller", "owner")], [3, 1, 0, 1]);
    assert_eq!(detail[&(0, "part", "platform")], [0, 1, 0, 0]);
    assert_eq!(detail[&(0, "part", "session_mode")], [0, 1, 0, 0]);
    assert!(!detail.contains_key(&(0, "part", "agent")));
}

#[test]
fn baselines_answer_only_their_own_generation() {
    remember(baseline());
    assert!(super::baseline("run", "generation").is_some());
    assert!(super::baseline("run", "older").is_none());
    assert!(super::baseline("other-run", "generation").is_none());
}
