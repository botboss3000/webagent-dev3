//! App-plane session directory; conversation contents remain in the owner's encrypted store.
use super::*;
use anyhow::bail;
impl Store {
    pub async fn shared_schema(&self) -> Result<()> {
        sqlx::raw_sql("CREATE TABLE IF NOT EXISTS shared_sessions(session_id TEXT PRIMARY KEY,owner_id TEXT NOT NULL,agent_id TEXT NOT NULL);
 CREATE TABLE IF NOT EXISTS shared_participants(session_id TEXT NOT NULL,user_id TEXT NOT NULL,joined_at INTEGER NOT NULL,read_through_seq INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(session_id,user_id));
 CREATE TABLE IF NOT EXISTS shared_invitations(id TEXT PRIMARY KEY,session_id TEXT NOT NULL,token_hash TEXT NOT NULL UNIQUE,issuer_id TEXT NOT NULL,request_id TEXT NOT NULL,expires_at INTEGER NOT NULL,requested_ttl INTEGER NOT NULL,redeemed_by TEXT,revoked INTEGER NOT NULL DEFAULT 0,UNIQUE(session_id,issuer_id,request_id));
 CREATE TABLE IF NOT EXISTS shared_observation_audit(session_id TEXT NOT NULL,actor_id TEXT NOT NULL,observed_at INTEGER NOT NULL);").execute(&self.app_pool).await?;
        Ok(())
    }
    pub async fn register_shared_session(&self, owner: &str, sid: &str, agent: &str) -> Result<()> {
        if !crate::access::valid_id(owner)
            || sid.is_empty()
            || sid.len() > 128
            || !crate::access::valid_agent_id(agent)
        {
            bail!("Invalid shared session");
        }
        self.shared_schema().await?;
        sqlx::query("INSERT OR IGNORE INTO shared_sessions VALUES(?,?,?)")
            .bind(sid)
            .bind(owner)
            .bind(agent)
            .execute(&self.app_pool)
            .await?;
        let dir = self.shared_directory(sid).await?.unwrap();
        if dir["owner_id"] != owner || dir["agent_id"] != agent {
            bail!("Session directory conflict");
        }
        Ok(())
    }
    pub async fn shared_directory(&self, sid: &str) -> Result<Option<Value>> {
        self.shared_schema().await?;
        Ok(sqlx::query("SELECT owner_id,agent_id FROM shared_sessions WHERE session_id=?").bind(sid).fetch_optional(&self.app_pool).await?.map(|r|json!({"session_id":sid,"owner_id":r.get::<String,_>("owner_id"),"agent_id":r.get::<String,_>("agent_id")})))
    }
    pub async fn shared_is_participant(&self, sid: &str, user: &str) -> Result<bool> {
        self.shared_schema().await?;
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM shared_participants WHERE session_id=? AND user_id=?",
        )
        .bind(sid)
        .bind(user)
        .fetch_optional(&self.app_pool)
        .await?
        .is_some())
    }
    pub async fn shared_sessions_for(&self, user: &str) -> Result<Vec<Value>> {
        self.shared_schema().await?;
        Ok(sqlx::query("SELECT s.session_id,s.owner_id,s.agent_id FROM shared_sessions s WHERE s.owner_id=? OR EXISTS(SELECT 1 FROM shared_participants p WHERE p.session_id=s.session_id AND p.user_id=?) ORDER BY s.session_id").bind(user).bind(user).fetch_all(&self.app_pool).await?.into_iter().map(|r|json!({"session_id":r.get::<String,_>("session_id"),"owner_id":r.get::<String,_>("owner_id"),"agent_id":r.get::<String,_>("agent_id")})).collect())
    }
    pub async fn shared_all_sessions(&self) -> Result<Vec<Value>> {
        self.shared_schema().await?;
        Ok(sqlx::query("SELECT session_id,owner_id,agent_id FROM shared_sessions ORDER BY session_id LIMIT 1000").fetch_all(&self.app_pool).await?.into_iter().map(|r|json!({"session_id":r.get::<String,_>("session_id"),"owner_id":r.get::<String,_>("owner_id"),"agent_id":r.get::<String,_>("agent_id")})).collect())
    }
    pub async fn shared_participants(&self, sid: &str) -> Result<Vec<Value>> {
        self.shared_schema().await?;
        let dir = self
            .shared_directory(sid)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        let mut members = vec![json!({"user_id":dir["owner_id"],"role":"owner"})];
        for r in sqlx::query("SELECT user_id,joined_at,read_through_seq FROM shared_participants WHERE session_id=? ORDER BY joined_at,user_id").bind(sid).fetch_all(&self.app_pool).await?{members.push(json!({"user_id":r.get::<String,_>("user_id"),"joined_at":r.get::<i64,_>("joined_at"),"read_through_seq":r.get::<i64,_>("read_through_seq"),"role":"participant"}));}
        Ok(members)
    }
    pub async fn shared_join(&self, sid: &str, user: &str) -> Result<bool> {
        self.shared_schema().await?;
        let directory = self
            .shared_directory(sid)
            .await?
            .ok_or_else(|| anyhow!("Session not found"))?;
        if directory["owner_id"] == user {
            return Ok(false);
        }
        Ok(sqlx::query(
            "INSERT OR IGNORE INTO shared_participants(session_id,user_id,joined_at) VALUES(?,?,?)",
        )
        .bind(sid)
        .bind(user)
        .bind(crate::now() as i64)
        .execute(&self.app_pool)
        .await?
        .rows_affected()
            == 1)
    }
    pub async fn shared_invite_issue(
        &self,
        sid: &str,
        issuer: &str,
        request: &str,
        ttl: u64,
    ) -> Result<Value> {
        self.shared_schema().await?;
        if request.is_empty() || request.len() > 128 || ttl == 0 || ttl > 30 * 86400 {
            bail!("Invalid invitation");
        }
        let mut tx = self.app_pool.begin().await?;
        // Reserve the app writer before key initialization and receipt lookup.
        let mut random = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut random);
        sqlx::query("INSERT OR IGNORE INTO docs(key,value) VALUES('shared_invitation_key',?)")
            .bind(self.seal(&json!(hex::encode(random)))?)
            .execute(&mut *tx)
            .await?;
        let raw: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM docs WHERE key='shared_invitation_key'")
                .fetch_one(&mut *tx)
                .await?;
        let secret = self.unseal(&raw)?;
        let secret = hex::decode(
            secret
                .as_str()
                .ok_or_else(|| anyhow!("Invalid invitation key"))?,
        )?;
        let existing=sqlx::query("SELECT id,expires_at,requested_ttl FROM shared_invitations WHERE session_id=? AND issuer_id=? AND request_id=?").bind(sid).bind(issuer).bind(request).fetch_optional(&mut *tx).await?;
        let (id, expires) = if let Some(row) = existing {
            if row.get::<i64, _>("requested_ttl") != ttl as i64 {
                bail!("Invitation request ID reused with different input");
            }
            (row.get::<String, _>("id"), row.get::<i64, _>("expires_at"))
        } else {
            (
                uuid::Uuid::new_v4().to_string(),
                crate::now()
                    .checked_add(ttl)
                    .ok_or_else(|| anyhow!("Expiration overflow"))? as i64,
            )
        };
        // HMAC-SHA256 permits identical retries without retaining plaintext tokens.
        let mut inner = [0x36u8; 64];
        let mut outer = [0x5cu8; 64];
        if secret.len() != 32 {
            bail!("Invalid invitation key");
        }
        for (i, b) in secret.iter().enumerate() {
            inner[i] ^= b;
            outer[i] ^= b;
        }
        let mut hash = Sha256::new();
        hash.update(inner);
        hash.update(serde_json::to_vec(&json!([
            "shared-chat-invitation-v1",
            id,
            sid,
            issuer,
            request,
            expires
        ]))?);
        let mut result = Sha256::new();
        result.update(outer);
        result.update(hash.finalize());
        let token = hex::encode(result.finalize());
        sqlx::query("INSERT OR IGNORE INTO shared_invitations(id,session_id,token_hash,issuer_id,request_id,expires_at,requested_ttl) VALUES(?,?,?,?,?,?,?)").bind(&id).bind(sid).bind(format!("{:x}",Sha256::digest(token.as_bytes()))).bind(issuer).bind(request).bind(expires).bind(ttl as i64).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"id":id,"token":token,"expires_at":expires}))
    }
    pub async fn shared_invites(&self, sid: &str) -> Result<Vec<Value>> {
        self.shared_schema().await?;
        Ok(sqlx::query("SELECT id,issuer_id,expires_at,redeemed_by,revoked FROM shared_invitations WHERE session_id=? ORDER BY expires_at DESC LIMIT 200").bind(sid).fetch_all(&self.app_pool).await?.into_iter().map(|r|json!({"id":r.get::<String,_>("id"),"issuer_id":r.get::<String,_>("issuer_id"),"expires_at":r.get::<i64,_>("expires_at"),"redeemed":r.get::<Option<String>,_>("redeemed_by").is_some(),"revoked":r.get::<i64,_>("revoked")==1})).collect())
    }
    pub async fn shared_invite_session(&self, token: &str) -> Result<Option<String>> {
        self.shared_schema().await?;
        if token.len() != 64 {
            return Ok(None);
        }
        Ok(sqlx::query_scalar("SELECT session_id FROM shared_invitations WHERE token_hash=? AND revoked=0 AND (redeemed_by IS NULL) AND expires_at>?").bind(format!("{:x}",Sha256::digest(token.as_bytes()))).bind(crate::now() as i64).fetch_optional(&self.app_pool).await?)
    }
    pub async fn shared_invite_redeem(&self, token: &str, user: &str) -> Result<(String, bool)> {
        self.shared_schema().await?;
        if token.len() != 64 {
            bail!("Invalid invitation");
        }
        let hash = format!("{:x}", Sha256::digest(token.as_bytes()));
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("UPDATE shared_invitations SET revoked=revoked WHERE token_hash=?")
            .bind(&hash)
            .execute(&mut *tx)
            .await?;
        let row=sqlx::query("SELECT session_id,redeemed_by,revoked,expires_at FROM shared_invitations WHERE token_hash=?").bind(&hash).fetch_optional(&mut *tx).await?.ok_or_else(||anyhow!("Invalid invitation"))?;
        let sid: String = row.get("session_id");
        let redeemed: Option<String> = row.get("redeemed_by");
        if redeemed.as_deref() == Some(user) {
            tx.commit().await?;
            return Ok((sid, false));
        }
        if redeemed.is_some()
            || row.get::<i64, _>("revoked") != 0
            || row.get::<i64, _>("expires_at") <= crate::now() as i64
        {
            bail!("Invitation expired or used");
        }
        sqlx::query("UPDATE shared_invitations SET redeemed_by=? WHERE token_hash=?")
            .bind(user)
            .bind(hash)
            .execute(&mut *tx)
            .await?;
        let added = sqlx::query(
            "INSERT OR IGNORE INTO shared_participants(session_id,user_id,joined_at) VALUES(?,?,?)",
        )
        .bind(&sid)
        .bind(user)
        .bind(crate::now() as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        tx.commit().await?;
        Ok((sid, added))
    }
    pub async fn shared_invite_revoke(&self, sid: &str, id: &str) -> Result<()> {
        self.shared_schema().await?;
        sqlx::query("UPDATE shared_invitations SET revoked=1 WHERE session_id=? AND id=? AND redeemed_by IS NULL").bind(sid).bind(id).execute(&self.app_pool).await?;
        Ok(())
    }
    pub async fn shared_observe_audit(&self, sid: &str, user: &str) -> Result<()> {
        self.shared_schema().await?;
        let mut tx = self.app_pool.begin().await?;
        sqlx::query("INSERT INTO shared_observation_audit VALUES(?,?,?)")
            .bind(sid)
            .bind(user)
            .bind(crate::now() as i64)
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM shared_observation_audit WHERE observed_at<? OR rowid NOT IN (SELECT rowid FROM shared_observation_audit ORDER BY observed_at DESC,rowid DESC LIMIT 100000)").bind(crate::now().saturating_sub(30*86400) as i64).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn shared_acknowledge_read(&self, sid: &str, user: &str, through: u64) -> Result<()> {
        self.shared_schema().await?;
        sqlx::query("UPDATE shared_participants SET read_through_seq=MAX(read_through_seq,?) WHERE session_id=? AND user_id=?").bind(through as i64).bind(sid).bind(user).execute(&self.app_pool).await?;
        Ok(())
    }
}

