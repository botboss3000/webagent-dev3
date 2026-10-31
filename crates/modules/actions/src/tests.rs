//! Pure rule tests and ledger tests on a real SQLite user-plane schema.
#![allow(clippy::unwrap_used)]

use super::intent::{self, Candidate};
use super::outcome::{self, RecoveryTarget};
use super::receipts;
use super::repository::Ledger;
use super::{default_on, supported, Protection, RecordCodec};
use anyhow::{anyhow, Result};
use serde_json::{json, Value};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::SqlitePool;

/// Test codec: a tagged JSON envelope, so a raw read proves records never sit as plain JSON.
struct TaggedCodec;
impl RecordCodec for TaggedCodec {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        let mut out = b"sealed:".to_vec();
        out.extend(serde_json::to_vec(value)?);
        Ok(out)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        let body = value
            .strip_prefix(b"sealed:")
            .ok_or_else(|| anyhow!("unsealed record"))?;
        Ok(serde_json::from_slice(body)?)
    }
    fn encode_user_record(&self, value: &Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(value)?)
    }
}

async fn pool(dir: &tempfile::TempDir) -> SqlitePool {
    let options = SqliteConnectOptions::new()
        .filename(dir.path().join("user.db"))
        .create_if_missing(true);
    let pool = SqlitePool::connect_with(options).await.unwrap();
    // Same tables Storage creates for every user plane.
    sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB NOT NULL); CREATE TABLE action_lock(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL); INSERT INTO action_lock VALUES(1,0); CREATE TABLE action_index(id TEXT PRIMARY KEY,resource TEXT NOT NULL,intent_key TEXT NOT NULL,state TEXT NOT NULL); CREATE INDEX action_intent ON action_index(resource,intent_key);")
        .execute(&pool)
        .await
        .unwrap();
    pool
}

fn candidate(id: &str, seq: i64, content: &str) -> Value {
    let args = json!({"slot_name":"notes","content":content});
    intent::candidate(Candidate {
        id,
        requester: "admin",
        tool: "edit_own_prompt",
        agent: "shared_default",
        session_id: "s",
        run_id: "r",
        origin: intent::origin("admin", "s", &format!("user-{seq}"), seq, 1),
        arguments: &args,
        pending_id: id,
        created_at: 1,
    })
}

#[test]
fn supported_tools_default_to_protection_on() {
    for name in [
        "prompt_slot_write",
        "edit_own_prompt",
        "save_own_skill",
        "remove_own_skill",
        "test_owner_purchase",
    ] {
        assert!(supported(name), "{name}");
        assert!(default_on(name), "{name}");
    }
    assert!(!supported("shell"));
    assert!(!default_on("file_read"));
    assert_eq!(Protection::default(), Protection::On);
    assert_eq!(serde_json::to_value(Protection::Off).unwrap(), "off");
    assert_eq!(
        serde_json::from_value::<Protection>(json!("on")).unwrap(),
        Protection::On
    );
    assert!(serde_json::from_value::<Protection>(json!("ignore")).is_err());
}

#[test]
fn effect_identity_distinguishes_content_and_ignores_retry_preconditions() {
    let a = json!({"slot_name":"notes","content":"one","expected_revision":1});
    let b = json!({"slot_name":"notes","content":"one","expected_revision":2});
    assert_eq!(
        intent::intent_key("edit_own_prompt", &a),
        intent::intent_key("edit_own_prompt", &b)
    );
    assert_ne!(
        intent::intent_key("edit_own_prompt", &a),
        intent::intent_key(
            "edit_own_prompt",
            &json!({"slot_name":"notes","content":"two"})
        )
    );
    // Both prompt adapters describe the same effect when optional metadata is empty.
    assert_eq!(
        intent::intent_key("edit_own_prompt", &a),
        intent::intent_key(
            "prompt_slot_write",
            &json!({"action":"put","slot_name":"notes","content":"one"})
        )
    );
    assert_ne!(
        intent::intent_key(
            "prompt_slot_write",
            &json!({"action":"put","slot_name":"notes","content":"one","lock":true})
        ),
        intent::intent_key("edit_own_prompt", &a)
    );
    assert_ne!(
        intent::intent_key("test_owner_purchase", &json!({"item":"book"})),
        intent::intent_key("test_owner_purchase", &json!({"item":"pen"}))
    );
}

