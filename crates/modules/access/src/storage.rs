//! Borrowed, already scoped resources. Storage retains keys, routing and live encryption policy.
use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::SqlitePool;
use std::path::Path;
/// Existing Storage encoding operations only; Access SQL and decisions are never callbacks.
pub trait RecordCodec: Sync {
    fn seal(&self, value: &Value) -> Result<Vec<u8>>;
    fn unseal(&self, value: &[u8]) -> Result<Value>;
    fn encode_application(&self, value: &Value) -> Result<Vec<u8>>;
    fn seal_agent(&self, value: &Value) -> Result<Vec<u8>> {
        self.seal(value)
    }
    fn unseal_agent(&self, value: &[u8]) -> Result<Value> {
        self.unseal(value)
    }
    fn seal_agent_for(&self, _agent: &str, value: &Value) -> Result<Vec<u8>> {
        self.seal_agent(value)
    }
    fn unseal_agent_for(&self, _agent: &str, value: &[u8]) -> Result<Value> {
        self.unseal_agent(value)
    }
}
pub(crate) struct AccountsCodec<'a, C: RecordCodec>(&'a C);
impl<C: RecordCodec> webagent_accounts::RecordCodec for AccountsCodec<'_, C> {
    fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.0.seal(value)
    }
    fn unseal(&self, value: &[u8]) -> Result<Value> {
        self.0.unseal(value)
    }
}
pub struct Repository<'a, C: RecordCodec> {
    pub(crate) root: &'a Path,
    pub(crate) app_pool: &'a SqlitePool,
    pub(crate) agent_pool: &'a SqlitePool,
    agent_id: &'a str,
    actor_id: &'a str,
    codec: &'a C,
    accounts_codec: AccountsCodec<'a, C>,
}
impl<'a, C: RecordCodec> Repository<'a, C> {
    /// Trusted root composition supplies existing pools with their bound agent identity.
    pub fn new(
        root: &'a Path,
        app_pool: &'a SqlitePool,
        agent_pool: &'a SqlitePool,
        agent_id: &'a str,
        actor_id: &'a str,
        codec: &'a C,
    ) -> Self {
        Self {
            root,
            app_pool,
            agent_pool,
            agent_id,
            actor_id,
            codec,
            accounts_codec: AccountsCodec(codec),
        }
    }
    pub(crate) async fn for_agent(&self, id: &str) -> Result<&Self> {
        if id != self.agent_id {
            return Err(anyhow!("Agent storage scope mismatch"));
        }
        Ok(self)
    }
    pub(crate) fn current_user_id(&self) -> &str {
        self.actor_id
    }
    pub(crate) fn seal(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.seal(value)
    }
    pub(crate) fn unseal(&self, value: &[u8]) -> Result<Value> {
        self.codec.unseal(value)
    }
    pub(crate) fn seal_agent(&self, value: &Value) -> Result<Vec<u8>> {
        self.codec.seal_agent_for(self.agent_id, value)
    }
    pub(crate) fn unseal_agent(&self, value: &[u8]) -> Result<Value> {
        self.codec.unseal_agent_for(self.agent_id, value)
    }
    pub(crate) fn accounts(&self) -> webagent_accounts::Repository<'_, &AccountsCodec<'a, C>> {
        webagent_accounts::Repository::new(self.app_pool, &self.accounts_codec)
    }
    pub async fn account_by_id(&self, id: &str) -> Result<Option<Value>> {
        self.accounts().account_state(id).await
    }
    pub(crate) async fn event(&self, sid: &str, kind: &str, payload: &Value) -> Result<()> {
        debug_assert!(sid.is_empty());
        sqlx::query("INSERT INTO events(session_id,kind,payload) VALUES(?,?,?)")
            .bind(sid)
            .bind(kind)
            .bind(self.codec.encode_application(payload)?)
            .execute(self.app_pool)
            .await?;
        Ok(())
    }
    pub async fn tier_user_ids(&self, tier: &str) -> Result<Vec<String>> {
        let account_ids = self.accounts().all_user_ids().await?;
        let mut ids = Vec::new();
        for id in account_ids {
            if self
                .account_by_id(&id)
                .await?
                .is_some_and(|account| account["tier_id"] == tier)
            {
                ids.push(id);
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use crate::{AccountStatus, Permission, Relationship, Role};
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Codec {
        cipher: Aes256Gcm,
        nonce: AtomicU64,
    }
    impl Codec {
        fn new() -> Self {
            Self {
                cipher: Aes256Gcm::new_from_slice(&[19; 32]).unwrap(),
                nonce: AtomicU64::new(1),
            }
        }
    }
    impl RecordCodec for Codec {
        fn seal(&self, v: &Value) -> Result<Vec<u8>> {
            let mut nonce = [0; 12];
            nonce[4..].copy_from_slice(&self.nonce.fetch_add(1, Ordering::Relaxed).to_be_bytes());
            let mut data = nonce.to_vec();
            data.extend(
                self.cipher
                    .encrypt(Nonce::from_slice(&nonce), serde_json::to_vec(v)?.as_ref())
                    .map_err(|_| anyhow!("Encryption failed"))?,
            );
            Ok(data)
        }
        fn unseal(&self, v: &[u8]) -> Result<Value> {
            if v.len() < 28 {
                return Err(anyhow!("Invalid encrypted record"));
            }
            let bytes = self
                .cipher
                .decrypt(Nonce::from_slice(&v[..12]), &v[12..])
                .map_err(|_| anyhow!("Storage authentication failed"))?;
            Ok(serde_json::from_slice(&bytes)?)
        }
        fn encode_application(&self, v: &Value) -> Result<Vec<u8>> {
            self.seal(v)
        }
    }
    struct Fixture {
        root: tempfile::TempDir,
        app: SqlitePool,
        agent: SqlitePool,
        codec: Codec,
    }
    impl Fixture {
        fn repo(&self) -> Repository<'_, Codec> {
            Repository::new(
                self.root.path(),
                &self.app,
                &self.agent,
                "shared_default",
                "admin",
                &self.codec,
            )
        }
        async fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            async fn pool(path: std::path::PathBuf) -> SqlitePool {
                SqlitePoolOptions::new()
                    .max_connections(1)
                    .connect_with(
                        SqliteConnectOptions::new()
                            .filename(path)
                            .create_if_missing(true),
                    )
                    .await
                    .unwrap()
            }
            let app = pool(root.path().join("app.db")).await;
            let agent = pool(root.path().join("agent.db")).await;
            sqlx::raw_sql("CREATE TABLE user_accounts(user_id TEXT PRIMARY KEY,username TEXT,value BLOB);
                CREATE TABLE user_profiles(user_id TEXT PRIMARY KEY,value BLOB);
                CREATE TABLE agent_directory(id TEXT PRIMARY KEY,owner_id TEXT);
                CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));
                CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);
                CREATE TABLE entitlement_tiers(tier_id TEXT PRIMARY KEY,revision INTEGER,value BLOB);
                CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB);").execute(&app).await.unwrap();
            sqlx::raw_sql("CREATE TABLE agents(id TEXT PRIMARY KEY,status TEXT,metadata BLOB,revision INTEGER,updated_at INTEGER);
                CREATE TABLE agent_memberships(user_id TEXT,agent_id TEXT,value BLOB,PRIMARY KEY(user_id,agent_id));
                CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB);").execute(&agent).await.unwrap();
            let f = Self {
                root,
                app,
                agent,
                codec: Codec::new(),
            };
            let repo = f.repo();
            repo.init_access_policy(None).unwrap();
            let basic = json!({"id":"basic","revision":1,"allowed_models":["*"],"tools":{"*":"auto"},"max_sessions":10,"max_active_runs":10});
            let mut operator = basic.clone();
            operator["id"] = json!("operator");
            repo.write_access_policy(&json!({"schema_version":1,"registration":"open","revision":1,"tiers":{"basic":basic,"operator":operator},"extension":{"preserve":true}})).unwrap();
            for (id, role, tier) in [
                ("admin", "administrator", "operator"),
                ("alice", "user", "basic"),
                ("guest_test", "guest", "basic"),
            ] {
                let value = json!({"user_id":id,"username":id,"status":"active","role":role,"tier_id":tier,"access_revision":1,"generation":"initial","is_approved":true});
                sqlx::query("INSERT INTO user_accounts VALUES(?,?,?)")
                    .bind(id)
                    .bind(id)
                    .bind(f.codec.seal(&value).unwrap())
                    .execute(&f.app)
                    .await
                    .unwrap();
                sqlx::query("INSERT INTO user_profiles VALUES(?,?)")
                    .bind(id)
                    .bind(f.codec.seal(&json!({"is_admin":id=="admin"})).unwrap())
                    .execute(&f.app)
                    .await
                    .unwrap();
            }
            sqlx::query("INSERT INTO agent_directory VALUES('shared_default','admin')")
                .execute(&f.app)
                .await
                .unwrap();
            sqlx::query("INSERT INTO agents VALUES('shared_default','active',?,1,0)")
                .bind(f.codec.seal(&json!({"owner_id":"admin"})).unwrap())
                .execute(&f.agent)
                .await
                .unwrap();
            for (id, relationship) in [("admin", "owner"), ("alice", "member")] {
                let value = json!({"user_id":id,"agent_id":"shared_default","relationship":relationship,"revision":1});
                sqlx::query("INSERT INTO agent_memberships VALUES(?,'shared_default',?)")
                    .bind(id)
                    .bind(f.codec.seal(&value).unwrap())
                    .execute(&f.agent)
                    .await
                    .unwrap();
            }
            f
        }
    }

    #[tokio::test]
    async fn policy_file_lock_serializes_cas_and_preserves_extensions_and_mode() {
        use std::os::unix::fs::PermissionsExt;
        let f = Fixture::new().await;
        let path = f.repo().access_policy_path();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut handles = Vec::new();
        for mode in ["open", "restricted"] {
            let barrier = barrier.clone();
            let root = f.root.path().to_owned();
            let app = f.app.clone();
            let agent = f.agent.clone();
            handles.push(std::thread::spawn(move || {
                let codec = Codec::new();
                let repo = Repository::new(&root, &app, &agent, "shared_default", "admin", &codec);
                barrier.wait();
                repo.set_registration_policy_revision(mode, Some(1)).is_ok()
            }));
        }
        assert_eq!(
            handles
                .into_iter()
                .map(|h| h.join().unwrap() as u8)
                .sum::<u8>(),
            1
        );
        let saved = f.repo().read_access_policy().unwrap();
        assert_eq!(saved["revision"], 2);
        assert_eq!(saved["extension"]["preserve"], true);
        let bytes = std::fs::read(&path).unwrap();
        assert!(f
            .repo()
            .set_registration_policy_revision("open", Some(1))
            .is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(
            !std::fs::read_dir(path.parent().unwrap()).unwrap().any(|p| p
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp"))
        );
        std::fs::write(&path, br#"{"registration":"invalid"}"#).unwrap();
        assert!(f.repo().read_access_policy().is_err());
        assert!(f.repo().init_access_policy(None).is_err());
    }

    #[tokio::test]
    async fn sqlite_membership_revocation_is_live_fenced_and_rolls_back_with_its_audit() {
        let f = Fixture::new().await;
        let repo = f.repo();
        let before = repo.live_access("alice", "shared_default").await.unwrap();
        assert!(before.admitted());
        let changed = repo
            .assign_agent_membership(
                "admin",
                "alice",
                "shared_default",
                Relationship::Revoked,
                false,
                1,
            )
            .await
            .unwrap();
        assert_eq!(changed["revision"], 2);
        let after = repo.live_access("alice", "shared_default").await.unwrap();
        assert!(!after.admitted());
        assert_ne!(before.context_fingerprint(), after.context_fingerprint());
        let account = repo.account_by_id("alice").await.unwrap().unwrap();
        assert_eq!(account["generation"], "initial");
        assert_eq!(account["access_revision"], 1);
        assert!(repo
            .assign_agent_membership(
                "admin",
                "alice",
                "shared_default",
                Relationship::Member,
                false,
                1
            )
            .await
            .is_err());
        sqlx::raw_sql("CREATE TRIGGER reject_access_receipt BEFORE INSERT ON docs WHEN NEW.key LIKE 'agent:shared_default:membership_audit:%' BEGIN SELECT RAISE(ABORT,'test late audit failure'); END;").execute(&f.agent).await.unwrap();
        assert!(repo
            .assign_agent_membership(
                "admin",
                "alice",
                "shared_default",
                Relationship::Member,
                false,
                2
            )
            .await
            .is_err());
        assert_eq!(
            repo.agent_membership("alice", "shared_default")
                .await
                .unwrap()
                .unwrap(),
            changed
        );
        assert_eq!(repo.account_by_id("alice").await.unwrap().unwrap(), account);
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM docs WHERE key LIKE 'agent:shared_default:membership_audit:%'",
        )
        .fetch_one(&f.agent)
        .await
        .unwrap();
        assert_eq!(count, 1);
        let bytes: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id='alice'")
                .fetch_one(&f.app)
                .await
                .unwrap();
        assert!(serde_json::from_slice::<Value>(&bytes).is_err());
        assert!(repo.capability_levels("other_agent").await.is_err());
    }

    #[tokio::test]
    async fn sqlite_capability_policy_and_agent_receipt_commit_or_rollback_together() {
        let f = Fixture::new().await;
        let repo = f.repo();
        let before = repo.live_access("alice", "shared_default").await.unwrap();
        let mut level = before.capability_level.clone();
        level
            .ability_permissions
            .insert("codebase_admin".into(), Permission::Deny);
        let saved = repo
            .save_capability_level("shared_default", level, 0)
            .await
            .unwrap();
        assert_eq!(saved.revision, 1);
        let after = repo.live_access("alice", "shared_default").await.unwrap();
        assert_eq!(
            after.capability_level.ability_permission("codebase_admin"),
            Permission::Deny
        );
        assert_ne!(before.context_fingerprint(), after.context_fingerprint());
        let metadata: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&f.agent)
                .await
                .unwrap();
        sqlx::raw_sql("CREATE TRIGGER reject_capability_receipt BEFORE INSERT ON docs BEGIN SELECT RAISE(ABORT,'test late receipt failure'); END;").execute(&f.agent).await.unwrap();
        assert!(repo
            .save_capability_level("shared_default", after.capability_level, 1)
            .await
            .is_err());
        let unchanged: Vec<u8> =
            sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
                .fetch_one(&f.agent)
                .await
                .unwrap();
        assert_eq!(metadata, unchanged);
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM docs")
            .fetch_one(&f.agent)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn account_access_preserves_original_administrator_guest_identity_and_live_status() {
        let f = Fixture::new().await;
        let repo = f.repo();
        assert!(repo
            .update_account_access(
                "admin",
                1,
                AccountStatus::Suspended,
                Role::Administrator,
                "operator"
            )
            .await
            .is_err());
        assert!(repo
            .update_account_access("guest_test", 1, AccountStatus::Active, Role::User, "basic")
            .await
            .is_err());
        assert!(crate::validate_self_administration(
            "admin",
            "admin",
            AccountStatus::Active,
            Role::User
        )
        .is_err());
        let updated = repo
            .update_account_access("alice", 1, AccountStatus::Suspended, Role::User, "basic")
            .await
            .unwrap();
        assert_eq!(updated["access_revision"], 2);
        assert!(!repo
            .live_access("alice", "shared_default")
            .await
            .unwrap()
            .admitted());
        assert!(repo
            .update_account_access("alice", 1, AccountStatus::Active, Role::User, "basic")
            .await
            .is_err());
        assert_eq!(
            repo.access_audit(20)
                .await
                .unwrap()
                .iter()
                .filter(|v| v["kind"] == "account_access_changed")
                .count(),
            1
        );
        assert!(
            crate::require_administrator(&repo.account_by_id("admin").await.unwrap().unwrap())
                .is_ok()
        );
    }

    #[test]
    fn credential_binding_rejects_foreign_revoked_and_changed_credentials() {
        let requirement = json!({"kind":"credential_owner","credential_kind":"search"});
        let run = json!({"id":"run","session_id":"session","requester_user_id":"alice","pending":{"access_revision":{"account":2}}});
        let credential = json!({"owner_user_id":"alice","version":1});
        let args = json!({"owner_user_id":"admin","query":"example"});
        let binding = crate::credential_authorization::binding(
            &run,
            "admin",
            "search",
            &args,
            &requirement,
            &credential,
        )
        .unwrap();
        assert_eq!(binding["owner_user_id"], "alice");
        assert_eq!(binding["credential_ref"], "tool_credential:alice:search");
        assert!(crate::credential_authorization::binding(
            &run,
            "admin",
            "search",
            &args,
            &requirement,
            &json!({"owner_user_id":"admin","version":1})
        )
        .is_err());
        assert!(crate::credential_authorization::binding(
            &run,
            "admin",
            "search",
            &args,
            &requirement,
            &json!({"owner_user_id":"alice","version":1,"revoked":true})
        )
        .is_err());
        let changed = crate::credential_authorization::binding(
            &run,
            "admin",
            "search",
            &args,
            &requirement,
            &json!({"owner_user_id":"alice","version":2}),
        )
        .unwrap();
        assert_ne!(binding, changed);
    }
    #[tokio::test]
    async fn legacy_access_migration_is_atomic_idempotent_and_preserves_credentials() {
        let f = Fixture::new().await;
        let repo = f.repo();
        let mut legacy = repo.account_by_id("admin").await.unwrap().unwrap();
        legacy.as_object_mut().unwrap().remove("status");
        legacy.as_object_mut().unwrap().remove("role");
        legacy.as_object_mut().unwrap().remove("tier_id");
        legacy.as_object_mut().unwrap().remove("access_revision");
        legacy["password_hash"] = json!("original-credential");
        let bytes = f.codec.seal(&legacy).unwrap();
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id='admin'")
            .bind(&bytes)
            .execute(&f.app)
            .await
            .unwrap();
        sqlx::query("DELETE FROM agent_memberships WHERE user_id='admin'")
            .execute(&f.agent)
            .await
            .unwrap();
        sqlx::raw_sql("CREATE TRIGGER reject_migration_marker BEFORE INSERT ON docs BEGIN SELECT RAISE(ABORT,'test late marker failure'); END;").execute(&f.app).await.unwrap();
        assert!(repo.migrate_access_v1().await.is_err());
        let tiers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entitlement_tiers")
            .fetch_one(&f.app)
            .await
            .unwrap();
        assert_eq!(tiers, 0);
        let unchanged: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id='admin'")
                .fetch_one(&f.app)
                .await
                .unwrap();
        assert_eq!(bytes, unchanged);
        assert!(repo
            .agent_membership("admin", "shared_default")
            .await
            .unwrap()
            .is_none());
        sqlx::query("DROP TRIGGER reject_migration_marker")
            .execute(&f.app)
            .await
            .unwrap();
        repo.migrate_access_v1().await.unwrap();
        let account = repo.account_by_id("admin").await.unwrap().unwrap();
        assert!(account.get("password_hash").is_none());
        assert_eq!(account["generation"], "initial");
        assert_eq!(account["status"], "active");
        assert_eq!(account["role"], "administrator");
        assert_eq!(account["tier_id"], "operator");
        let legacy:Vec<u8>=sqlx::query_scalar("SELECT value FROM agent_memberships WHERE user_id='admin' AND agent_id='shared_default'").fetch_one(&f.app).await.unwrap();
        assert_eq!(f.codec.unseal(&legacy).unwrap()["relationship"], "owner");
        assert!(repo
            .agent_membership("admin", "shared_default")
            .await
            .unwrap()
            .is_none());
        let stable: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id='admin'")
                .fetch_one(&f.app)
                .await
                .unwrap();
        repo.migrate_access_v1().await.unwrap();
        let after: Vec<u8> =
            sqlx::query_scalar("SELECT value FROM user_accounts WHERE user_id='admin'")
                .fetch_one(&f.app)
                .await
                .unwrap();
        assert_eq!(stable, after);
        let tiers: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM entitlement_tiers")
            .fetch_one(&f.app)
            .await
            .unwrap();
        assert_eq!(tiers, 2);
        // Fresh-install seeds are explicit: only Operator receives codebase_admin.
        for (id, codebase) in [("basic", Permission::Deny), ("operator", Permission::Auto)] {
            let blob: Vec<u8> =
                sqlx::query_scalar("SELECT value FROM entitlement_tiers WHERE tier_id=?")
                    .bind(id)
                    .fetch_one(&f.app)
                    .await
                    .unwrap();
            let tier: crate::Tier = serde_json::from_value(f.codec.unseal(&blob).unwrap()).unwrap();
            assert_eq!(tier.ability_permission("codebase_admin"), codebase);
            assert_eq!(tier.ability_permission("web_access"), Permission::Auto);
        }
        let markers: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM docs WHERE key='storage:access_v1'")
                .fetch_one(&f.app)
                .await
                .unwrap();
        assert_eq!(markers, 1);
    }
}