impl Store {
    pub async fn shared_help_request(
        &self,
        sid: &str,
        actor: &str,
        request: &str,
    ) -> Result<Value> {
        let key = format!("shared_help:{sid}");
        let receipt = format!("receipt:session:{sid}:help:{actor}:{request}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{sid}"))
            .execute(&mut *tx)
            .await?;
        if let Some(raw) = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&receipt)
            .fetch_optional(&mut *tx)
            .await?
        {
            tx.commit().await?;
            return self.unseal(&raw);
        }
        let current = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?
            .map(|raw| self.unseal(&raw))
            .transpose()?;
        let value = if let Some(current) = current.filter(|v| v["status"] == "pending") {
            current
        } else {
            let value = json!({"id":uuid::Uuid::new_v4().to_string(),"status":"pending","requested_by":actor,"requested_at":crate::now()});
            self.write_document(&mut tx, &key, &value).await?;
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'help_changed',?)")
                .bind(sid)
                .bind(self.encode_user_record(&value)?)
                .execute(&mut *tx)
                .await?;
            let event_key = format!("help:{}", value["id"].as_str().unwrap());
            self.stage_notification(
                &mut tx,
                &event_key,
                "session.help_requested",
                sid,
                None,
                Some("pending"),
            )
            .await?;
            sqlx::query("UPDATE notification_stage SET actor=? WHERE event_id=?")
                .bind(actor)
                .bind(format!("{}:{event_key}", self.user_id))
                .execute(&mut *tx)
                .await?;
            value
        };
        self.write_document(&mut tx, &receipt, &value).await?;
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key, receipt]);
        Ok(value)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn invitations_are_single_use_hashed_and_shared_directory_is_immutable() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .register_shared_session("admin", "chat-one", "shared_default")
            .await
            .unwrap();
        assert!(store
            .register_shared_session("other", "chat-one", "shared_default")
            .await
            .is_err());
        let issued = store
            .shared_invite_issue("chat-one", "admin", "req-one", 86400)
            .await
            .unwrap();
        let token = issued["token"].as_str().unwrap().to_owned();
        let stored: String = sqlx::query_scalar("SELECT token_hash FROM shared_invitations")
            .fetch_one(&store.app_pool)
            .await
            .unwrap();
        assert_ne!(stored, token);
        let (a, b) = tokio::join!(
            store.shared_invite_redeem(&token, "first"),
            store.shared_invite_redeem(&token, "second")
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(
            store.shared_participants("chat-one").await.unwrap().len(),
            2
        );
        assert_eq!(
            store.shared_sessions_for("unrelated").await.unwrap().len(),
            0
        );
    }
    #[tokio::test]
    async fn revoked_and_expired_invitations_fail() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .register_shared_session("admin", "chat-one", "shared_default")
            .await
            .unwrap();
        let issued = store
            .shared_invite_issue("chat-one", "admin", "req-one", 86400)
            .await
            .unwrap();
        store
            .shared_invite_revoke("chat-one", issued["id"].as_str().unwrap())
            .await
            .unwrap();
        assert!(store
            .shared_invite_redeem(issued["token"].as_str().unwrap(), "first")
            .await
            .is_err());
        let issued = store
            .shared_invite_issue("chat-one", "admin", "req-two", 1)
            .await
            .unwrap();
        sqlx::query("UPDATE shared_invitations SET expires_at=0")
            .execute(&store.app_pool)
            .await
            .unwrap();
        assert!(store
            .shared_invite_redeem(issued["token"].as_str().unwrap(), "first")
            .await
            .is_err());
    }
}