#[test]
fn metadata_ids_are_bounded_and_strict() {
    assert_eq!(intent::metadata(&json!({"x":1})).unwrap(), None);
    assert_eq!(intent::metadata(&json!({"_action":null})).unwrap(), None);
    let meta = intent::metadata(&json!({"_action":{"id":"a-1","separate_from":"b_2"}}))
        .unwrap()
        .unwrap();
    assert_eq!(meta.id, "a-1");
    assert_eq!(meta.separate_from.as_deref(), Some("b_2"));
    assert_eq!(intent::action_id(Some(&meta), "p"), "a-1");
    assert_eq!(intent::action_id(None, "p"), "action-p");
    for bad in [
        json!({"_action":{"id":""}}),
        json!({"_action":{"id":"has space"}}),
        json!({"_action":{"id":"x".repeat(intent::ID_MAX + 1)}}),
        json!({"_action":{"id":"ok","separate_from":"no/slash"}}),
    ] {
        assert_eq!(
            intent::metadata(&bad).unwrap_err().to_string(),
            "Invalid action identity"
        );
    }
    assert!(intent::metadata(&json!({"_action":{"id":"ok","extra":1}})).is_err());
    assert!(intent::valid_id(&"x".repeat(intent::ID_MAX)));
    assert_eq!(
        intent::strip(&json!({"a":1,"_action":{"id":"x"}})),
        json!({"a":1})
    );
}

#[test]
fn candidate_binds_requester_resource_and_provider_key() {
    let record = candidate("a", 3, "one");
    assert_eq!(record["resource"], "agent:shared_default");
    assert_eq!(record["origin"]["seq"], 3);
    assert_eq!(
        intent::resource("x", "test_owner_purchase"),
        "provider:test"
    );
    let other = intent::candidate(Candidate {
        requester: "someone",
        ..Candidate {
            id: "a",
            requester: "admin",
            tool: "edit_own_prompt",
            agent: "shared_default",
            session_id: "s",
            run_id: "r",
            origin: Value::Null,
            arguments: &json!({"slot_name":"notes","content":"one"}),
            pending_id: "a",
            created_at: 1,
        }
    });
    assert_eq!(record["intent_key"], other["intent_key"]);
    assert_ne!(record["provider_key"], other["provider_key"]);
    assert!(record["provider_key"].as_str().unwrap().starts_with("wa-"));
}

#[test]
fn automated_runs_and_agent_messages_are_not_human_origin() {
    assert!(!intent::automated_run(&json!({"id":"r"})));
    assert!(intent::automated_run(
        &json!({"module_binding":{"id":"automation"}})
    ));
    assert!(intent::automated_run(
        &json!({"received_agent_message":true})
    ));
    assert!(intent::automated_run(&json!({"origin_agent_message":{}})));
    assert!(intent::agent_message(&json!({"agent_message":{}})));
    assert!(!intent::agent_message(&json!({})));
}

#[test]
fn responses_replay_success_and_hold_uncertainty() {
    let done = json!({"id":"a","state":"completed","result":"saved","receipt":{"r":1}});
    let replay = outcome::response(&done);
    assert_eq!(replay["output"], "saved");
    assert_eq!(replay["replayed_action"], true);
    assert_eq!(replay["receipt"]["r"], 1);
    let held = outcome::response(&json!({"id":"a","state":"uncertain"}));
    assert_eq!(held["status"], "action_status");
    assert!(held["output"]
        .as_str()
        .unwrap()
        .starts_with("Outcome is uncertain"));
    assert!(outcome::unsettled(&json!({"state":"running"})));
    assert!(!outcome::unsettled(&json!({"state":"completed"})));
}

