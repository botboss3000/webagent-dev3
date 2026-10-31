//! Store adapters for the Actions ledger: the requester's user plane and the agent receipt
//! plane are handed to `webagent_actions` with the Store codec. SQLite, not a process mutex,
//! owns claims; see `webagent_actions::repository`.
use super::Store;
use anyhow::Result;
use serde_json::Value;
use sqlx::Row;
use webagent_actions::intent;
use webagent_actions::receipts;
use webagent_actions::repository::Ledger;

impl Store {
    fn action_ledger(&self) -> Ledger<'_, Self> {
        Ledger::new(self, &self.pool, &self.user_id)
    }
    /// Native receipt `id`, read in the caller's agent transaction.
    pub(super) async fn action_agent_receipt_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
    ) -> Result<Option<Value>> {
        receipts::read_in(self, tx, id).await
    }
    /// Write native receipt `id` in the caller's agent transaction.
    pub(super) async fn action_agent_receipt_write(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
        hash: &str,
        result: &Value,
    ) -> Result<()> {
        receipts::write_in(self, tx, id, hash, result).await
    }
    /// Native receipt of an action of `name` for `agent` and pending call `pending`.
    pub(crate) async fn action_agent_receipt(
        &self,
        agent: &str,
        name: &str,
        pending: &str,
    ) -> Result<Option<Value>> {
        let scoped = self.for_agent(agent).await?;
        // The receipt is read from the agent plane and unsealed with this handle's codec.
        receipts::read(
            self,
            &scoped.agent_pool,
            &receipts::key(agent, name, pending),
        )
        .await
    }
    #[cfg(test)]
    pub(crate) async fn test_provider_purchase(&self, key: &str, result: &Value) -> Result<Value> {
        sqlx::query("CREATE TABLE IF NOT EXISTS test_action_provider(idempotency_key TEXT PRIMARY KEY,result BLOB NOT NULL)").execute(&self.app_pool).await?;
        sqlx::query("INSERT OR IGNORE INTO test_action_provider VALUES(?,?)")
            .bind(key)
            .bind(self.seal(result)?)
            .execute(&self.app_pool)
            .await?;
        self.test_provider_lookup(key)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Missing provider receipt"))
    }
    #[cfg(test)]
    pub(crate) async fn test_provider_lookup(&self, key: &str) -> Result<Option<Value>> {
        sqlx::query("CREATE TABLE IF NOT EXISTS test_action_provider(idempotency_key TEXT PRIMARY KEY,result BLOB NOT NULL)").execute(&self.app_pool).await?;
        let raw: Option<Vec<u8>> =
            sqlx::query_scalar("SELECT result FROM test_action_provider WHERE idempotency_key=?")
                .bind(key)
                .fetch_optional(&self.app_pool)
                .await?;
        raw.map(|v| self.unseal(&v)).transpose()
    }
    #[cfg(test)]
    pub(crate) async fn test_provider_effect_count(&self) -> Result<i64> {
        Ok(
            sqlx::query_scalar("SELECT COUNT(*) FROM test_action_provider")
                .fetch_one(&self.app_pool)
                .await?,
        )
    }
    /// The human message that authorized run `rid`, or `null` for automated work: a scheduled
    /// prompt or agent message cannot authorize another matching consequential action.
    pub(crate) async fn action_origin(&self, sid: &str, rid: &str) -> Result<Value> {
        if self
            .get(&format!("run:{rid}"))
            .await?
            .is_some_and(|run| intent::automated_run(&run))
        {
            return Ok(Value::Null);
        }
        if self
            .get(&format!("message_meta:{rid}"))
            .await?
            .is_some_and(|meta| intent::agent_message(&meta))
        {
            return Ok(Value::Null);
        }
        let row = sqlx::query("SELECT id,seq,created_at FROM chat WHERE session_id=? AND run_id=? AND role='user' ORDER BY seq DESC LIMIT 1")
            .bind(sid).bind(rid).fetch_optional(&self.pool).await?;
        Ok(row
            .map(|row| {
                intent::origin(
                    &self.user_id,
                    sid,
                    &row.get::<String, _>("id"),
                    row.get::<i64, _>("seq"),
                    row.get::<i64, _>("created_at"),
                )
            })
            .unwrap_or(Value::Null))
    }
    /// See [`Ledger::get`].
    pub(crate) async fn action_get(&self, id: &str) -> Result<Option<Value>> {
        self.action_ledger().get(id).await
    }
    /// See [`Ledger::find_intent`].
    pub(crate) async fn action_match(&self, resource: &str, intent: &str) -> Result<Option<Value>> {
        self.action_ledger().find_intent(resource, intent).await
    }
    /// See [`Ledger::list`].
    pub(crate) async fn action_list(&self, resource: &str) -> Result<Vec<Value>> {
        self.action_ledger().list(resource).await
    }
    /// See [`Ledger::prepare`].
    pub(crate) async fn action_prepare(
        &self,
        candidate: &Value,
        separate_from: Option<&str>,
    ) -> Result<Value> {
        self.action_ledger().prepare(candidate, separate_from).await
    }
    /// See [`Ledger::claim`].
    pub(crate) async fn action_claim(&self, id: &str, approved: bool) -> Result<Value> {
        self.action_ledger().claim(id, approved).await
    }
    /// See [`Ledger::finish`].
    pub(crate) async fn action_finish(
        &self,
        id: &str,
        claim: &str,
        state: &str,
        result: &Value,
        receipt: &Value,
        retry_safe: bool,
    ) -> Result<bool> {
        self.action_ledger()
            .finish(id, claim, state, result, receipt, retry_safe)
            .await
    }
    /// See [`Ledger::not_dispatched`].
    pub(crate) async fn action_not_dispatched(&self, id: &str) -> Result<()> {
        self.action_ledger().not_dispatched(id).await
    }
    /// See [`Ledger::interrupt`].
    pub(crate) async fn action_interrupt(&self, id: &str) -> Result<()> {
        self.action_ledger().interrupt(id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use webagent_accounts_host::AccountsStore;
    fn candidate(id: &str, seq: i64) -> Value {
        json!({"schema_version":1,"id":id,"requester":"admin","tool":"edit_own_prompt","resource":"agent:shared_default",
            "agent_id":"shared_default","session_id":"s","run_id":"r","pending_id":id,
            "arguments_digest":"original-arguments","intent_key":"same-intent","created_at":1,
            "origin":{"storage_owner":"admin","message_id":format!("user-{seq}"),"seq":seq,"created_at":1}})
    }
    #[tokio::test]
    async fn requester_and_actual_resource_boundaries_keep_separate_actions() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        first
            .action_prepare(&candidate("same-id", 1), None)
            .await
            .unwrap();
        let account = first
            .create_user_account("other_action_owner", "fixture-only-password-hash")
            .await
            .unwrap();
        let second = first
            .for_user(account["user_id"].as_str().unwrap())
            .await
            .unwrap();
        assert!(second.action_get("same-id").await.unwrap().is_none());
        let mut other = candidate("same-id", 1);
        other["requester"] = json!(second.current_user_id());
        assert!(first.action_prepare(&other, None).await.is_err());
        assert_eq!(
            second.action_prepare(&other, None).await.unwrap()["kind"],
            "created"
        );
        let mut resource = candidate("different-resource", 1);
        resource["resource"] = json!("agent:other");
        resource["agent_id"] = json!("other");
        assert_eq!(
            first.action_prepare(&resource, None).await.unwrap()["kind"],
            "created"
        );
        assert_eq!(
            first
                .action_list("agent:shared_default")
                .await
                .unwrap()
                .len(),
            1
        );
    }
    #[tokio::test]
    async fn independent_connections_different_ids_and_claims_execute_once() {
        let root = tempfile::tempdir().unwrap();
        let first = Store::open(root.path()).await.unwrap();
        let second = Store::open(root.path()).await.unwrap();
        let ca = candidate("a", 1);
        let cb = candidate("b", 1);
        let (a, b) = tokio::join!(
            first.action_prepare(&ca, None),
            second.action_prepare(&cb, None)
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_ne!(a["kind"], b["kind"]);
        let winner = if a["kind"] == "created" {
            a["action"].clone()
        } else {
            b["action"].clone()
        };
        let id = winner["id"].as_str().unwrap();
        let (a, b) = tokio::join!(
            first.action_claim(id, false),
            second.action_claim(id, false)
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_ne!(a["claimed"], b["claimed"]);
        assert_eq!(first.action_get(id).await.unwrap().unwrap()["attempt"], 1);
    }
    #[tokio::test]
    async fn durable_success_conflict_uncertainty_safe_retry_and_late_fence() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .action_prepare(&candidate("a", 1), None)
            .await
            .unwrap();
        let first = store.action_claim("a", false).await.unwrap()["action"].clone();
        store
            .action_finish(
                "a",
                first["claim"].as_str().unwrap(),
                "confirmed_failed",
                &json!({"status":"denied"}),
                &Value::Null,
                true,
            )
            .await
            .unwrap();
        let next = store.action_claim("a", false).await.unwrap()["action"].clone();
        assert_eq!(next["attempt"], 2);
        assert!(!store
            .action_finish(
                "a",
                first["claim"].as_str().unwrap(),
                "completed",
                &json!({}),
                &Value::Null,
                false
            )
            .await
            .unwrap());
        store.action_interrupt("a").await.unwrap();
        assert_eq!(
            store.action_claim("a", true).await.unwrap()["claimed"],
            false
        );
        let reopened = Store::open(root.path()).await.unwrap();
        assert_eq!(
            reopened.action_get("a").await.unwrap().unwrap()["state"],
            "uncertain"
        );
        reopened
            .action_finish(
                "a",
                next["claim"].as_str().unwrap(),
                "completed",
                &json!({"status":"success","output":"saved-result"}),
                &json!({"external_id":"receipt-1"}),
                false,
            )
            .await
            .unwrap();
        let replay = reopened
            .action_prepare(&candidate("a", 1), None)
            .await
            .unwrap();
        assert_eq!(
            crate::actions::response(&replay["action"])["output"],
            "saved-result"
        );
        let mut changed = candidate("a", 1);
        changed["arguments_digest"] = json!("different");
        assert_eq!(
            reopened.action_prepare(&changed, None).await.unwrap()["kind"],
            "conflict"
        );
        assert_eq!(
            reopened
                .action_prepare(&candidate("new-id", 1), None)
                .await
                .unwrap()["kind"],
            "duplicate"
        );
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='action:a'")
            .fetch_one(&reopened.pool)
            .await
            .unwrap();
        assert!(serde_json::from_slice::<Value>(&raw).is_err());
    }
    #[tokio::test]
    async fn separate_intent_requires_new_source_and_specific_approval() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        s.action_prepare(&candidate("a", 1), None).await.unwrap();
        let claim = s.action_claim("a", false).await.unwrap()["action"].clone();
        assert_eq!(
            s.action_prepare(&candidate("b", 2), Some("a"))
                .await
                .unwrap()["kind"],
            "duplicate"
        );
        s.action_finish(
            "a",
            claim["claim"].as_str().unwrap(),
            "completed",
            &json!({"status":"success"}),
            &Value::Null,
            false,
        )
        .await
        .unwrap();
        assert_eq!(
            s.action_prepare(&candidate("b", 1), Some("a"))
                .await
                .unwrap()["kind"],
            "duplicate"
        );
        let b = s
            .action_prepare(&candidate("b", 2), Some("a"))
            .await
            .unwrap();
        assert_eq!(b["kind"], "created");
        assert_eq!(b["action"]["requires_new_intent_approval"], true);
        assert_eq!(s.action_claim("b", false).await.unwrap()["claimed"], false);
        assert_eq!(s.action_claim("b", true).await.unwrap()["claimed"], true);
        let mut different = candidate("different-effect", 1);
        different["intent_key"] = json!("different-intent");
        assert_eq!(
            s.action_prepare(&different, None).await.unwrap()["kind"],
            "created"
        );
    }
    #[tokio::test]
    async fn native_prompt_receipt_is_atomic_and_survives_delete_and_reopen() {
        let root = tempfile::tempdir().unwrap();
        let s = Store::open(root.path()).await.unwrap();
        let receipt = "agent:shared_default:action_prompt:write";
        let slot = s
            .upsert_prompt_slot_receipted(
                "notes",
                "once",
                None,
                None,
                None,
                0,
                Some((receipt, "hash")),
            )
            .await
            .unwrap();
        assert_eq!(
            s.upsert_prompt_slot_receipted(
                "notes",
                "once",
                None,
                None,
                None,
                0,
                Some((receipt, "hash"))
            )
            .await
            .unwrap(),
            slot
        );
        assert!(s
            .upsert_prompt_slot_receipted(
                "notes",
                "changed",
                None,
                None,
                None,
                0,
                Some((receipt, "other"))
            )
            .await
            .is_err());
        let receipt = "agent:shared_default:action_prompt:delete";
        assert!(s
            .delete_prompt_slot_receipted("notes", 1, Some((receipt, "delete-hash")))
            .await
            .unwrap());
        let reopened = Store::open(root.path()).await.unwrap();
        assert!(reopened
            .delete_prompt_slot_receipted("notes", 1, Some((receipt, "delete-hash")))
            .await
            .unwrap());
        assert!(reopened.get_prompt_slot("notes").await.unwrap().is_none());
        let record = reopened
            .action_agent_receipt("shared_default", "prompt_slot_write", "delete")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record["result"]["output"]["deleted"], true);
    }
}

#[cfg(test)]
mod module_origin_tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn automatic_prompt_is_not_new_human_authorization() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .put(
                "session:s",
                &json!({"id":"s","agent_id":"shared_default","status":"active"}),
            )
            .await
            .unwrap();
        sqlx::query("INSERT INTO chat(id,session_id,run_id,role,content,created_at) VALUES('m','s','r','user',?,1)")
            .bind(store.seal(&json!("A human command")).unwrap()).execute(&store.pool).await.unwrap();
        assert!(!store.action_origin("s", "r").await.unwrap().is_null());
        store
            .put(
                "run:r",
                &json!({"id":"r","session_id":"s","module_binding":{"id":"automation"}}),
            )
            .await
            .unwrap();
        assert!(store.action_origin("s", "r").await.unwrap().is_null());
    }
}