impl Store {
    pub async fn backfill_shared_directory(&self) -> Result<()> {
        self.shared_schema().await?;
        for user in self.all_user_ids().await? {
            let store = self.for_user(&user).await?;
            for session in store.list("session:").await? {
                let Some(sid) = session["id"].as_str() else {
                    continue;
                };
                self.register_shared_session(
                    &user,
                    sid,
                    session["agent_id"].as_str().unwrap_or("shared_default"),
                )
                .await?;
            }
        }
        Ok(())
    }
    pub async fn publish_shared_join(&self, sid: &str, user: &str, name: &str) -> Result<()> {
        let rid = format!("join:{sid}:{user}");
        let id = format!("working:{rid}:notice");
        let mut tx = self.pool.begin().await?;
        let existing: Option<i64> = sqlx::query_scalar("SELECT seq FROM chat WHERE id=?")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
        if existing.is_some() {
            tx.commit().await?;
            return Ok(());
        }
        self.insert_working_message(&mut tx, sid, &rid, &id, &format!("{name} joined this chat"))
            .await?;
        let meta = json!({"kind":"participant_joined","author_id":user,"author_name":name,"context_eligible":false,"queued":false});
        self.write_document(&mut tx, &format!("message_meta:{id}"), &meta)
            .await?;
        self.write_document(&mut tx, &format!("message_meta:{rid}"), &meta)
            .await?;
        // A stable publication receipt makes membership-first crash recovery replay safe.

        tx.commit().await?;
        Ok(())
    }
}