#[test]
fn outcomes_settle_only_explicit_success_or_denial() {
    let ok = outcome::outcome(&Ok(json!({"status":"success"})));
    assert_eq!((ok.state, ok.retry_safe), ("completed", false));
    let denied = outcome::outcome(&Ok(json!({"status":"denied"})));
    assert_eq!(
        (denied.state, denied.retry_safe),
        ("confirmed_failed", true)
    );
    let flagged = outcome::outcome(&Ok(json!({"status":"success","uncertain":true})));
    assert_eq!(flagged.state, "uncertain");
    let failed = outcome::outcome(&Err(anyhow!("timeout")));
    assert_eq!((failed.state, failed.value), ("uncertain", Value::Null));
    assert!(outcome::uses_agent_receipt(
        "completed",
        &json!({"tool":"edit_own_prompt"})
    ));
    assert!(!outcome::uses_agent_receipt(
        "completed",
        &json!({"tool":"test_owner_purchase"})
    ));
    let saved =
        json!({"id":"a","state":"completed","receipt":{"r":2},"result":{"status":"success"}});
    let lost = outcome::finished(false, &saved, outcome::outcome(&Ok(json!({}))));
    assert_eq!(lost["replayed_action"], true);
    let won = outcome::finished(
        true,
        &saved,
        outcome::outcome(&Ok(json!({"status":"success","output":1}))),
    );
    assert_eq!(
        (won["output"].clone(), won["receipt"]["r"].clone()),
        (json!(1), json!(2))
    );
    let unsure = outcome::finished(true, &saved, outcome::outcome(&Ok(json!({}))));
    assert_eq!(unsure["uncertain"], true);
}

#[test]
fn admit_binds_new_actions_and_refuses_duplicates() {
    let mut run = json!({"pending":{"id":"p"}});
    let created = json!({"kind":"created","action":{"id":"a","pending_id":"p","state":"prepared",
        "requires_new_intent_approval":true,"separate_from":"old"}});
    assert!(outcome::admit(&mut run, &created, &json!({})).is_none());
    assert_eq!(run["pending"]["action_id"], "a");
    assert_eq!(run["pending"]["new_intent_confirmation"], true);
    assert_eq!(outcome::pending_id(&run), "p");
    let conflict = json!({"kind":"conflict","action":null});
    assert_eq!(
        outcome::admit(&mut run, &conflict, &json!({})).unwrap()["status"],
        "denied"
    );
    let purchase = json!({"kind":"duplicate","action":{"id":"a","tool":"test_owner_purchase",
        "arguments_digest":"other","state":"completed"}});
    let refused = outcome::admit(
        &mut run,
        &purchase,
        &json!({"item":"x","_action":{"id":"n"}}),
    )
    .unwrap();
    assert_eq!(refused["status"], "new_intent_required");
    assert_eq!(refused["requested_action_id"], "n");
    assert!(!refused["duplicate_notice"].is_null());
}

#[test]
fn receipts_confirm_only_matching_success() {
    let action = json!({"arguments_digest":"h"});
    let good = json!({"arguments_digest":"h","result":{"status":"success"}});
    assert!(
        outcome::confirmed_result("prompt_slot_write", &action, &good)
            .unwrap()
            .is_some()
    );
    let other = json!({"arguments_digest":"x","result":{"status":"success"}});
    assert!(outcome::confirmed_result("prompt_slot_write", &action, &other).is_err());
    assert!(
        outcome::confirmed_result("edit_own_prompt", &action, &other)
            .unwrap()
            .is_some()
    );
    let failed = json!({"arguments_digest":"h","result":{"status":"denied"}});
    assert!(
        outcome::confirmed_result("prompt_slot_write", &action, &failed)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        receipts::key("a", "prompt_slot_write", "p"),
        "agent:a:action_prompt:p"
    );
    assert_eq!(
        receipts::key("a", "save_own_skill", "p"),
        "agent:a:base_self_edit:p"
    );
}

