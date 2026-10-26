//! Files-owned behavior with real chat.json files and encrypted-record SQLite boundaries;
//! root tests cover HTTP composition, authority and message admission.
use crate::{repository, uploads::*, RecordCodec};
use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};

/// Tags records so tests can prove which encoding each write used.
struct TaggedCodec;
impl RecordCodec for TaggedCodec {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        Ok([b"S".as_slice(), &serde_json::to_vec(value)?].concat())
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        match value.split_first() {
            Some((b'S' | b'U', rest)) => Ok(serde_json::from_slice(rest)?),
            _ => Err(anyhow!("Storage authentication failed")),
        }
    }
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        Ok([b"U".as_slice(), &serde_json::to_vec(value)?].concat())
    }
}

async fn pool(dir: &std::path::Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(dir.join("files.db"))
        .create_if_missing(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

async fn put(pool: &SqlitePool, key: &str, value: &Value) {
    sqlx::query("INSERT OR REPLACE INTO docs(key,value) VALUES(?,?)")
        .bind(key)
        .bind(TaggedCodec.seal(value).unwrap())
        .execute(pool)
        .await
        .unwrap();
}

async fn raw(pool: &SqlitePool, key: &str) -> Option<Vec<u8>> {
    sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[test]
fn envelope_follows_chat_attachment_limits() {
    // Chat reads chat.json (with per-agent overlays) and owns the limit semantics.
    let parse = |value: Value| webagent_chat::configuration::parse_attachment_limits(&value);
    let open = parse(json!({})).unwrap();
    assert_eq!(
        (
            open.max_file_size_mb,
            open.max_per_message,
            envelope_limit(open)
        ),
        (None, None, usize::MAX)
    );
    let limits =
        parse(json!({"chat_limits":{"attachments":{"max_file_size_mb":1,"max_per_message":2}}}))
            .unwrap();
    assert_eq!(
        (limits.max_file_size_mb, limits.max_per_message),
        (Some(1), Some(2))
    );
    assert_eq!(
        envelope_limit(limits),
        (1024 * 1024usize).div_ceil(3) * 4 + 65536
    );
    assert!(
        parse(json!({"chat_limits":{"attachments":{"max_file_size_mb":0}}}))
            .unwrap_err()
            .to_string()
            .contains("positive integer")
    );
}

#[test]
fn names_media_and_identifiers_are_bounded() {
    assert!(
        name_ok("notes.txt")
            && !name_ok("")
            && !name_ok(" x")
            && !name_ok("a/b")
            && !name_ok("a\\b")
            && !name_ok("a\nb")
    );
    assert!(!name_ok(&"x".repeat(256)));
    assert!(
        allowed_media("image/png")
            && allowed_media("application/pdf")
            && !allowed_media("application/octet-stream")
    );
    assert_eq!(normalize_media(" Text/Plain; charset=utf-8"), "text/plain");
    assert!(upload_id_ok("6f1c1f9e-36b4-4f3b-9d4e-0f61b6f0d0a1") && !upload_id_ok("not-a-uuid"));
}

#[test]
fn upload_record_keeps_provenance_text_and_size_limits() {
    let limits = AttachmentLimits {
        max_file_size_mb: Some(1),
        max_per_message: None,
    };
    let input = |media, data| UploadInput {
        id: "a",
        session_id: "s",
        uploader_id: "alice",
        name: "n.txt",
        media_type: media,
        data,
        created_at: 7,
    };
    let encoded = STANDARD.encode(b"hello");
    let text = new_upload(limits, input("text/plain", &encoded)).unwrap();
    assert_eq!(
        (
            text.bytes,
            &text.record["text"],
            &text.record["uploader_id"],
            &text.record["run_id"]
        ),
        (5, &json!("hello"), &json!("alice"), &Value::Null)
    );
    assert_eq!(text.record["upload_digest"], text.fingerprint);
    assert_eq!(
        new_upload(limits, input("text/plain", &encoded))
            .unwrap()
            .fingerprint,
        text.fingerprint
    );
    let image = new_upload(limits, input("image/png", &encoded)).unwrap();
    assert_eq!(
        (&image.record["preview"], &image.record["text"]),
        (&json!(true), &json!(""))
    );
    assert_ne!(image.fingerprint, text.fingerprint);
    assert_eq!(
        new_upload(limits, input("application/pdf", &encoded))
            .unwrap()
            .record["text"],
        ""
    );
    assert_eq!(
        new_upload(limits, input("text/plain", "***")).err(),
        Some(Rejected::Invalid("Invalid attachment bytes"))
    );
    assert_eq!(
        new_upload(limits, input("text/plain", "")).err(),
        Some(Rejected::Invalid("File must be nonempty"))
    );
    let large = STANDARD.encode(vec![b'x'; 1024 * 1024 + 1]);
    assert_eq!(
        new_upload(limits, input("text/plain", &large)).err(),
        Some(Rejected::TooLarge(
            "File exceeds configured 1 MB limit".into()
        ))
    );
    let open = AttachmentLimits {
        max_file_size_mb: None,
        max_per_message: None,
    };
    assert!(new_upload(open, input("text/plain", &large)).is_ok());
    let meta = metadata(&text.record);
    assert!(
        meta.get("data").is_none()
            && meta.get("text").is_none()
            && meta.get("uploader_id").is_none()
    );
}

#[test]
fn draft_ownership_retry_and_sent_view() {
    let mut record = json!({"id":"a","session_id":"s","run_id":null,"uploader_id":"alice","upload_digest":"f","schema_version":2,"text":"t"});
    assert_eq!(draft_owner(&record, "owner"), "alice");
    assert!(same_upload(&record, "s", "f", "alice", "alice"));
    assert!(
        !same_upload(&record, "s", "f", "alice", "bob")
            && !same_upload(&record, "other", "f", "alice", "alice")
            && !same_upload(&record, "s", "g", "alice", "alice")
    );
    assert!(sent_view(&record, "s").is_none());
    record["run_id"] = json!("r");
    assert_eq!(sent_view(&record, "s").unwrap()["text"], "t");
    assert!(sent_view(&record, "other").is_none());
    record["schema_version"] = json!(3);
    assert!(sent_view(&record, "s").is_none());
    record.as_object_mut().unwrap().remove("uploader_id");
    assert_eq!(draft_owner(&record, "owner"), "owner");
}

#[test]
fn quota_message_limits_and_content_policy() {
    assert_eq!(quota_with(None, 3), Some(json!({"bytes":3})));
    assert_eq!(
        quota_with(Some(&json!({"bytes":4})), 3),
        Some(json!({"bytes":7}))
    );
    assert_eq!(quota_with(Some(&json!({"bytes":u64::MAX})), 1), None);
    let limits = AttachmentLimits {
        max_file_size_mb: Some(1),
        max_per_message: Some(2),
    };
    let inline = crate::attachments::prepare(
        Some(&json!([{"name":"a.txt","media_type":"text/plain","text":"x"}])),
        "s",
        "r",
    )
    .unwrap();
    assert!(check_message_limits(limits, 1, &inline).is_ok());
    assert_eq!(
        check_message_limits(limits, 2, &inline).unwrap_err(),
        "At most 2 attachments per message"
    );
    assert!(check_record_size(limits, &json!({"bytes":1024*1024})).is_ok());
    assert_eq!(
        check_record_size(limits, &json!({"bytes":1024*1024+1})).unwrap_err(),
        "Attachment exceeds configured 1 MB limit"
    );
    assert!(check_record_size(limits, &json!({})).is_err());
    assert!(check_record_size(
        AttachmentLimits {
            max_file_size_mb: None,
            max_per_message: None
        },
        &json!({})
    )
    .is_ok());
    assert_eq!(
        content_bytes(&json!({"schema_version":1,"text":"legacy"})).unwrap(),
        b"legacy"
    );
    assert_eq!(
        content_bytes(&json!({"schema_version":2,"data":STANDARD.encode(b"raw")})).unwrap(),
        b"raw"
    );
    assert!(content_bytes(&json!({"schema_version":2,"data":"***"})).is_err());
    assert!(
        inline_media("image/png")
            && inline_media("video/mp4")
            && !inline_media("image/svg+xml")
            && !inline_media("text/html")
    );
    let input = describe_input(&json!({"media_type":"image/png","data":"AAAA"}));
    assert_eq!(
        input[0]["content"][1]["image_url"],
        "data:image/png;base64,AAAA"
    );
}

#[tokio::test]
async fn pending_upload_deletion_is_session_scoped_and_keeps_user_plane_quota() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(dir.path()).await;
    put(
        &pool,
        "attachment:a",
        &json!({"id":"a","session_id":"s","run_id":null,"bytes":3}),
    )
    .await;
    put(
        &pool,
        "attachment:sent",
        &json!({"id":"sent","session_id":"s","run_id":"r","bytes":2}),
    )
    .await;
    put(&pool, crate::QUOTA_KEY, &json!({"bytes":8})).await;
    for (sid, aid) in [("other", "a"), ("s", "sent")] {
        let mut tx = pool.begin().await.unwrap();
        assert!(
            repository::delete_pending_upload(&TaggedCodec, &mut tx, sid, aid)
                .await
                .is_err()
        );
    }
    let mut tx = pool.begin().await.unwrap();
    assert!(
        repository::delete_pending_upload(&TaggedCodec, &mut tx, "s", "a")
            .await
            .unwrap()
    );
    tx.commit().await.unwrap();
    assert!(raw(&pool, "attachment:a").await.is_none());
    let quota = raw(&pool, crate::QUOTA_KEY).await.unwrap();
    assert_eq!(
        quota[0], b'U',
        "quota keeps the owner's user-plane encoding"
    );
    assert_eq!(TaggedCodec.unseal(&quota).unwrap()["bytes"], 5);
    let mut tx = pool.begin().await.unwrap();
    assert!(
        !repository::delete_pending_upload(&TaggedCodec, &mut tx, "s", "a")
            .await
            .unwrap()
    );
    assert!(raw(&pool, "attachment:sent").await.is_some());
}
