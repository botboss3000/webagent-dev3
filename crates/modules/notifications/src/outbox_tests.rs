// The workspace has no clippy.toml with `allow-unwrap-in-tests` yet.
#![allow(clippy::unwrap_used)]

use super::*;

/// A user plane and an application plane under one temporary root.
struct Planes {
    _root: tempfile::TempDir,
    root: std::path::PathBuf,
    app: SqlitePool,
    user: SqlitePool,
}

const SOURCE: Source<'static> = Source {
    user_id: "admin",
    agent_id: "shared_default",
};

async fn open(root: &std::path::Path) -> (SqlitePool, SqlitePool) {
    let app = webagent_storage::pool::open_pool(&root.join("app.db"), true, false)
        .await
        .unwrap();
    let user = webagent_storage::pool::open_pool(&root.join("user.db"), true, true)
        .await
        .unwrap();
    schema(&app, Plane::Application).await.unwrap();
    schema(&user, Plane::User).await.unwrap();
    (app, user)
}

async fn planes() -> Planes {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let (app, user) = open(&root).await;
    // Session records are plain JSON here; the host decodes sealed ones.
    sqlx::query("INSERT INTO docs(key,value) VALUES('session:s',?)")
        .bind(br#"{"id":"s","agent_id":"session_agent"}"#.to_vec())
        .execute(&user)
        .await
        .unwrap();
    Planes {
        _root: dir,
        root,
        app,
        user,
    }
}

fn plain(raw: &[u8]) -> Result<Value> {
    Ok(serde_json::from_slice(raw)?)
}

fn notice<'a>(event_key: &'a str, kind: &'a str) -> Notice<'a> {
    Notice {
        event_key,
        kind,
        session_id: "s",
        run_id: Some("r"),
        status: None,
    }
}

async fn stage_once(planes: &Planes, notice: &Notice<'_>) -> Result<()> {
    let mut tx = planes.user.begin().await?;
    stage(&mut tx, &SOURCE, notice, plain).await?;
    tx.commit().await?;
    Ok(())
}

async fn stage_run_once(planes: &Planes, run: &Value) {
    let mut tx = planes.user.begin().await.unwrap();
    stage_run(&mut tx, &SOURCE, run, plain).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn stopped_and_cancelled_are_committed_once_with_no_payload_text() {
    let planes = planes().await;
    for status in ["stopped", "cancelled"] {
        let run = json!({"id":"settled","session_id":"s","status":status,"error":"private","input":"private"});
        let mut tx = planes.user.begin().await.unwrap();
        stage_run(&mut tx, &SOURCE, &run, plain).await.unwrap();
        tx.rollback().await.unwrap();
        for _ in 0..2 {
            stage_run_once(&planes, &run).await;
        }
    }
    relay(&planes.app, &planes.user, "admin").await.unwrap();
    let page = page(&planes.app, 0, 20).await.unwrap();
    let rows = page["events"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["type"], "run.stopped");
    assert_eq!(rows[1]["type"], "run.cancelled");
    assert_eq!(rows[0]["actor"], "runtime");
    assert!(!page.to_string().contains("private"));
}

#[tokio::test]
async fn transactional_relay_deduplication_and_redaction() {
    let planes = planes().await;
    let mut tx = planes.user.begin().await.unwrap();
    stage(
        &mut tx,
        &SOURCE,
        &notice("message:r", "message.received"),
        plain,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(relay(&planes.app, &planes.user, "admin").await.unwrap(), 0);
    for _ in 0..2 {
        stage_once(&planes, &notice("message:r", "message.received"))
            .await
            .unwrap();
    }
    assert_eq!(relay(&planes.app, &planes.user, "admin").await.unwrap(), 1);
    let page = page(&planes.app, 0, 200).await.unwrap();
    let event = &page["events"][0];
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    assert_eq!(event["type"], "message.received");
    assert_eq!(event["event_id"], "admin:message:r");
    assert_eq!(event["agent_id"], "session_agent");
    assert_eq!(event["actor"], "admin");
    assert_eq!(event["payload"], json!({"schema_version":1,"status":null}));
    // Simulate the crash window after canonical commit and before source acknowledgement.
    sqlx::query("INSERT INTO notification_stage VALUES(1,'admin:message:r','message.received','s','r','shared_default','admin','{}',0)")
        .execute(&planes.user).await.unwrap();
    assert_eq!(relay(&planes.app, &planes.user, "admin").await.unwrap(), 0);
}

#[tokio::test]
async fn retention_reopen_and_no_resurrection() {
    let planes = planes().await;
    stage_once(&planes, &notice("created:s", "session.created"))
        .await
        .unwrap();
    relay(&planes.app, &planes.user, "admin").await.unwrap();
    assert_eq!(
        prune(&planes.app, now() as i64 + RETENTION_SECONDS + 1)
            .await
            .unwrap(),
        1
    );
    let (app, user) = open(&planes.root).await;
    assert_eq!(page(&app, 0, 20).await.unwrap()["error"], "cursor_expired");
    assert!(page(&app, 1, 20).await.unwrap()["events"]
        .as_array()
        .unwrap()
        .is_empty());
    sqlx::query("INSERT INTO notification_stage VALUES(1,'admin:created:s','session.created','s',NULL,'shared_default','admin','{}',0)")
        .execute(&user).await.unwrap();
    assert_eq!(relay(&app, &user, "admin").await.unwrap(), 0);
    assert_eq!(page(&app, 1, 20).await.unwrap()["latest_seq"], 1);
}

#[tokio::test]
async fn approval_checkpoint_and_terminal_deduplication() {
    let planes = planes().await;
    let mut run = json!({"id":"r","session_id":"s","status":"waiting_approval","pending":{"id":"approval-1","arguments":{"secret":"excluded"}}});
    for _ in 0..2 {
        stage_run_once(&planes, &run).await;
    }
    run["pending"]["id"] = json!("approval-2");
    stage_run_once(&planes, &run).await;
    run["status"] = json!("completed");
    for _ in 0..2 {
        stage_run_once(&planes, &run).await;
    }
    relay(&planes.app, &planes.user, "admin").await.unwrap();
    let page_one = page(&planes.app, 0, 2).await.unwrap();
    assert_eq!(page_one["events"].as_array().unwrap().len(), 2);
    assert_eq!(page_one["has_more"], true);
    let tail = page(&planes.app, page_one["next_seq"].as_i64().unwrap(), 2)
        .await
        .unwrap();
    assert_eq!(tail["events"][0]["type"], "run.completed");
    assert_eq!(tail["has_more"], false);
    assert!(!tail.to_string().contains("secret"));
}

#[tokio::test]
async fn unsupported_kinds_and_plane_wide_agent() {
    let planes = planes().await;
    let error = stage_once(&planes, &notice("x", "checkpoint"))
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "Unsupported notification type");
    // Plane-wide events take the handle's agent without reading a session record.
    let mut notes = notice("main_notes:1", "main_notes.updated");
    notes.session_id = "absent";
    notes.status = Some("1");
    stage_once(&planes, &notes).await.unwrap();
    relay(&planes.app, &planes.user, "admin").await.unwrap();
    let page = page(&planes.app, 0, 20).await.unwrap();
    assert_eq!(page["events"][0]["agent_id"], "shared_default");
    assert_eq!(page["events"][0]["payload"]["status"], "1");
    // A session event whose session record is missing fails inside the caller's transaction,
    // which rolls back its deduplication marker too.
    let mut absent = notice("created:absent", "session.created");
    absent.session_id = "absent";
    assert!(stage_once(&planes, &absent).await.is_err());
    let staged: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_emitted")
        .fetch_one(&planes.user)
        .await
        .unwrap();
    assert_eq!(staged, 1);
}

#[tokio::test]
async fn one_pass_moves_every_account_in_order_and_a_crash_before_the_delete_duplicates_nothing() {
    let planes = planes().await;
    let other = webagent_storage::pool::open_pool(&planes.root.join("other.db"), true, true)
        .await
        .unwrap();
    schema(&other, Plane::User).await.unwrap();
    sqlx::query("INSERT INTO docs(key,value) VALUES('session:s',?)")
        .bind(br#"{"id":"s","agent_id":"other_agent"}"#.to_vec())
        .execute(&other)
        .await
        .unwrap();
    let source = Source {
        user_id: "other",
        agent_id: "shared_default",
    };
    stage_once(&planes, &notice("created:s", "session.created"))
        .await
        .unwrap();
    stage_once(&planes, &notice("message:r", "message.received"))
        .await
        .unwrap();
    let mut tx = other.begin().await.unwrap();
    stage(
        &mut tx,
        &source,
        &notice("created:s", "session.created"),
        plain,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let first = collect(&planes.app, &planes.user, "admin")
        .await
        .unwrap()
        .unwrap();
    let second = collect(&planes.app, &other, "other")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.len(), second.len()), (2, 1));
    assert_eq!(
        commit(&planes.app, &[first, second]).await.unwrap(),
        vec![2, 1]
    );
    // A crash here leaves both accounts' rows staged: the next pass moves none of them again,
    // and a row staged after the crash follows the earlier ones.
    stage_once(&planes, &notice("run:r", "run.started"))
        .await
        .unwrap();
    let first = collect(&planes.app, &planes.user, "admin")
        .await
        .unwrap()
        .unwrap();
    let second = collect(&planes.app, &other, "other")
        .await
        .unwrap()
        .unwrap();
    assert_eq!((first.len(), second.len()), (1, 0));
    let through = commit(&planes.app, &[first, second]).await.unwrap();
    assert_eq!(through, vec![3, 1]);
    acknowledge(&planes.user, through[0]).await.unwrap();
    acknowledge(&other, through[1]).await.unwrap();
    let page = page(&planes.app, 0, 20).await.unwrap();
    let events: Vec<(i64, &str)> = page["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["seq"].as_i64().unwrap(),
                row["event_id"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        events,
        vec![
            (1, "admin:created:s"),
            (2, "admin:message:r"),
            (3, "other:created:s"),
            (4, "admin:run:r"),
        ]
    );
    let watermarks: Vec<(String, i64)> =
        sqlx::query_as("SELECT source_user,through_seq FROM notification_relay ORDER BY 1")
            .fetch_all(&planes.app)
            .await
            .unwrap();
    assert_eq!(
        watermarks,
        vec![("admin".to_owned(), 3), ("other".to_owned(), 1)]
    );
    assert!(collect(&planes.app, &planes.user, "admin")
        .await
        .unwrap()
        .is_none());
    assert!(collect(&planes.app, &other, "other")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_pass_with_nothing_staged_writes_nothing() {
    let planes = planes().await;
    assert!(collect(&planes.app, &planes.user, "admin")
        .await
        .unwrap()
        .is_none());
    assert!(commit(&planes.app, &[]).await.unwrap().is_empty());
    let watermarks: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notification_relay")
        .fetch_one(&planes.app)
        .await
        .unwrap();
    assert_eq!(watermarks, 0);
}
