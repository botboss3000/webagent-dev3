use super::*;

fn vector() -> RevisionVector {
    RevisionVector {
        principals: Some(Principals {
            owner_id: "owner".into(),
            caller_epoch: 1,
            owner_epoch: 2,
        }),
        agent: Some(3),
        session: Some(SessionRevisions {
            record: 4,
            mode: 5,
            abilities: 6,
        }),
        model_catalog: Some(7),
        policy: Some("digest".into()),
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
    let cases: [(&str, fn(&mut RevisionVector)); 6] = [
        ("accounts", |v| {
            v.principals.as_mut().unwrap().caller_epoch += 1
        }),
        ("agent", |v| v.agent = Some(4)),
        ("session", |v| v.session.as_mut().unwrap().mode += 1),
        ("model_catalog", |v| v.model_catalog = None),
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
    now.agent = Some(99);
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
    moved.agent = Some(4);
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
}

#[test]
fn baselines_answer_only_their_own_generation() {
    remember(baseline());
    assert!(super::baseline("run", "generation").is_some());
    assert!(super::baseline("run", "older").is_none());
    assert!(super::baseline("other-run", "generation").is_none());
}
