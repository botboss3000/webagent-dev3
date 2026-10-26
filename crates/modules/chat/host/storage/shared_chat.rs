//! App-plane session directory; conversation contents remain in the owner's encrypted store.
use super::*;
use anyhow::bail;
impl Store {
    /// App-plane directory repository owned by Chat; this Store supplies the pool and codec.
    fn directory_repository(&self) -> webagent_chat::shared::DirectoryRepository<'_, Store> {
        webagent_chat::shared::DirectoryRepository::new(&self.app_pool, self)
    }
    pub async fn shared_schema(&self) -> Result<()> {
        self.directory_repository().schema().await
    }
    pub async fn register_shared_session(&self, owner: &str, sid: &str, agent: &str) -> Result<()> {
        self.directory_repository().register_session(owner, sid, agent).await
    }
    pub async fn shared_directory(&self, sid: &str) -> Result<Option<Value>> {
        self.directory_repository().directory(sid).await
    }
    pub async fn shared_is_participant(&self, sid: &str, user: &str) -> Result<bool> {
        self.directory_repository().is_participant(sid, user).await
    }
    pub async fn shared_sessions_for(&self, user: &str) -> Result<Vec<Value>> {
        self.directory_repository().sessions_for(user).await
    }
    pub async fn shared_all_sessions(&self) -> Result<Vec<Value>> {
        self.directory_repository().all_sessions().await
    }
    pub async fn shared_participants(&self, sid: &str) -> Result<Vec<Value>> {
        self.directory_repository().participants(sid).await
    }
    pub async fn shared_join(&self, sid: &str, user: &str) -> Result<bool> {
        self.directory_repository().join(sid, user, crate::now()).await
    }
    pub async fn shared_invite_issue(&self, sid: &str, issuer: &str, request: &str, ttl: u64) -> Result<Value> {
        self.directory_repository().invite_issue(sid, issuer, request, ttl, crate::now()).await
    }
    pub async fn shared_invites(&self, sid: &str) -> Result<Vec<Value>> {
        self.directory_repository().invites(sid).await
    }
    pub async fn shared_invite_session(&self, token: &str) -> Result<Option<String>> {
        self.directory_repository().invite_session(token, crate::now()).await
    }
    pub async fn shared_invite_redeem(&self, token: &str, user: &str) -> Result<(String, bool)> {
        self.directory_repository().invite_redeem(token, user, crate::now()).await
    }
    pub async fn shared_invite_revoke(&self, sid: &str, id: &str) -> Result<()> {
        self.directory_repository().invite_revoke(sid, id).await
    }
    pub async fn shared_observe_audit(&self, sid: &str, user: &str) -> Result<()> {
        self.directory_repository().observe_audit(sid, user, crate::now()).await
    }
    pub async fn shared_acknowledge_read(&self, sid: &str, user: &str, through: u64) -> Result<()> {
        self.directory_repository().acknowledge_read(sid, user, through).await
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
    pub async fn shared_invite_session_for(&self, token: &str, user: &str) -> Result<Option<String>> {
        self.directory_repository().invite_session_for(token, user, crate::now()).await
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
