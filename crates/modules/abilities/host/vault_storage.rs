//! Direct authenticated credential submission has its own nonce, revision and encrypted receipt.
use super::*;
impl Store {
    pub(crate) async fn core_save_credential(
        &self,
        sid: &str,
        name: &str,
        request: &str,
        expected: u64,
        value: &str,
    ) -> Result<Value> {
        if !crate::tools::core_vault::valid_name(name)
            || request.len() != 64
            || value.is_empty()
            || value.len() > 8192
            || value.contains('\0')
        {
            bail!("Invalid credential form");
        }
        let key = format!("core_vault:{name}");
        let receipt_key = format!("receipt:core_vault:{sid}:{request}");
        let fingerprint = crate::digest(&json!([name, expected, value]));
        let _guard = self.context_gate.write().await;
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("module_state:core_ui:{sid}"))
            .execute(&mut *tx)
            .await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt_key)
            .fetch_optional(&mut *tx)
            .await?
        {
            let receipt = self.unseal(&raw)?;
            if receipt["fingerprint"] != fingerprint {
                bail!("Credential request reused");
            }
            return Ok(receipt["output"].clone());
        }
        self.require_active_session(&mut tx, sid).await?;
        let raw: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key=?")
            .bind(format!("module_state:core_ui:{sid}"))
            .fetch_one(&mut *tx)
            .await?;
        let ui = self.unseal(&raw)?;
        let state = &ui["state"];
        if state["destination"] != "credential"
            || state["request_id"] != request
            || state["credential_name"] != name
            || state["actor_id"] != self.current_user_id()
            || state["expected_revision"] != expected
        {
            bail!("Credential request changed");
        }
        let old = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|v| self.unseal(&v))
            .transpose()?;
        if old
            .as_ref()
            .and_then(|v| v["revision"].as_u64())
            .unwrap_or(0)
            != expected
        {
            bail!("Credential revision changed");
        }
        if old.is_none() {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM docs WHERE substr(key,1,11)='core_vault:'",
            )
            .fetch_one(&mut *tx)
            .await?;
            if count >= 100 {
                bail!("Credential quota reached");
            }
        }
        let revision = expected
            .checked_add(1)
            .ok_or_else(|| anyhow!("Credential revision exhausted"))?;
        let record = json!({"name":name,"revision":revision,"origin":state["credential_origin"],"value":value});
        let output = json!({"saved":true,"name":name,"revision":revision,"secret_returned":false});
        self.write_credential_record(&mut tx, &key, &record).await?;
        // The form is spent: clear it so its card disappears on every device.
        let ui_key = format!("module_state:core_ui:{sid}");
        let mut spent = ui.clone();
        spent["state"] = json!({});
        spent["revision"] = json!(ui["revision"].as_u64().unwrap_or(0) + 1);
        self.write_credential_record(&mut tx, &ui_key, &spent).await?;
        self.write_credential_record(
            &mut tx,
            &receipt_key,
            &json!({"fingerprint":fingerprint,"output":output}),
        )
        .await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key, receipt_key, ui_key]);
        Ok(output)
    }
    async fn write_credential_record(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        key: &str,
        value: &Value,
    ) -> Result<()> {
        sqlx::query("INSERT INTO docs(key,value) VALUES(?,?) ON CONFLICT(key) DO UPDATE SET value=excluded.value").bind(key).bind(self.seal(value)?).execute(&mut **tx).await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn core_vault_submission_is_nonce_revision_and_user_bound() {
        let (_root, app) = crate::runtime_tests::fixture_uninitialized().await;
        let request = crate::digest(&json!(crate::uuid()));
        let value = crate::uuid();
        app.store
            .put(
                "session:source",
                &json!({"id":"source","status":"active","agent_id":"shared_default"}),
            )
            .await
            .unwrap();
        app.store.module_cas("core_ui:source",0,&json!({"state":{"request_id":request,"destination":"credential","credential_name":"example","credential_origin":"https://example.test","actor_id":"admin","expected_revision":0}})).await.unwrap();
        assert!(app
            .store
            .core_save_credential("source", "other", &request, 0, &value)
            .await
            .is_err());
        let output = app
            .store
            .core_save_credential("source", "example", &request, 0, &value)
            .await
            .unwrap();
        assert_eq!(output["revision"], 1);
        assert!(output.get("value").is_none());
        // The spent form is cleared so its card disappears everywhere; the replayed receipt still answers retries.
        let ui = app.store.module_state("core_ui:source").await.unwrap();
        assert_eq!(ui["state"], json!({}));
        assert_eq!(ui["revision"], 2);
        assert_eq!(
            app.store
                .core_save_credential("source", "example", &request, 0, &value)
                .await
                .unwrap(),
            output
        );
        assert!(app
            .store
            .core_save_credential("source", "example", &request, 0, "different")
            .await
            .is_err());
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='core_vault:example'")
                .fetch_one(&app.store.pool)
                .await
                .unwrap();
        assert!(!raw
            .windows(value.len())
            .any(|part| part == value.as_bytes()));
    }
}