impl Store {
    pub async fn shared_invite_session_for(
        &self,
        token: &str,
        user: &str,
    ) -> Result<Option<String>> {
        self.shared_schema().await?;
        if token.len() != 64 {
            return Ok(None);
        }
        Ok(sqlx::query_scalar("SELECT session_id FROM shared_invitations WHERE token_hash=? AND ((revoked=0 AND redeemed_by IS NULL AND expires_at>?) OR redeemed_by=?)").bind(format!("{:x}",Sha256::digest(token.as_bytes()))).bind(crate::now() as i64).bind(user).fetch_optional(&self.app_pool).await?)
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    #[tokio::test]
    async fn invitation_retry_preserves_token_and_expiration_without_storing_token() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .register_shared_session("admin", "retry-chat", "shared_default")
            .await
            .unwrap();
        let one = store
            .shared_invite_issue("retry-chat", "admin", "stable", 3600)
            .await
            .unwrap();
        let two = store
            .shared_invite_issue("retry-chat", "admin", "stable", 3600)
            .await
            .unwrap();
        assert_eq!(one, two);
        assert!(store
            .shared_invite_issue("retry-chat", "admin", "stable", 7200)
            .await
            .is_err());
        let token = one["token"].as_str().unwrap();
        let redeemed = store.shared_invite_redeem(token, "alice").await.unwrap();
        assert!(redeemed.1);
        let retry = store.shared_invite_redeem(token, "alice").await.unwrap();
        assert!(!retry.1);
        assert_eq!(
            store
                .shared_invite_session_for(token, "alice")
                .await
                .unwrap()
                .as_deref(),
            Some("retry-chat")
        );
        assert!(store
            .shared_invite_session_for(token, "bob")
            .await
            .unwrap()
            .is_none());
        assert!(store.shared_invite_redeem(token, "bob").await.is_err());
    }
    #[tokio::test]
    async fn read_markers_are_personal_and_monotonic() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .register_shared_session("admin", "read-chat", "shared_default")
            .await
            .unwrap();
        store.shared_join("read-chat", "alice").await.unwrap();
        store.shared_join("read-chat", "bob").await.unwrap();
        store
            .shared_acknowledge_read("read-chat", "alice", 9)
            .await
            .unwrap();
        store
            .shared_acknowledge_read("read-chat", "alice", 3)
            .await
            .unwrap();
        let members = store.shared_participants("read-chat").await.unwrap();
        assert_eq!(
            members.iter().find(|v| v["user_id"] == "alice").unwrap()["read_through_seq"],
            9
        );
        assert_eq!(
            members.iter().find(|v| v["user_id"] == "bob").unwrap()["read_through_seq"],
            0
        );
    }
}

