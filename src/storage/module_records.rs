//! Optional package data belongs to the authenticated user, always AEAD sealed.
use super::*;
impl Store {
    /// Only failed work with no consequential calls can retry as a new occurrence attempt.
    /// Read trusted private interactions from this user/run, never inherited history.
    pub(crate) async fn module_retry_safe(&self, run: &Value) -> Result<bool> {
        if run["status"] != "failed" || run["uncertain"] == true {
            return Ok(false);
        }
        if let Some(name) = run["pending"]["name"].as_str() {
            if !crate::tools::is_read(name) {
                return Ok(false);
            }
        }
        let Some(rid) = run["id"].as_str() else {
            return Ok(false);
        };
        let Some(sid) = run["session_id"].as_str() else {
            return Ok(false);
        };
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM interactions WHERE run_id=?")
            .bind(rid)
            .fetch_one(&self.pool)
            .await?;
        let largest: Option<i64> =
            sqlx::query_scalar("SELECT MAX(length(item)) FROM interactions WHERE run_id=?")
                .bind(rid)
                .fetch_one(&self.pool)
                .await?;
        if count > 512 || largest.is_some_and(|bytes| bytes > 65536) {
            return Ok(false);
        }
        let rows = sqlx::query(
            "SELECT session_id,item FROM interactions WHERE run_id=? ORDER BY seq LIMIT 512",
        )
        .bind(rid)
        .fetch_all(&self.pool)
        .await?;
        let mut calls = std::collections::HashSet::new();
        let mut outputs = std::collections::HashSet::new();
        for row in rows {
            if row.get::<String, _>("session_id") != sid {
                return Ok(false);
            }
            let item = self.unseal(&row.get::<Vec<u8>, _>("item"))?;
            if matches!(
                item["type"].as_str(),
                Some("function_call" | "function_call_output")
            ) {
                let Some(call_id) = item["call_id"].as_str().filter(|id| {
                    !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control)
                }) else {
                    return Ok(false);
                };
                if item["type"] == "function_call" {
                    if !item["name"].as_str().is_some_and(crate::tools::is_read)
                        || !calls.insert(call_id.to_owned())
                    {
                        return Ok(false);
                    }
                } else if !calls.contains(call_id) || !outputs.insert(call_id.to_owned()) {
                    return Ok(false);
                }
            }
        }
        // An interrupted authorized read needs no result to establish absence of
        // consequential effects; orphaned or duplicate results are ambiguous.
        Ok(true)
    }

    pub(crate) async fn module_state(&self, id: &str) -> Result<Value> {
        Ok(self
            .get(&format!("module_state:{id}"))
            .await?
            .unwrap_or_else(|| json!({"revision":0,"state":{},"pending":[]})))
    }
    pub(crate) async fn module_cas(&self, id: &str, expected: u64, value: &Value) -> Result<()> {
        if serde_json::to_vec(value)?.len() > 3 * 1024 * 1024 {
            return Err(anyhow!("Module state capacity reached"));
        }
        let key = format!("module_state:{id}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(&key)
            .execute(&mut *tx)
            .await?;
        let raw: Option<Vec<u8>> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
        let previous = raw.map(|raw| self.unseal(&raw)).transpose()?;
        if previous
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0)
            != expected
        {
            return Err(anyhow!("Module revision changed"));
        }
        let mut next = value.clone();
        next["revision"] = json!(expected + 1);
        // Never let administrator plaintext storage expose connector signing keys.
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value")
            .bind(&key).bind(self.seal(&next)?).execute(&mut *tx).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(())
    }
    pub(crate) async fn module_events(&self, after: i64) -> Result<Value> {
        let bounds = self.notification_page(after, 1).await?;
        if bounds["error"] == "cursor_expired" {
            return Ok(
                json!({"events":[],"error":"cursor_expired","pruned_through":bounds["pruned_through"],"latest_seq":bounds["latest_seq"],"next_seq":bounds["latest_seq"]}),
            );
        }
        let rows=sqlx::query("SELECT seq,event_id,type,session_id,run_id,agent_id,payload,created_at FROM notification_outbox WHERE source_user=? AND seq>? ORDER BY seq LIMIT 128")
            .bind(&self.user_id).bind(after).fetch_all(&self.app_pool).await?;
        let mut events = Vec::new();
        for row in rows {
            let payload: Value = serde_json::from_str(&row.get::<String, _>("payload"))?;
            let rid = row.get::<Option<String>, _>("run_id");
            let mut chain = Vec::new();
            let mut settled = json!({});
            if let Some(rid) = &rid {
                if let Some(run) = self.get(&format!("run:{rid}")).await? {
                    settled = json!({"outcome":run["status"],"is_incomplete":run["status"]!="completed","is_voluntary_stop":run["status"]=="cancelled"||run["error"]=="Cancelled by user"});
                    if let Some(automation) = run["module_binding"]["automation_id"].as_str() {
                        chain.extend(
                            run["module_binding"]["causal_chain"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(|v| v.as_str().map(str::to_owned)),
                        );
                        if !chain.iter().any(|id| id == automation) {
                            chain.push(automation.to_owned());
                        }
                    }
                }
            }
            events.push(json!({"id":row.get::<String,_>("event_id"),"kind":row.get::<String,_>("type"),"causal_chain":chain,"outcome":settled["outcome"],"is_incomplete":settled["is_incomplete"],"is_voluntary_stop":settled["is_voluntary_stop"],"seq":row.get::<i64,_>("seq"),"event_id":row.get::<String,_>("event_id"),"type":row.get::<String,_>("type"),"session_id":row.get::<String,_>("session_id"),"run_id":row.get::<Option<String>,_>("run_id"),"agent_id":row.get::<String,_>("agent_id"),"payload":payload,"created_at":row.get::<i64,_>("created_at")}));
        }
        let next = events
            .last()
            .and_then(|v| v["seq"].as_i64())
            .unwrap_or(after);
        Ok(json!({"events":events,"next_seq":next}))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn namespace_cas_is_encrypted_and_user_scoped() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .module_cas("automation", 0, &json!({"state":{"secret":"private"}}))
            .await
            .unwrap();
        assert!(store.module_cas("automation", 0, &json!({})).await.is_err());
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='module_state:automation'")
                .fetch_one(&store.pool)
                .await
                .unwrap();
        assert!(!String::from_utf8_lossy(&raw).contains("private"));
        assert_eq!(
            store.module_state("automation").await.unwrap()["revision"],
            1
        );
        let other = store.create_guest_account().await.unwrap();
        assert_eq!(
            store
                .for_user(other["user_id"].as_str().unwrap())
                .await
                .unwrap()
                .module_state("automation")
                .await
                .unwrap()["revision"],
            0
        );
    }

    fn failed(rid: &str) -> Value {
        json!({"id":rid,"session_id":"retry-session","status":"failed","uncertain":false})
    }
    async fn interaction(store: &Store, rid: &str, step: &str, item: Value) {
        let mut tx = store.pool.begin().await.unwrap();
        store
            .insert_interaction(&mut tx, "retry-session", rid, step, &item)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }
    #[tokio::test]
    async fn retry_safe_requires_failed_certain_work_and_ignores_inherited_calls() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let mut run = failed("no-effects");
        assert!(store.module_retry_safe(&run).await.unwrap());
        run["input"] =
            json!([{"type":"function_call","name":"file_write","call_id":"previous-run"}]);
        assert!(
            store.module_retry_safe(&run).await.unwrap(),
            "inherited history is not current-run evidence"
        );
        for status in [
            "queued",
            "running",
            "completed",
            "cancelled",
            "waiting_approval",
            "recovery",
        ] {
            run["status"] = json!(status);
            assert!(!store.module_retry_safe(&run).await.unwrap(), "{status}");
        }
        run["status"] = json!("failed");
        run["uncertain"] = json!(true);
        assert!(!store.module_retry_safe(&run).await.unwrap());
        run["uncertain"] = json!(false);
        run["pending"] = json!({"name":"file_write"});
        assert!(!store.module_retry_safe(&run).await.unwrap());
        run["pending"] = json!({"name":"file_read"});
        assert!(store.module_retry_safe(&run).await.unwrap());
        assert!(!store
            .module_retry_safe(&json!({"id":"missing-session","status":"failed"}))
            .await
            .unwrap());
    }
    #[tokio::test]
    async fn retry_safe_accepts_current_run_read_pairs_and_interrupted_reads() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        interaction(
            &store,
            "read",
            "call",
            json!({"type":"function_call","name":"file_read","call_id":"read-1"}),
        )
        .await;
        assert!(store.module_retry_safe(&failed("read")).await.unwrap());
        interaction(
            &store,
            "read",
            "output",
            json!({"type":"function_call_output","call_id":"read-1","output":"Evidence"}),
        )
        .await;
        assert!(store.module_retry_safe(&failed("read")).await.unwrap());
        interaction(
            &store,
            "unrelated",
            "write",
            json!({"type":"function_call","name":"file_write","call_id":"write-1"}),
        )
        .await;
        assert!(
            store.module_retry_safe(&failed("read")).await.unwrap(),
            "another run's write cannot contaminate this retry"
        );
    }
    #[tokio::test]
    async fn retry_safe_rejects_consequential_unknown_and_ambiguous_calls() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        for (rid, item) in [
            (
                "write",
                json!({"type":"function_call","name":"file_write","call_id":"c"}),
            ),
            (
                "unknown",
                json!({"type":"function_call","name":"uninstalled_tool","call_id":"c"}),
            ),
            (
                "missing-name",
                json!({"type":"function_call","call_id":"c"}),
            ),
            (
                "missing-id",
                json!({"type":"function_call","name":"file_read"}),
            ),
            (
                "empty-id",
                json!({"type":"function_call","name":"file_read","call_id":""}),
            ),
            (
                "oversized-id",
                json!({"type":"function_call","name":"file_read","call_id":"x".repeat(257)}),
            ),
            (
                "orphan",
                json!({"type":"function_call_output","call_id":"unknown","output":"Possibly an effect"}),
            ),
        ] {
            interaction(&store, rid, "item", item).await;
            assert!(
                !store.module_retry_safe(&failed(rid)).await.unwrap(),
                "{rid}"
            );
        }
        interaction(
            &store,
            "mismatch",
            "call",
            json!({"type":"function_call","name":"file_read","call_id":"read"}),
        )
        .await;
        interaction(&store,"mismatch","output",json!({"type":"function_call_output","call_id":"different","output":"Ambiguous evidence"})).await;
        assert!(!store.module_retry_safe(&failed("mismatch")).await.unwrap());
        for rid in ["duplicate-call", "duplicate-output"] {
            interaction(
                &store,
                rid,
                "call",
                json!({"type":"function_call","name":"file_read","call_id":"same"}),
            )
            .await;
            interaction(
                &store,
                rid,
                "output",
                json!({"type":"function_call_output","call_id":"same","output":"First"}),
            )
            .await;
            let duplicate = if rid == "duplicate-call" {
                json!({"type":"function_call","name":"file_read","call_id":"same"})
            } else {
                json!({"type":"function_call_output","call_id":"same","output":"Second"})
            };
            interaction(&store, rid, "duplicate", duplicate).await;
            assert!(!store.module_retry_safe(&failed(rid)).await.unwrap());
        }
    }
    #[tokio::test]
    async fn retry_safe_rejects_wrong_session_oversized_and_excessive_evidence() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        interaction(
            &store,
            "wrong-session",
            "call",
            json!({"type":"function_call","name":"file_read","call_id":"c"}),
        )
        .await;
        sqlx::query(
            "UPDATE interactions SET session_id='different-session' WHERE run_id='wrong-session'",
        )
        .execute(&store.pool)
        .await
        .unwrap();
        assert!(!store
            .module_retry_safe(&failed("wrong-session"))
            .await
            .unwrap());
        interaction(
            &store,
            "oversized",
            "message",
            json!({"type":"message","content":"x".repeat(65536)}),
        )
        .await;
        assert!(!store.module_retry_safe(&failed("oversized")).await.unwrap());
        let mut tx = store.pool.begin().await.unwrap();
        for index in 0..513 {
            store
                .insert_interaction(
                    &mut tx,
                    "retry-session",
                    "too-many",
                    &format!("message:{index}"),
                    &json!({"type":"message","content":"Bounded"}),
                )
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        assert!(!store.module_retry_safe(&failed("too-many")).await.unwrap());
    }
    #[tokio::test]
    async fn retry_safe_propagates_authenticated_record_damage() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        interaction(
            &store,
            "damaged",
            "item",
            json!({"type":"message","content":"Evidence"}),
        )
        .await;
        sqlx::query("UPDATE interactions SET item=? WHERE run_id='damaged'")
            .bind(vec![0u8; 32])
            .execute(&store.pool)
            .await
            .unwrap();
        assert!(
            store.module_retry_safe(&failed("damaged")).await.is_err(),
            "damaged evidence must not authorize a retry"
        );
    }
}