#[test]
fn recovery_walks_unsettled_batch_items_and_requeues_reads() {
    let mut run = json!({"pending":{"action_id":"single"},"batch":[
        {"state":"done","pending":{"name":"w","action_id":"x"}},
        {"state":"executing","pending":{"name":"w","action_id":"y"}},
        {"state":"executing","pending":{"name":"r"}},
        {"state":"uncertain","pending":{"name":"w","action_id":"z"}}]});
    let targets = outcome::recovery_targets(&run);
    assert_eq!(
        targets.iter().map(|t| t.batch_index).collect::<Vec<_>>(),
        vec![Some(1), Some(2), Some(3)]
    );
    assert_eq!(
        outcome::pending_action_ids(&run),
        vec!["single", "x", "y", "z"]
    );
    outcome::requeue_reads(&mut run, |name| name == "r");
    assert_eq!(run["batch"][2]["state"], "requested");
    assert_eq!(run["batch"][2]["pending"]["state"], "requested");
    assert_eq!(run["batch"][1]["state"], "executing");
    assert_eq!(
        outcome::recovery_targets(&json!({"pending":{"id":"p"}})),
        vec![RecoveryTarget {
            batch_index: None,
            pending: json!({"id":"p"})
        }]
    );
    assert!(outcome::recovered_result(&json!({"state":"running"})).is_none());
    let failed = json!({"id":"a","state":"confirmed_failed","result":{"status":"denied"}});
    assert_eq!(
        outcome::recovered_result(&failed).unwrap()["action"]["state"],
        "confirmed_failed"
    );
}

#[test]
fn ownership_and_new_intent_confirmation_belong_to_the_requester() {
    let action = json!({"requester":"u","tool":"edit_own_prompt","resource":"agent:a"});
    assert!(outcome::owned_by(&action, "u", "a"));
    assert!(!outcome::owned_by(&action, "v", "a"));
    assert!(!outcome::owned_by(&action, "u", "b"));
    let run = json!({"requester_user_id":"u","pending":{"new_intent_confirmation":true}});
    assert!(outcome::may_confirm_new_intent(&run, "owner", "u"));
    assert!(!outcome::may_confirm_new_intent(&run, "owner", "v"));
    let plain = json!({"pending":{}});
    assert!(outcome::may_confirm_new_intent(&plain, "owner", "v"));
    let defaulted = json!({"pending":{"new_intent_confirmation":true}});
    assert!(outcome::may_confirm_new_intent(
        &defaulted, "owner", "owner"
    ));
}