impl Store {
    pub async fn shared_help_finish(&self, sid: &str, status: &str, actor: &str) -> Result<Value> {
        if !matches!(status, "resolved" | "cancelled") {
            bail!("Invalid help state");
        }
        let key = format!("shared_help:{sid}");
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE docs SET key=key WHERE key=?")
            .bind(format!("session:{sid}"))
            .execute(&mut *tx)
            .await?;
        let raw = sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
            .bind(&key)
            .fetch_optional(&mut *tx)
            .await?;
        let mut value = raw
            .as_ref()
            .map(|raw| self.unseal(raw))
            .transpose()?
            .unwrap_or(json!({"status":"none"}));
        if value["status"] == "pending" {
            value["status"] = json!(status);
            value["resolved_by"] = json!(actor);
            value["resolved_at"] = json!(crate::now());
            self.write_document(&mut tx, &key, &value).await?;
            sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,'help_changed',?)")
                .bind(sid)
                .bind(self.encode_user_record(&value)?)
                .execute(&mut *tx)
                .await?;
            let kind = if status == "resolved" {
                "session.help_resolved"
            } else {
                "session.help_cancelled"
            };
            let event_key = format!(
                "help:{}:{status}",
                value["id"]
                    .as_str()
                    .ok_or_else(|| anyhow!("Help identity missing"))?
            );
            self.stage_notification(&mut tx, &event_key, kind, sid, None, Some(status))
                .await?;
            sqlx::query("UPDATE notification_stage SET actor=? WHERE event_id=?")
                .bind(actor)
                .bind(format!("{}:{event_key}", self.user_id))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        self.cache.lock().unwrap().invalidate(&[key]);
        Ok(value)
    }
}
