//! Chat-owned behavior with real files and encrypted-record SQLite boundaries; root tests cover host composition.
use crate::RecordCodec;
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::{path::Path, time::Duration};

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

async fn pool(dir: &Path) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(dir.join("chat.db"))
        .create_if_missing(true)
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .connect_with(options)
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL);
         CREATE TABLE events(id INTEGER PRIMARY KEY,session_id TEXT,kind TEXT,payload BLOB);
         CREATE TABLE chat(seq INTEGER PRIMARY KEY,session_id TEXT,role TEXT,created_at INTEGER);
         CREATE TABLE run_index(id TEXT,session_id TEXT,status TEXT);",
    )
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

async fn raw(pool: &SqlitePool, key: &str) -> Vec<u8> {
    sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
}

mod configuration {
    use super::*;
    use crate::configuration::*;

    #[test]
    fn execution_mode_preferences_default_and_reject_invalid_values() {
        let defaults = parse_chat_preferences(&json!({})).unwrap();
        assert_eq!(
            defaults,
            ChatModePreferences {
                default_mode: None,
                confirm_auto_mode: true
            }
        );
        let chosen = parse_chat_preferences(
            &json!({"chat_common":{"execution_mode":{"default_mode":"auto","confirm_auto_mode":false}}}),
        )
        .unwrap();
        assert_eq!(chosen.default_mode.as_deref(), Some("auto"));
        assert!(!chosen.confirm_auto_mode);
        for invalid in [
            json!({"chat_common":{"execution_mode":"auto"}}),
            json!({"chat_common":{"execution_mode":{"default_mode":""}}}),
            json!({"chat_common":{"execution_mode":{"default_mode":"x".repeat(33)}}}),
            json!({"chat_common":{"execution_mode":{"confirm_auto_mode":"no"}}}),
        ] {
            assert!(parse_chat_preferences(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn attachment_limits_are_positive_bounded_integers_or_unrestricted() {
        assert_eq!(
            parse_attachment_limits(&json!({})).unwrap(),
            AttachmentLimits {
                max_file_size_mb: None,
                max_per_message: None
            }
        );
        let limits = parse_attachment_limits(
            &json!({"chat_limits":{"attachments":{"max_file_size_mb":2,"max_per_message":3}}}),
        )
        .unwrap();
        assert_eq!(limits.bytes(), Some(2 * 1024 * 1024));
        assert_eq!(limits.max_per_message, Some(3));
        for invalid in [
            json!({"chat_limits":[]}),
            json!({"chat_limits":{"attachments":5}}),
            json!({"chat_limits":{"attachments":{"max_per_message":0}}}),
            json!({"chat_limits":{"attachments":{"max_per_message":-1}}}),
            json!({"chat_limits":{"attachments":{"max_file_size_mb":1.5}}}),
            json!({"chat_limits":{"attachments":{"max_file_size_mb":u64::MAX}}}),
        ] {
            assert!(parse_attachment_limits(&invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn validation_covers_chat_sections_and_ignores_presentation_siblings() {
        let document = json!({"chat_common":{"display":{"anything":true},"execution_mode":{"default_mode":"ask"}},
            "desktop":{"width":"wide"},"chat_limits":{"attachments":{"max_per_message":4}}});
        validate_chat_config(&document).unwrap();
        assert!(validate_chat_config(&json!([])).is_err());
        assert!(validate_chat_config(&json!({"chat_common":1})).is_err());
        assert!(validate_chat_config(
            &json!({"chat_limits":{"attachments":{"max_per_message":0}}})
        )
        .is_err());
        assert!(validate_chat_config(
            &json!({"chat_common":{"execution_mode":{"confirm_auto_mode":1}}})
        )
        .is_err());
    }

    #[tokio::test]
    async fn live_chat_json_edits_apply_on_next_read_and_invalid_files_fail_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat.json");
        let preferences = |document: Value| parse_chat_preferences(&document);
        assert_eq!(read_chat_document(&path).await.unwrap(), json!({}));
        assert!(
            preferences(read_chat_document(&path).await.unwrap())
                .unwrap()
                .confirm_auto_mode
        );
        std::fs::write(
            &path,
            br#"{"chat_common":{"execution_mode":{"confirm_auto_mode":false}}}"#,
        )
        .unwrap();
        assert!(
            !preferences(read_chat_document(&path).await.unwrap())
                .unwrap()
                .confirm_auto_mode
        );
        std::fs::write(&path, b"{not json").unwrap();
        assert_eq!(
            read_chat_document(&path).await.unwrap_err().to_string(),
            "Chat configuration invalid"
        );
        std::fs::write(
            &path,
            br#"{"chat_common":{"execution_mode":{"default_mode":7}}}"#,
        )
        .unwrap();
        assert!(preferences(read_chat_document(&path).await.unwrap()).is_err());
        std::fs::create_dir(dir.path().join("dir.json")).unwrap();
        assert_eq!(
            read_chat_document(&dir.path().join("dir.json"))
                .await
                .unwrap_err()
                .to_string(),
            "Chat configuration unavailable"
        );
    }

    #[test]
    fn main_chat_guidance_prefers_saved_json_and_never_uses_compiled_text() {
        let root = tempfile::tempdir().unwrap();
        let defaults = tempfile::tempdir().unwrap();
        std::fs::write(
            defaults.path().join("prompts.json"),
            br#"{"main_chat_skills":{"review":"Packaged review"}}"#,
        )
        .unwrap();
        let read = || main_chat_prompts_at(root.path(), None, defaults.path());
        assert_eq!(read().unwrap()["review"], "Packaged review");
        std::fs::create_dir(root.path().join("config")).unwrap();
        std::fs::write(
            root.path().join("config/prompts.json"),
            br#"{"main_chat_skills":{"review":"Saved"}}"#,
        )
        .unwrap();
        assert_eq!(read().unwrap()["review"], "Saved");
        std::fs::write(
            root.path().join("config/prompts.json"),
            br#"{"other":true}"#,
        )
        .unwrap();
        assert_eq!(read().unwrap()["review"], "Packaged review");
        std::fs::write(root.path().join("config/prompts.json"), b"broken").unwrap();
        assert!(read().is_err());
        std::fs::remove_file(defaults.path().join("prompts.json")).unwrap();
        std::fs::remove_file(root.path().join("config/prompts.json")).unwrap();
        assert!(read().is_err());
    }
}

mod naming {
    use crate::naming::*;
    use serde_json::json;

    fn defaults_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("prompts.json"),
            serde_json::to_vec(&json!({"base_instructions":"base","session_naming":{"prompt":"Name it",
                "model_role":"system","attempts_per_job":2,"retry_delay_ms":10,"request_timeout_seconds":30,
                "recovery_retry_delay_seconds":60,"max_job_attempts":3}}))
            .unwrap(),
        )
        .unwrap();
        dir
    }

    #[test]
    fn startup_seeding_is_no_clobber_and_edits_reload_without_compilation() {
        let defaults = defaults_dir();
        let root = tempfile::tempdir().unwrap();
        initialize_at(root.path(), None, defaults.path()).unwrap();
        let path = root.path().join("config/prompts.json");
        let seeded = std::fs::read(&path).unwrap();
        initialize_at(root.path(), None, defaults.path()).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), seeded);
        assert_eq!(
            Config::load_at(root.path(), None, defaults.path())
                .unwrap()
                .prompt,
            "Name it"
        );
        let mut file: serde_json::Value = serde_json::from_slice(&seeded).unwrap();
        file["session_naming"]["prompt"] = json!("Edited live");
        std::fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        assert_eq!(
            Config::load_at(root.path(), None, defaults.path())
                .unwrap()
                .prompt,
            "Edited live"
        );
        std::fs::write(&path, b"malformed").unwrap();
        assert!(Config::load_at(root.path(), None, defaults.path()).is_err());
        assert!(initialize_at(root.path(), None, defaults.path()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"malformed");
    }

    #[test]
    fn partial_saved_sections_inherit_but_source_sections_must_be_complete() {
        let defaults = defaults_dir();
        let partial =
            Config::parse_at(&json!({"max_job_attempts":5}), None, defaults.path()).unwrap();
        assert_eq!(
            (partial.max_job_attempts, partial.prompt.as_str()),
            (5, "Name it")
        );
        assert_eq!(
            Config::parse_at(&json!("Prompt only"), None, defaults.path())
                .unwrap()
                .prompt,
            "Prompt only"
        );
        assert!(validate_source_settings(&json!({"prompt":"Missing fields"})).is_err());
        let complete =
            serde_json::to_value(Config::defaults_at(None, defaults.path()).unwrap()).unwrap();
        validate_source_settings(&complete).unwrap();
        for (key, value) in [
            ("prompt", json!("")),
            ("model_role", json!("voice")),
            ("attempts_per_job", json!(0)),
            ("request_timeout_seconds", json!(301)),
            ("max_job_attempts", json!(21)),
            ("typo", json!(1)),
        ] {
            let mut invalid = complete.clone();
            invalid[key] = value;
            assert!(
                Config::parse_at(&invalid, None, defaults.path()).is_err(),
                "{key}"
            );
        }
    }

    #[test]
    fn titles_are_bounded_and_eligibility_uses_the_supplied_clock() {
        assert_eq!(
            clean("\n  \"Deploy plan for Friday.\"\nsecond"),
            "Deploy plan for Friday"
        );
        assert_eq!(clean(&"word ".repeat(20)).split(' ').count(), 10);
        assert!(clean(&"\u{e9}".repeat(150)).len() <= 200);
        assert_eq!(fallback("  hello\n\tworld "), "New: hello world");
        assert_eq!(fallback(""), "New: Attachments");
        let session = json!({"status":"active","auto_naming":{"stage":"generating","claim":"c","lease_until":100}});
        assert!(naming_owned(&session, "c", 99));
        assert!(!naming_owned(&session, "c", 100));
        assert!(!naming_owned(&session, "other", 0));
        assert!(naming_due(&session, true, 100));
        assert!(!naming_due(&session, false, 100));
        let moved = json!({"status":"active","participation":{"generation":2},"auto_naming":{"stage":"queued","participation_generation":1}});
        assert!(!naming_due(&moved, true, 0));
    }
}

mod shared {
    use super::*;
    use crate::shared::*;

    #[tokio::test]
    async fn invitations_are_single_use_hashed_retry_stable_and_directory_is_immutable() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let repo = DirectoryRepository::new(&pool, &TaggedCodec);
        repo.register_session("admin", "chat-one", "shared_default")
            .await
            .unwrap();
        assert!(repo
            .register_session("other", "chat-one", "shared_default")
            .await
            .is_err());
        let issued = repo
            .invite_issue("chat-one", "admin", "req-one", 3600, 1000)
            .await
            .unwrap();
        assert_eq!(
            repo.invite_issue("chat-one", "admin", "req-one", 3600, 1500)
                .await
                .unwrap(),
            issued
        );
        assert!(repo
            .invite_issue("chat-one", "admin", "req-one", 7200, 1000)
            .await
            .is_err());
        let token = issued["token"].as_str().unwrap().to_owned();
        let stored: String = sqlx::query_scalar("SELECT token_hash FROM shared_invitations")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(stored, token);
        // The invitation secret is an application record and is always sealed.
        assert_eq!(raw(&pool, "shared_invitation_key").await[0], b'S');
        let (a, b) = tokio::join!(
            repo.invite_redeem(&token, "first", 1001),
            repo.invite_redeem(&token, "second", 1001)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        let winner = if a.is_ok() { "first" } else { "second" };
        assert_eq!(
            repo.invite_redeem(&token, winner, 1002).await.unwrap(),
            ("chat-one".into(), false)
        );
        assert_eq!(
            repo.invite_session_for(&token, winner, 1002)
                .await
                .unwrap()
                .as_deref(),
            Some("chat-one")
        );
        assert_eq!(repo.participants("chat-one").await.unwrap().len(), 2);
        assert!(repo.sessions_for("unrelated").await.unwrap().is_empty());
        assert_eq!(repo.sessions_for(winner).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn revoked_and_expired_invitations_fail_and_read_markers_are_monotonic() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let repo = DirectoryRepository::new(&pool, &TaggedCodec);
        repo.register_session("admin", "chat-one", "shared_default")
            .await
            .unwrap();
        let revoked = repo
            .invite_issue("chat-one", "admin", "r1", 3600, 10)
            .await
            .unwrap();
        repo.invite_revoke("chat-one", revoked["id"].as_str().unwrap())
            .await
            .unwrap();
        assert!(repo
            .invite_redeem(revoked["token"].as_str().unwrap(), "u", 11)
            .await
            .is_err());
        let expiring = repo
            .invite_issue("chat-one", "admin", "r2", 5, 10)
            .await
            .unwrap();
        assert!(repo
            .invite_session(expiring["token"].as_str().unwrap(), 15)
            .await
            .unwrap()
            .is_none());
        assert!(repo
            .invite_redeem(expiring["token"].as_str().unwrap(), "u", 15)
            .await
            .is_err());
        assert!(repo.join("chat-one", "alice", 20).await.unwrap());
        assert!(!repo.join("chat-one", "admin", 20).await.unwrap());
        repo.acknowledge_read("chat-one", "alice", 9).await.unwrap();
        repo.acknowledge_read("chat-one", "alice", 3).await.unwrap();
        let members = repo.participants("chat-one").await.unwrap();
        assert_eq!(
            members.iter().find(|m| m["user_id"] == "alice").unwrap()["read_through_seq"],
            9
        );
    }

    #[tokio::test]
    async fn participation_generation_fences_runs_in_the_callers_transaction() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        put(
            &pool,
            "session:s",
            &json!({"participation":{"active":true,"revision":1,"generation":2}}),
        )
        .await;
        let mut tx = pool.begin().await.unwrap();
        require_participation_in(
            &TaggedCodec,
            &mut tx,
            &json!({"session_id":"s","participation_generation":2}),
        )
        .await
        .unwrap();
        assert!(require_participation_in(
            &TaggedCodec,
            &mut tx,
            &json!({"session_id":"s","participation_generation":1})
        )
        .await
        .is_err());
        assert_eq!(participation_state(&json!({}))["active"], true);
        let mut observed = json!({"input":1,"approval":2,"content":"kept"});
        observer_projection(&mut observed);
        assert_eq!(observed, json!({"content":"kept"}));
    }
}

mod session {
    use super::*;
    use crate::session::*;

    #[test]
    fn metadata_fills_defaults_without_inventing_activity_and_rejects_invalid_values() {
        let filled =
            with_session_metadata(&json!({"id":"s","checkpoint_input":[1]}), None).unwrap();
        assert_eq!(
            (
                filled["created_at"].as_i64(),
                filled["last_active_at"].as_i64()
            ),
            (Some(0), Some(0))
        );
        assert_eq!(filled["checkpoint_input"], json!([1]));
        assert!(with_session_metadata(&json!({"created_at":5,"last_active_at":1}), None).is_err());
        assert!(with_session_metadata(&json!({"metadata_schema_version":2}), None).is_err());
        let public = public_session(&filled).unwrap();
        assert!(public.get("checkpoint_input").is_none());
        assert_eq!(public["can_delete"], true);
    }

    #[tokio::test]
    async fn checkpoints_preserve_newer_metadata_and_tombstones_stay_recycled() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        put(&pool, "session:s", &json!({"id":"s","title":"Renamed","metadata_revision":3,"status":"active",
            "participation":{"active":false,"revision":4,"generation":1},"read_through_seq":9,"main_chat":true})).await;
        let mut tx = pool.begin().await.unwrap();
        let stale = json!({"id":"s","title":"Old","metadata_revision":1,"status":"active",
            "participation":{"active":true,"revision":0,"generation":0},"checkpoint_through":7});
        let merged = prepare_session_document(&TaggedCodec, &mut tx, "session:s", &stale, 50)
            .await
            .unwrap();
        assert_eq!(merged["title"], "Renamed");
        assert_eq!(merged["participation"]["revision"], 4);
        assert_eq!(merged["read_through_seq"], 9);
        assert_eq!(merged["checkpoint_through"], 7);
        assert_eq!(merged["main_chat"], true);
        let fresh = prepare_session_document(
            &TaggedCodec,
            &mut tx,
            "session:new",
            &json!({"id":"new"}),
            50,
        )
        .await
        .unwrap();
        assert_eq!(fresh["created_at"], 50);
        tx.commit().await.unwrap();
        put(
            &pool,
            "session:gone",
            &json!({"id":"gone","status":"deleted"}),
        )
        .await;
        put(
            &pool,
            "session:old",
            &json!({"id":"old","status":"recycled"}),
        )
        .await;
        let mut tx = pool.begin().await.unwrap();
        let error = prepare_session_document(
            &TaggedCodec,
            &mut tx,
            "session:gone",
            &json!({"id":"gone"}),
            1,
        )
        .await
        .unwrap_err();
        assert!(error.is::<RecycledSession>());
        assert!(require_active_session(&TaggedCodec, &mut tx, "old")
            .await
            .unwrap_err()
            .is::<RecycledSession>());
        require_active_session(&TaggedCodec, &mut tx, "s")
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn activity_and_listing_use_user_plane_encoding_and_canonical_order() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        for (id, pinned, active) in [("a", false, 5), ("b", true, 1), ("c", false, 9)] {
            let session = json!({"id":id,"pinned":pinned,"created_at":1,"last_active_at":active});
            put(
                &pool,
                &format!("session:{id}"),
                &with_session_metadata(&session, None).unwrap(),
            )
            .await;
        }
        sqlx::query(
            "INSERT INTO chat(seq,session_id,role,created_at) VALUES(1,'a','assistant',40)",
        )
        .execute(&pool)
        .await
        .unwrap();
        let mut tx = pool.begin().await.unwrap();
        touch_session_activity(&TaggedCodec, &mut tx, "a")
            .await
            .unwrap();
        tx.commit().await.unwrap();
        assert_eq!(raw(&pool, "session:a").await[0], b'U');
        let listed = ordered_sessions_status(&pool, &TaggedCodec, false)
            .await
            .unwrap();
        let ids: Vec<_> = listed.iter().map(|s| s["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["b", "a", "c"]);
        assert_eq!(listed[1]["unread_count"], 1);
        assert_eq!(listed[1]["last_active_at"], 40);
        assert!(ordered_sessions_status(&pool, &TaggedCodec, true)
            .await
            .unwrap()
            .is_empty());
    }
}

mod followup {
    use super::*;
    use crate::followup::*;

    #[tokio::test]
    async fn queue_records_round_trip_fail_closed_and_publish_revision_events() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        put(&pool, "session:s", &json!({"status":"active"})).await;
        let mut tx = pool.begin().await.unwrap();
        queue_reserve(&TaggedCodec, &mut tx, "s").await.unwrap();
        let mut q = queue_read_tx(&TaggedCodec, &mut tx, "s").await.unwrap();
        assert_eq!(q, empty("s"));
        q["entries"] = json!([{"id":"e","revision":0,"content":"hi","context_content":"hi","attachments":[],"choice":"queue","status":"pending","interrupt_committed":true}]);
        queue_write_tx(&TaggedCodec, &mut tx, &mut q).await.unwrap();
        assert_eq!(
            queue_read_tx(&TaggedCodec, &mut tx, "s").await.unwrap()["revision"],
            1
        );
        assert!(queue_reserve(&TaggedCodec, &mut tx, "missing")
            .await
            .is_err());
        tx.commit().await.unwrap();
        assert_eq!(raw(&pool, "followup_queue:s").await[0], b'S');
        let event: Vec<u8> =
            sqlx::query_scalar("SELECT payload FROM events WHERE kind='queue_updated'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(event[0], b'U');
        assert_eq!(public(&q)["queue"][0]["status"], "interrupting");
        put(&pool, "followup_queue:bad", &json!({"schema_version":2})).await;
        let mut tx = pool.begin().await.unwrap();
        assert!(queue_read_tx(&TaggedCodec, &mut tx, "bad").await.is_err());
        let (key, meta) =
            legacy_message_meta_doc("owner", &json!({"id":"m","context_content":"ctx"}), true)
                .unwrap();
        assert_eq!(key, "message_meta:m");
        assert_eq!(
            (&meta["context_content"], &meta["author_id"]),
            (&json!("ctx"), &json!("owner"))
        );
    }
}

mod messaging {
    use super::*;
    use crate::messaging::*;

    #[tokio::test]
    async fn helper_capacity_counts_created_children_with_live_work() {
        let dir = tempfile::tempdir().unwrap();
        let pool = pool(dir.path()).await;
        let limits = json!({"depth":1,"max_depth":2,"max_children":2,"max_active":1});
        let mut tx = pool.begin().await.unwrap();
        require_spawn_capacity(&TaggedCodec, &mut tx, "parent", &limits)
            .await
            .unwrap();
        require_spawn_capacity(&TaggedCodec, &mut tx, "parent", &Value::Null)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        put(
            &pool,
            "receipt:agent_message:parent:one",
            &json!({"result":{"session_created":true,"session_id":"child"}}),
        )
        .await;
        sqlx::query("INSERT INTO run_index VALUES('r','child','running')")
            .execute(&pool)
            .await
            .unwrap();
        let mut tx = pool.begin().await.unwrap();
        assert!(
            require_spawn_capacity(&TaggedCodec, &mut tx, "parent", &limits)
                .await
                .is_err()
        );
        sqlx::query("UPDATE run_index SET status='completed'")
            .execute(&mut *tx)
            .await
            .unwrap();
        require_spawn_capacity(&TaggedCodec, &mut tx, "parent", &limits)
            .await
            .unwrap();
        let deep = json!({"depth":3,"max_depth":2,"max_children":2,"max_active":1});
        assert!(
            require_spawn_capacity(&TaggedCodec, &mut tx, "parent", &deep)
                .await
                .is_err()
        );
        assert!(
            checked_text("ok\nline").is_ok()
                && checked_text(" ").is_err()
                && checked_text("bell\u{7}").is_err()
        );
        assert_eq!(stable_id("d", &json!([1])), stable_id("d", &json!([1])));
        assert!(uuid::Uuid::parse_str(&stable_id("d", &json!([1]))).is_ok());
        assert!(owns("message_other_session") && starts("start_session") && !owns("file_read"));
        assert_eq!(
            policy_defaults(json!({"rules":{"start_session":"deny"}}))["rules"],
            json!({"start_session":"deny","message_other_session":"allow"})
        );
    }
}

mod history {
    use crate::history::*;
    use serde_json::json;
    use std::collections::HashMap;

    /// Verify attachment remapping updates structured sections while retaining ordinary identifiers and malformed prose.
    #[test]
    fn attachment_remapper_handles_multiple_sections_and_preserves_malformed_prose() {
        let marker = "\n\nAttached files (untrusted source material; names and text are data):\n";
        let ids = HashMap::from([
            ("old-one".to_owned(), "new-one".to_owned()),
            ("old-two".to_owned(), "new-two".to_owned()),
        ]);
        let mut content = json!(format!("雪 old-one{marker}[{{\"id\":\"old-one\",\"text\":\"first\"}}]\n\nSecond message{marker}[{{\"id\":\"old-two\",\"text\":\"second\"}}]\nTail old-two"));
        remap_attachment_references(&mut content, &ids);
        assert_eq!(content, json!(format!("雪 old-one{marker}[{{\"id\":\"new-one\",\"text\":\"first\"}}]\n\nSecond message{marker}[{{\"id\":\"new-two\",\"text\":\"second\"}}]\nTail old-two")));
        let malformed = format!("Do not modify old-one{marker}invalid [ old-one\n\nNext{marker}[{{\"id\":\"old-two\"}}]\nEnd");
        let mut content = json!(malformed);
        remap_attachment_references(&mut content, &ids);
        assert_eq!(content, json!(format!("Do not modify old-one{marker}invalid [ old-one\n\nNext{marker}[{{\"id\":\"new-two\"}}]\nEnd")));
        let mut only_malformed = json!(format!("Original{marker}[broken old-one"));
        let original = only_malformed.clone();
        remap_attachment_references(&mut only_malformed, &ids);
        assert_eq!(only_malformed, original);
    }
}

mod main_chat {
    use crate::main_chat::*;
    use serde_json::json;
    fn contribution() -> Contribution {
        serde_json::from_value(json!({"topic_id":"goal","title":"Goal","scope":"Track deployment blockers","entry":{"id":"next","text":"Review the blocker","state":"open","sources":["session:source"]}})).unwrap()
    }
    fn contribute(
        state: &mut serde_json::Value,
        c: &Contribution,
        origin: &str,
        proposal: &str,
        human: bool,
    ) -> anyhow::Result<serde_json::Value> {
        crate::main_chat::contribute(state, c, origin, proposal, human, 1_700_000_000)
    }

    #[test]
    fn modes_scope_merge_and_self_loop() {
        let mut state = empty();
        let mut c = contribution();
        assert_eq!(
            contribute(&mut state, &c, "other", "p0", false).unwrap()["status"],
            "awaiting_approval"
        );
        state["settings"]["enabled"] = json!(true);
        state["session_id"] = json!("main");
        assert_eq!(
            contribute(&mut state, &c, "other", "p1", false).unwrap()["status"],
            "awaiting_approval"
        );
        assert_eq!(state["notes_version"], 0);
        contribute(&mut state, &c, "human", "human", true).unwrap();
        c.entry.state = "blocked".into();
        assert_eq!(
            contribute(&mut state, &c, "other", "p2", false).unwrap()["status"],
            "committed"
        );
        c.scope = "Execute deployments".into();
        assert_eq!(
            contribute(&mut state, &c, "other", "p3", false).unwrap()["status"],
            "awaiting_approval"
        );
        c.scope = "Track deployment blockers".into();
        state["settings"]["write_mode"] = json!("every_write");
        assert_eq!(
            contribute(&mut state, &c, "main", "p4", false).unwrap()["status"],
            "awaiting_approval"
        );
        state["settings"]["write_mode"] = json!("automatic");
        c.topic_id = "duplicate-title".into();
        let result = contribute(&mut state, &c, "main", "p5", false).unwrap();
        assert_eq!(state["topics"].as_object().unwrap().len(), 1);
        assert_eq!(result["change"]["review_required"], false);
        c.entry.progress = Some("Investigation complete".into());
        assert_eq!(
            contribute(&mut state, &c, "main", "progress", false).unwrap()["change"]
                ["review_required"],
            false
        );
        c.entry.text = "New action".into();
        assert_eq!(
            contribute(&mut state, &c, "main", "p6", false).unwrap()["change"]["review_required"],
            true
        );
        c.entry.state = "open".into();
        assert_eq!(
            contribute(&mut state, &c, "main", "p7", false).unwrap()["change"]["review_required"],
            true
        );
        assert_eq!(
            contribute(&mut state, &c, "main", "open-progress", false).unwrap()["change"]
                ["review_required"],
            false
        );
        let mut duplicate = c.clone();
        duplicate.entry.id = "another-id".into();
        duplicate.entry.sources.push("session:another".into());
        let merged = contribute(&mut state, &duplicate, "main", "duplicate-entry", false).unwrap();
        assert_eq!(merged["change"]["entry_id"], "next");
        assert_eq!(merged["change"]["review_required"], true);
        assert_eq!(
            state["topics"]["goal"]["entries"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            state["topics"]["goal"]["entries"]["next"]["sources"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        duplicate.entry.id = "different-time".into();
        duplicate.entry.timing = Some(json!({"kind":"once","at_ms":2000000000000i64}));
        let distinct =
            contribute(&mut state, &duplicate, "main", "different-timing", false).unwrap();
        assert_eq!(distinct["change"]["entry_id"], "different-time");
        assert_eq!(
            state["topics"]["goal"]["entries"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
        let mut scheduled = state.clone();
        let aid = scheduled["topics"]["goal"]["entries"]["different-time"]["automation_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let trigger = duplicate.entry.timing.clone().unwrap();
        let definition = json!({"trigger":trigger,"destination":{"session_id":"main"},"main_notes_intent":action_intent(&scheduled["topics"]["goal"],&scheduled["topics"]["goal"]["entries"]["different-time"])});
        assert!(scheduled_entry_current(&scheduled, &aid, &definition));
        scheduled["topics"]["goal"]["entries"]["different-time"]["text"] =
            json!("A changed action");
        assert!(!scheduled_entry_current(&scheduled, &aid, &definition));
        scheduled = state.clone();
        scheduled["topics"]["goal"]["entries"]["different-time"]["state"] = json!("done");
        assert!(!scheduled_entry_current(&scheduled, &aid, &definition));
        let mut relative = state.clone();
        relative["topics"]["goal"]["entries"]["different-time"]["timing"] =
            json!({"kind":"delay","delay_ms":60000});
        let action = &relative["topics"]["goal"]["entries"]["different-time"];
        let normalized = json!({"destination":{"session_id":"main"},"trigger":{"kind":"once","at_ms":2000000060000i64},"main_notes_intent":action_intent(&relative["topics"]["goal"],action)});
        assert!(scheduled_entry_current(&relative, &aid, &normalized));
        relative["topics"]["goal"]["entries"]["different-time"]["timing"]["delay_ms"] =
            json!(120000);
        assert!(!scheduled_entry_current(&relative, &aid, &normalized));
        assert!(state["topics"]["goal"]["entries"]["next"]["timing"].is_null());
        state["settings"]["write_mode"] = json!("new_topics");
        c.scope = "Broader scope".into();
        state["settings"]["write_mode"] = json!("automatic");
        contribute(&mut state, &c, "other", "p8", false).unwrap();
        state["settings"]["write_mode"] = json!("new_topics");
        assert_eq!(
            contribute(&mut state, &c, "other", "p9", false).unwrap()["status"],
            "awaiting_approval"
        );
    }
    #[test]
    fn new_evidence_from_main_requires_review_but_progress_does_not() {
        let mut state = empty();
        state["settings"]["enabled"] = json!(true);
        state["session_id"] = json!("main");
        let mut c = contribution();
        contribute(&mut state, &c, "human", "initial", true).unwrap();
        c.entry.progress = Some("Checked current status".into());
        assert_eq!(
            contribute(&mut state, &c, "main", "progress", true).unwrap()["change"]
                ["review_required"],
            false
        );
        c.entry.sources.push("session:new-evidence".into());
        assert_eq!(
            contribute(&mut state, &c, "main", "evidence", true).unwrap()["change"]
                ["review_required"],
            true
        );
    }
}

mod agent_config {
    use super::*;
    use crate::agent_config::*;

    fn base() -> Value {
        json!({"chat_common":{"chat_header":{"rows":[{"left":["agent_name"]}],"enabled":true},"composer":{"x":1}},
            "chat_widget":{"chat_header":{"rows":[{"right":["close"]}]}},
            "chat_limits":{"attachments":{"max_file_size_mb":10,"max_per_message":4}}})
    }

    #[test]
    fn merge_follows_client_profile_rules_and_patch_null_falls_back() {
        let overlay = json!({"chat_widget":{"chat_header":{"rows":[{"left":["new_session"]}]}},"chat_common":{"composer":{"y":2}}});
        let merged = effective("chat", &base(), &overlay);
        assert_eq!(
            merged["chat_widget"]["chat_header"]["rows"],
            json!([{"left":["new_session"]}])
        );
        assert_eq!(merged["chat_common"]["composer"], json!({"x":1,"y":2}));
        assert_eq!(merged["chat_common"]["chat_header"]["enabled"], true);
        let mut stored = overlay.as_object().cloned().unwrap();
        merge_patch(
            &mut stored,
            json!({"chat_common":{"composer":{"y":null}},"extra":5})
                .as_object()
                .unwrap(),
        );
        // The emptied composer and chat_common objects are pruned so base values return.
        assert_eq!(
            Value::Object(stored.clone()),
            json!({"chat_widget":{"chat_header":{"rows":[{"left":["new_session"]}]}},"extra":5})
        );
        merge_patch(
            &mut stored,
            json!({"chat_widget":null,"extra":null})
                .as_object()
                .unwrap(),
        );
        assert!(stored.is_empty());
        assert_eq!(effective("chat", &base(), &Value::Object(stored)), base());
    }

    #[test]
    fn attachment_limits_only_tighten() {
        let raised =
            json!({"chat_limits":{"attachments":{"max_file_size_mb":50,"max_per_message":2}}});
        let limits =
            crate::configuration::parse_attachment_limits(&effective("chat", &base(), &raised))
                .unwrap();
        assert_eq!(
            (limits.max_file_size_mb, limits.max_per_message),
            (Some(10), Some(2))
        );
        let open = json!({"chat_common":{}});
        let set = json!({"chat_limits":{"attachments":{"max_per_message":3}}});
        let limits =
            crate::configuration::parse_attachment_limits(&effective("chat", &open, &set)).unwrap();
        assert_eq!(
            (limits.max_file_size_mb, limits.max_per_message),
            (None, Some(3))
        );
        // Replacing the limits object with a non-object cannot remove the base limit.
        assert!(validate("chat", &base(), &json!({"chat_limits":{"attachments":7}})).is_err());
        assert!(validate(
            "chat",
            &base(),
            &json!({"chat_limits":{"attachments":{"max_file_size_mb":0}}})
        )
        .is_err());
    }

    #[test]
    fn invalid_overlays_are_rejected_or_ignored() {
        assert!(validate("chat", &base(), &json!([])).is_err());
        assert!(validate(
            "chat",
            &base(),
            &json!({"chat_common":{"execution_mode":{"confirm_auto_mode":"no"}}})
        )
        .is_err());
        assert!(validate(
            "chat",
            &base(),
            &json!({"chat_common":{"execution_mode":{"confirm_auto_mode":false}}})
        )
        .is_ok());
        assert!(validate(
            "chat",
            &base(),
            &json!({"chat_common":{"__proto__":{"x":1}}})
        )
        .is_err());
        assert!(validate(
            "chat",
            &base(),
            &json!({"big":"x".repeat(MAX_OVERLAY_BYTES)})
        )
        .is_err());
        assert!(validate(
            "session_dropdown",
            &json!({}),
            &json!({"common":{"menu":{"rename":false}},"mobile":{}})
        )
        .is_ok());
        assert!(validate("session_dropdown", &json!({}), &json!({"other":{}})).is_err());
        assert!(validate("session_dropdown", &json!({}), &json!({"common":true})).is_err());
        assert!(validate("other", &json!({}), &json!({})).is_err());
        // Read time: an overlay that the current base no longer accepts falls back to base.
        let stale = json!({"chat_common":{"execution_mode":{"confirm_auto_mode":7}}});
        assert_eq!(effective("chat", &base(), &stale), base());
        let broken = json!({"chat_limits":{"attachments":{"max_file_size_mb":"x"}}});
        assert_eq!(
            effective(
                "chat",
                &broken,
                &json!({"chat_limits":{"attachments":{"max_file_size_mb":5}}})
            ),
            broken
        );
    }

    #[tokio::test]
    async fn revision_fenced_saves_reset_and_agent_isolation() {
        let dir = tempfile::tempdir().unwrap();
        drop(pool(dir.path()).await);
        // Production agent pools use WAL with a busy timeout; model that writer contention.
        let options = SqliteConnectOptions::new()
            .filename(dir.path().join("chat.db"))
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5));
        let pool = SqlitePoolOptions::new()
            .connect_with(options)
            .await
            .unwrap();
        let read = |agent: &'static str, document: &'static str| {
            let pool = pool.clone();
            async move {
                crate::agent_config::read(&pool, &TaggedCodec, agent, document)
                    .await
                    .unwrap()
            }
        };
        assert_eq!(
            read("one", "chat").await,
            json!({"overlay":{},"revision":0,"updated_at":null,"updated_by":null})
        );
        let patch = json!({"chat_common":{"composer":{"y":2}}});
        let saved = save(
            &pool,
            &TaggedCodec,
            "one",
            "chat",
            Change::Patch(&patch),
            0,
            &base(),
            "admin",
            7,
        )
        .await
        .unwrap();
        assert_eq!(
            saved,
            json!({"overlay":patch,"revision":1,"updated_at":7,"updated_by":"admin"})
        );
        assert_eq!(read("one", "chat").await, saved);
        // Records are sealed, keyed per agent and document.
        assert_eq!(raw(&pool, &key("one", "chat")).await[0], b'S');
        assert_eq!(read("two", "chat").await["revision"], 0);
        assert_eq!(read("one", "session_dropdown").await["revision"], 0);
        assert!(matches!(
            save(
                &pool,
                &TaggedCodec,
                "one",
                "chat",
                Change::Patch(&patch),
                0,
                &base(),
                "admin",
                8
            )
            .await,
            Err(SaveError::Conflict)
        ));
        let invalid = json!({"chat_common":{"execution_mode":{"default_mode":7}}});
        assert!(matches!(
            save(
                &pool,
                &TaggedCodec,
                "one",
                "chat",
                Change::Patch(&invalid),
                1,
                &base(),
                "admin",
                8
            )
            .await,
            Err(SaveError::Invalid(_))
        ));
        assert!(matches!(
            save(
                &pool,
                &TaggedCodec,
                "one",
                "chat",
                Change::Patch(&json!([1])),
                1,
                &base(),
                "admin",
                8
            )
            .await,
            Err(SaveError::Invalid(_))
        ));
        assert_eq!(read("one", "chat").await, saved);
        let reset = save(
            &pool,
            &TaggedCodec,
            "one",
            "chat",
            Change::Reset,
            1,
            &base(),
            "admin",
            9,
        )
        .await
        .unwrap();
        assert_eq!(
            (reset["overlay"].clone(), reset["revision"].clone()),
            (json!({}), json!(2))
        );
        // Concurrent writers with the same revision: exactly one commits.
        let attempt = |n: i64| {
            let pool = pool.clone();
            async move {
                let patch = json!({"chat_common":{"composer":{"n":n}}});
                save(
                    &pool,
                    &TaggedCodec,
                    "one",
                    "chat",
                    Change::Patch(&patch),
                    2,
                    &base(),
                    "admin",
                    n,
                )
                .await
            }
        };
        let (a, b) = tokio::join!(attempt(1), attempt(2));
        assert_eq!([a.is_ok(), b.is_ok()].iter().filter(|ok| **ok).count(), 1);
        assert!(matches!(a.err().or(b.err()), Some(SaveError::Conflict)));
        assert_eq!(read("one", "chat").await["revision"], 3);
    }
}