#[tokio::test]
async fn ledger_prepares_claims_and_fences_late_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(&dir).await;
    let ledger = Ledger::new(&TaggedCodec, &pool, "admin");
    let mut foreign = candidate("a", 1, "one");
    foreign["requester"] = json!("other");
    assert!(ledger.prepare(&foreign, None).await.is_err());
    assert_eq!(
        ledger
            .prepare(&candidate("a", 1, "one"), None)
            .await
            .unwrap()["kind"],
        "created"
    );
    assert_eq!(
        ledger
            .prepare(&candidate("a", 1, "one"), None)
            .await
            .unwrap()["kind"],
        "existing"
    );
    assert_eq!(
        ledger
            .prepare(&candidate("a", 1, "two"), None)
            .await
            .unwrap()["kind"],
        "conflict"
    );
    assert_eq!(
        ledger
            .prepare(&candidate("b", 1, "one"), None)
            .await
            .unwrap()["kind"],
        "duplicate"
    );
    assert_eq!(
        ledger
            .prepare(&candidate("c", 1, "new"), Some("a"))
            .await
            .unwrap()["kind"],
        "conflict"
    );
    let first = ledger.claim("a", false).await.unwrap();
    assert_eq!(first["claimed"], true);
    assert_eq!(ledger.claim("a", false).await.unwrap()["claimed"], false);
    let claim = first["action"]["claim"].as_str().unwrap();
    assert!(ledger
        .finish("a", claim, "bogus", &json!({}), &Value::Null, false)
        .await
        .is_err());
    assert!(ledger
        .finish(
            "a",
            claim,
            "confirmed_failed",
            &json!({"status":"denied"}),
            &Value::Null,
            true
        )
        .await
        .unwrap());
    let retry = ledger.claim("a", false).await.unwrap();
    assert_eq!(retry["action"]["attempt"], 2);
    assert!(!ledger
        .finish("a", claim, "completed", &json!({}), &Value::Null, false)
        .await
        .unwrap());
    ledger.interrupt("a").await.unwrap();
    assert_eq!(
        ledger.get("a").await.unwrap().unwrap()["state"],
        "uncertain"
    );
    assert_eq!(ledger.claim("a", true).await.unwrap()["claimed"], false);
    let found = ledger
        .find_intent(
            "agent:shared_default",
            candidate("z", 1, "one")["intent_key"].as_str().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found["id"], "a");
    assert_eq!(ledger.list("agent:shared_default").await.unwrap().len(), 1);
    let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='action:a'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(serde_json::from_slice::<Value>(&raw).is_err());
}

#[tokio::test]
async fn separate_intent_needs_fresh_origin_and_approval() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(&dir).await;
    let ledger = Ledger::new(&TaggedCodec, &pool, "admin");
    ledger
        .prepare(&candidate("a", 1, "one"), None)
        .await
        .unwrap();
    ledger.not_dispatched("a").await.unwrap();
    let failed = ledger.get("a").await.unwrap().unwrap();
    assert_eq!(failed["state"], "confirmed_failed");
    assert_eq!(failed["retry_safe"], true);
    let claim = ledger.claim("a", false).await.unwrap()["action"]["claim"].clone();
    ledger
        .finish(
            "a",
            claim.as_str().unwrap(),
            "completed",
            &json!({"status":"success"}),
            &Value::Null,
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        ledger
            .prepare(&candidate("b", 1, "one"), Some("a"))
            .await
            .unwrap()["kind"],
        "duplicate"
    );
    let b = ledger
        .prepare(&candidate("b", 2, "one"), Some("a"))
        .await
        .unwrap();
    assert_eq!(b["kind"], "created");
    assert_eq!(b["action"]["separate_from"], "a");
    assert_eq!(ledger.claim("b", false).await.unwrap()["claimed"], false);
    assert_eq!(ledger.claim("b", true).await.unwrap()["claimed"], true);
    // A still-prepared action is the only one marked not dispatched.
    ledger.not_dispatched("b").await.unwrap();
    assert_eq!(ledger.get("b").await.unwrap().unwrap()["state"], "running");
}

#[tokio::test]
async fn agent_receipts_are_sealed_once_per_key() {
    let dir = tempfile::tempdir().unwrap();
    let pool = pool(&dir).await;
    let key = receipts::key("a", "prompt_slot_write", "p");
    let mut tx = pool.begin().await.unwrap();
    assert!(receipts::read_in(&TaggedCodec, &mut tx, &key)
        .await
        .unwrap()
        .is_none());
    receipts::write_in(
        &TaggedCodec,
        &mut tx,
        &key,
        "h",
        &json!({"status":"success"}),
    )
    .await
    .unwrap();
    assert!(
        receipts::write_in(&TaggedCodec, &mut tx, &key, "h", &json!({}))
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let saved = receipts::read(&TaggedCodec, &pool, &key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        saved,
        json!({"arguments_digest":"h","result":{"status":"success"}})
    );
}
