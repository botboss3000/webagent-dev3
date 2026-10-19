//! Requester-owned action evidence. SQLite, not a process mutex, owns claims.
use super::*;

impl Store {
    pub(super) async fn action_agent_receipt_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
    ) -> Result<Option<Value>> {
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(id)
            .fetch_optional(&mut **tx)
            .await?;
        raw.map(|v| self.unseal(&v)).transpose()
    }
    pub(super) async fn action_agent_receipt_write(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        id: &str,
        hash: &str,
        result: &Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?)")
            .bind(id)
            .bind(self.seal(&json!({"arguments_digest":hash,"result":result}))?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    pub(crate) async fn action_agent_receipt(
        &self,
        agent: &str,
        name: &str,
        pending: &str,
    ) -> Result<Option<Value>> {
        let scoped = self.for_agent(agent).await?;
        let key = if name == "prompt_slot_write" {
            format!("agent:{agent}:action_prompt:{pending}")
        } else {
            format!("agent:{agent}:base_self_edit:{pending}")
        };
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(key)
            .fetch_optional(&scoped.agent_pool)
            .await?;
        raw.map(|v| self.unseal(&v)).transpose()
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
            .ok_or_else(|| anyhow!("Missing provider receipt"))
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
    pub(crate) async fn action_origin(&self, sid: &str, rid: &str) -> Result<Value> {
        // A scheduled prompt is executable work, not a fresh human message.
        // It cannot authorize another matching consequential action.
        if self.get(&format!("run:{rid}")).await?
            .is_some_and(|run|run["module_binding"].is_object() || run["received_agent_message"]==true || run["origin_agent_message"].is_object()) {
            return Ok(Value::Null);
        }

        if self.get(&format!("message_meta:{rid}")).await?.is_some_and(|meta|meta["agent_message"].is_object()){return Ok(Value::Null);}
        let row = sqlx::query("SELECT id,seq,created_at FROM chat WHERE session_id=? AND run_id=? AND role='user' ORDER BY seq DESC LIMIT 1")
            .bind(sid).bind(rid).fetch_optional(&self.pool).await?;
        Ok(row.map(|row| json!({"storage_owner":self.user_id,"session_id":sid,"message_id":row.get::<String,_>("id"),"seq":row.get::<i64,_>("seq"),"created_at":row.get::<i64,_>("created_at")}))
            .unwrap_or(Value::Null))
    }
    pub(crate) async fn action_get(&self, id: &str) -> Result<Option<Value>> {
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("action:{id}"))
            .fetch_optional(&self.pool)
            .await?;
        raw.map(|raw| self.unseal(&raw)).transpose()
    }
    pub(crate) async fn action_match(&self, resource: &str, intent: &str) -> Result<Option<Value>> {
        let raw:Option<Vec<u8>>=sqlx::query_scalar("SELECT d.value FROM action_index i JOIN docs d ON d.key='action:'||i.id WHERE i.resource=? AND i.intent_key=? ORDER BY i.rowid DESC LIMIT 1")
            .bind(resource).bind(intent).fetch_optional(&self.pool).await?;
        raw.map(|v| self.unseal(&v)).transpose()
    }
    pub(crate) async fn action_list(&self, resource: &str) -> Result<Vec<Value>> {
        let ids: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM action_index WHERE resource=? ORDER BY rowid DESC LIMIT 32",
        )
        .bind(resource)
        .fetch_all(&self.pool)
        .await?;
        let mut records = Vec::new();
        for id in ids {
            records.push(
                self.action_get(&id)
                    .await?
                    .ok_or_else(|| anyhow!("Action index is incomplete"))?,
            );
        }
        Ok(records)
    }
    // Writer reservation comes before reading: competing connections cannot both
    // create a new intent, including when they supply different action IDs.
    pub(crate) async fn action_prepare(
        &self,
        candidate: &Value,
        separate_from: Option<&str>,
    ) -> Result<Value> {
        if candidate["requester"] != self.user_id {
            return Err(anyhow!("Action requester mismatch"));
        }
        let id = candidate["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action ID"))?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE action_lock SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("action:{id}"))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(raw) = raw {
            let old = self.unseal(&raw)?;
            if old["arguments_digest"] != candidate["arguments_digest"]
                || old["resource"] != candidate["resource"]
                || old["tool"] != candidate["tool"]
            {
                return Ok(json!({"kind":"conflict","action":Value::Null}));
            }
            return Ok(json!({"kind":"existing","action":old}));
        }
        let prior: Option<Vec<u8>> = sqlx::query_scalar("SELECT d.value FROM action_index i JOIN docs d ON d.key='action:'||i.id WHERE i.resource=? AND i.intent_key=? ORDER BY i.rowid DESC LIMIT 1")
            .bind(candidate["resource"].as_str()).bind(candidate["intent_key"].as_str()).fetch_optional(&mut *tx).await?;
        let mut record = candidate.clone();
        record["requires_new_intent_approval"] = json!(false);
        if let Some(raw) = prior {
            let old = self.unseal(&raw)?;
            if old["requester"] != self.user_id
                || old["resource"] != candidate["resource"]
                || old["intent_key"] != candidate["intent_key"]
            {
                return Err(anyhow!("Action index scope mismatch"));
            }
            let fresh = !candidate["origin"].is_null()
                && !old["origin"].is_null()
                && if candidate["origin"]["storage_owner"] == old["origin"]["storage_owner"] {
                    candidate["origin"]["seq"].as_i64().unwrap_or(0)
                        > old["origin"]["seq"].as_i64().unwrap_or(0)
                } else {
                    candidate["origin"]["created_at"].as_i64().unwrap_or(0)
                        > old["updated_at"].as_i64().unwrap_or(i64::MAX)
                };
            if separate_from != old["id"].as_str()
                || !fresh
                || matches!(
                    old["state"].as_str(),
                    Some("running" | "uncertain" | "prepared")
                )
            {
                return Ok(json!({"kind":"duplicate","action":old}));
            }
            record["requires_new_intent_approval"] = json!(true);
            record["separate_from"] = old["id"].clone();
        } else if separate_from.is_some() {
            return Ok(json!({"kind":"conflict","action":Value::Null}));
        }
        record["state"] = json!("prepared");
        record["attempt"] = json!(0);
        record["revision"] = json!(0);
        sqlx::query("INSERT INTO action_index(id,resource,intent_key,state) VALUES(?,?,?,?)")
            .bind(id)
            .bind(record["resource"].as_str())
            .bind(record["intent_key"].as_str())
            .bind("prepared")
            .execute(&mut *tx)
            .await?;
        self.action_write(&mut tx, &record).await?;
        tx.commit().await?;
        Ok(json!({"kind":"created","action":record}))
    }
    async fn action_write(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        action: &Value,
    ) -> Result<()> {
        let id = action["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Missing action identity"))?;
        // Always AEAD-sealed even when admin conversation encryption is disabled.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(format!("action:{id}")).bind(self.seal(action)?).execute(&mut **tx).await?;
        sqlx::query("UPDATE action_index SET state=? WHERE id=?")
            .bind(action["state"].as_str())
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
    pub(crate) async fn action_claim(&self, id: &str, approved: bool) -> Result<Value> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE action_lock SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("action:{id}"))
            .fetch_one(&mut *tx)
            .await?;
        let mut action = self.unseal(&raw)?;
        let retry = action["state"] == "confirmed_failed" && action["retry_safe"] == true;
        if (action["state"] != "prepared" && !retry)
            || (action["requires_new_intent_approval"] == true && !approved)
        {
            return Ok(json!({"claimed":false,"action":action}));
        }
        action["state"] = json!("running");
        action["attempt"] = json!(action["attempt"].as_u64().unwrap_or(0) + 1);
        action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
        action["claim"] = json!(crate::uuid());
        action["updated_at"] = json!(crate::now());
        self.action_write(&mut tx, &action).await?;
        tx.commit().await?;
        Ok(json!({"claimed":true,"action":action}))
    }
    pub(crate) async fn action_finish(
        &self,
        id: &str,
        claim: &str,
        state: &str,
        result: &Value,
        receipt: &Value,
        retry_safe: bool,
    ) -> Result<bool> {
        if !matches!(state, "completed" | "confirmed_failed" | "uncertain") {
            return Err(anyhow!("Invalid action outcome"));
        }
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE action_lock SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("action:{id}"))
            .fetch_one(&mut *tx)
            .await?;
        let mut action = self.unseal(&raw)?;
        if action["claim"] != claim
            || !matches!(action["state"].as_str(), Some("running" | "uncertain"))
        {
            return Ok(false);
        }
        action["state"] = json!(state);
        action["result"] = result.clone();
        action["receipt"] = receipt.clone();
        action["retry_safe"] = json!(state == "confirmed_failed" && retry_safe);
        action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
        action["updated_at"] = json!(crate::now());
        self.action_write(&mut tx, &action).await?;
        tx.commit().await?;
        Ok(true)
    }
    pub(crate) async fn action_not_dispatched(&self, id: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE action_lock SET revision=revision+1 WHERE singleton=1")
            .execute(&mut *tx)
            .await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("action:{id}"))
            .fetch_optional(&mut *tx)
            .await?;
        if let Some(raw) = raw {
            let mut action = self.unseal(&raw)?;
            if action["state"] == "prepared" {
                action["state"] = json!("confirmed_failed");
                action["retry_safe"] = json!(true);
                action["result"] =
                    json!({"status":"denied","output":"Operation was not dispatched"});
                action["revision"] = json!(action["revision"].as_u64().unwrap_or(0) + 1);
                self.action_write(&mut tx, &action).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }
    pub(crate) async fn action_interrupt(&self, id: &str) -> Result<()> {
        if let Some(action) = self.action_get(id).await? {
            if action["state"] == "running" {
                self.action_finish(
                    id,
                    action["claim"].as_str().unwrap_or(""),
                    "uncertain",
                    &Value::Null,
                    &Value::Null,
                    false,
                )
                .await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
