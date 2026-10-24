use super::*;
use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
use serde_json::json;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::sync::atomic::{AtomicU64, Ordering};
struct Codec {
    cipher: Aes256Gcm,
    nonce: AtomicU64,
}
impl RecordCodec for Codec {
    fn seal(&self, v: &Value) -> Result<Vec<u8>> {
        let mut nonce = [0; 12];
        nonce[4..].copy_from_slice(&self.nonce.fetch_add(1, Ordering::Relaxed).to_be_bytes());
        let mut out = nonce.to_vec();
        out.extend(
            self.cipher
                .encrypt(Nonce::from_slice(&nonce), serde_json::to_vec(v)?.as_ref())
                .map_err(|_| anyhow::anyhow!("Encryption failed"))?,
        );
        Ok(out)
    }
    fn unseal(&self, v: &[u8]) -> Result<Value> {
        anyhow::ensure!(v.len() >= 28, "Invalid encrypted record");
        let bytes = self
            .cipher
            .decrypt(Nonce::from_slice(&v[..12]), &v[12..])
            .map_err(|_| anyhow::anyhow!("Authentication failed"))?;
        Ok(serde_json::from_slice(&bytes)?)
    }
}
struct Fixture {
    _root: tempfile::TempDir,
    pool: SqlitePool,
    codec: Codec,
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                SqliteConnectOptions::new()
                    .filename(root.path().join("app.db"))
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        let f = Self {
            _root: root,
            pool,
            codec: Codec {
                cipher: Aes256Gcm::new_from_slice(&[19; 32]).unwrap(),
                nonce: AtomicU64::new(1),
            },
        };
        sqlx::raw_sql("CREATE TABLE docs(key TEXT PRIMARY KEY,value BLOB);CREATE TABLE events(seq INTEGER PRIMARY KEY AUTOINCREMENT,session_id TEXT,kind TEXT,payload BLOB);").execute(&f.pool).await.unwrap();
        f.repo().initialize().await.unwrap();
        f
    }
    fn repo(&self) -> Repository<'_, Codec> {
        Repository::new(&self.pool, &self.codec)
    }
    async fn active(&self, name: &str) -> String {
        let account = self
            .repo()
            .fixture_user(name, "fixture-password-hash-placeholder")
            .await
            .unwrap();
        let id = account["user_id"].as_str().unwrap().to_owned();
        let mut record = self.repo().account_by_id(&id).await.unwrap().unwrap();
        record["status"] = json!("active");
        record["is_approved"] = json!(true);
        self.save(&id, &record).await;
        id
    }
    async fn save(&self, id: &str, v: &Value) {
        sqlx::query("UPDATE user_accounts SET value=? WHERE user_id=?")
            .bind(self.codec.seal(v).unwrap())
            .bind(id)
            .execute(&self.pool)
            .await
            .unwrap();
    }
}
#[tokio::test]
async fn encrypted_creation_defaults_and_duplicate_rollback() {
    let f = Fixture::new().await;
    let r = f.repo();
    let v = r
        .fixture_user("member", "fixture-password-hash-placeholder")
        .await
        .unwrap();
    assert_eq!(v["status"], "pending");
    assert!(v.get("password_hash").is_none());
    assert!(r
        .fixture_user("member", "fixture-password-hash-placeholder")
        .await
        .is_err());
    let blob: Vec<u8> = sqlx::query_scalar("SELECT value FROM user_accounts")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert!(!blob.windows(6).any(|w| w == b"member"));
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM user_profiles")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        1
    );
}
#[tokio::test]
async fn profile_and_password_are_scoped_and_cas_guarded() {
    let f = Fixture::new().await;
    let a = f.active("member").await;
    let b = f.active("other").await;
    let r = f.repo();
    let original = r.account_by_id(&a).await.unwrap().unwrap();
    r.update_user_profile(&a, "renamed", "Member")
        .await
        .unwrap();
    assert!(r
        .replace_user_password(&a, "wrong", "replacement-hash-placeholder", "next")
        .await
        .is_err());
    r.replace_user_password(
        &a,
        original["generation"].as_str().unwrap(),
        "replacement-hash-placeholder",
        "next",
    )
    .await
    .unwrap();
    assert_eq!(
        r.account_by_id(&a).await.unwrap().unwrap()["generation"],
        "next"
    );
    assert_ne!(
        r.account_by_id(&b).await.unwrap().unwrap()["generation"],
        "next"
    );
}
#[tokio::test]
async fn expiry_logout_and_explicit_revoke() {
    let f = Fixture::new().await;
    let id = f.active("member").await;
    let r = f.repo();
    let s = r.fixture_session(&id, 100, "browser").await.unwrap();
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    assert!(r
        .authenticate_session(&s.token, s.expires, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(!r.revoke_session("other", &s.id).await.unwrap());
    assert!(r.revoke_session(&id, &s.id).await.unwrap());
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    let s = r.fixture_session(&id, 100, "browser").await.unwrap();
    r.logout(&s.token).await.unwrap();
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn disabling_and_generation_rotation_withdraw_sessions() {
    let f = Fixture::new().await;
    let id = f.active("member").await;
    let r = f.repo();
    let s = r.fixture_session(&id, 100, "browser").await.unwrap();
    let mut record = r.account_by_id(&id).await.unwrap().unwrap();
    record["status"] = json!("disabled");
    f.save(&id, &record).await;
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .is_none());
    assert!(r.fixture_session(&id, 100, "browser").await.is_err());
    record["status"] = json!("active");
    record["generation"] = json!("changed");
    f.save(&id, &record).await;
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn guest_policy_and_no_password_identity() {
    let f = Fixture::new().await;
    let r = f.repo();
    let guest = r.fixture_guest().await.unwrap();
    let id = guest["user_id"].as_str().unwrap();
    assert!(r
        .verify_credentials(id, "anything".into())
        .await
        .unwrap()
        .is_none());
    let s = r.fixture_session(id, 100, "guest").await.unwrap();
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .is_some());
    assert!(r
        .authenticate_session(&s.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn credential_verification_rejects_pending_and_wrong_password() {
    let f = Fixture::new().await;
    let r = f.repo();
    let hash = hash_password("fixture-password".into()).await.unwrap();
    let v = r.fixture_user("member", &hash).await.unwrap();
    assert!(r
        .verify_credentials("member", "fixture-password".into())
        .await
        .unwrap()
        .is_none());
    let id = v["user_id"].as_str().unwrap();
    let mut record = r.account_by_id(id).await.unwrap().unwrap();
    record["status"] = json!("active");
    record["is_approved"] = json!(true);
    f.save(id, &record).await;
    assert!(r
        .verify_credentials("member", "wrong".into())
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        r.verify_credentials("member", "fixture-password".into())
            .await
            .unwrap()
            .unwrap()
            .identity()
            .user_id,
        id
    );
}
#[tokio::test]
async fn shared_throttle_window() {
    let budget = LoginAttempts::default();
    let other = budget.clone();
    for _ in 0..5 {
        assert!(
            budget
                .admit(
                    crate::Admission::Login,
                    std::net::IpAddr::from([127, 0, 0, 1]),
                    "member",
                    100
                )
                .await
        );
    }
    assert!(
        !other
            .admit(
                crate::Admission::Login,
                std::net::IpAddr::from([127, 0, 0, 1]),
                "member",
                159
            )
            .await
    );
    assert!(
        other
            .admit(
                crate::Admission::Login,
                std::net::IpAddr::from([127, 0, 0, 1]),
                "member",
                160
            )
            .await
    );
}
#[test]
fn registration_and_message_credentials_fail_closed() {
    assert_eq!(
        RegistrationMode::from_policy(&json!({"registration":"open","extension":1})).unwrap(),
        RegistrationMode::Open
    );
    assert!(RegistrationMode::from_policy(&json!({"registration":"invalid"})).is_err());
    assert!(message_api_token_hash("short").is_err());
    let token = "x".repeat(32);
    let hash = message_api_token_hash(&token).unwrap();
    assert!(message_api_authorized(
        hash.as_ref(),
        Some(&format!("Bearer {token}"))
    ));
    assert!(!message_api_authorized(hash.as_ref(), Some("Bearer wrong")));
    assert!(!message_api_authorized(None, None));
}
#[tokio::test]
async fn bootstrap_joins_caller_transaction_and_rolls_back() {
    let f = Fixture::new().await;
    let r = f.repo();
    let mut tx = f.pool.begin().await.unwrap();
    r.fixture_admin_in(&mut tx, "operator", "fixture-hash", "generation")
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert!(!r.has_admin_account().await.unwrap());
    let mut tx = f.pool.begin().await.unwrap();
    r.fixture_admin_in(&mut tx, "operator", "fixture-hash", "generation")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(r.has_admin_account().await.unwrap());
    let mut tx = f.pool.begin().await.unwrap();
    assert!(r
        .fixture_admin_in(&mut tx, "second", "fixture-hash", "generation")
        .await
        .is_err());
    tx.rollback().await.unwrap();
    assert!(r.account_by_username("second").await.unwrap().is_none());
}
#[tokio::test]
async fn status_handoff_is_transactional_and_revision_guarded() {
    let f = Fixture::new().await;
    let id = f.active("member").await;
    let r = f.repo();
    let session = r.fixture_session(&id, 100, "browser").await.unwrap();
    let old = r.account_by_id(&id).await.unwrap().unwrap()["generation"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut tx = f.pool.begin().await.unwrap();
    r.apply_access_in(
        &mut tx,
        &id,
        AccessMutation {
            expected_revision: 1,
            status: AccountStatus::Disabled,
            role: "user",
            tier_id: "basic",
            profile_is_admin: false,
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert!(r
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    let mut tx = f.pool.begin().await.unwrap();
    assert!(r
        .apply_access_in(
            &mut tx,
            &id,
            AccessMutation {
                expected_revision: 999,
                status: AccountStatus::Disabled,
                role: "user",
                tier_id: "basic",
                profile_is_admin: false
            }
        )
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    r.apply_access_in(
        &mut tx,
        &id,
        AccessMutation {
            expected_revision: 1,
            status: AccountStatus::Disabled,
            role: "user",
            tier_id: "basic",
            profile_is_admin: false,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_ne!(
        r.account_state(&id).await.unwrap().unwrap()["generation"],
        old
    );
    assert_eq!(
        r.identity(&id).await.unwrap().unwrap().status,
        AccountStatus::Disabled
    );
    assert!(r
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn duration_policy_rejects_guest_and_invalid_values() {
    let f = Fixture::new().await;
    let id = f.active("member").await;
    let r = f.repo();
    assert!(r.set_session_days(&id, 2).await.is_err());
    r.set_session_days(&id, 7).await.unwrap();
    assert_eq!(r.session_days(&id).await.unwrap(), 7);
    let guest = r.fixture_guest().await.unwrap();
    assert!(r
        .set_session_days(guest["user_id"].as_str().unwrap(), 7)
        .await
        .is_err());
}

#[tokio::test]
async fn access_handoff_preserves_credentials_and_rolls_back_with_caller() {
    let f = Fixture::new().await;
    let id = f.active("handoff").await;
    let r = f.repo();
    let before = r.account_by_id(&id).await.unwrap().unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    let result = r
        .apply_access_in(
            &mut tx,
            &id,
            AccessMutation {
                expected_revision: 1,
                status: AccountStatus::Disabled,
                role: "user",
                tier_id: "basic",
                profile_is_admin: false,
            },
        )
        .await
        .unwrap();
    assert_eq!(result["status"], "disabled");
    assert!(result.get("password_hash").is_none());
    assert_ne!(result["generation"], before["generation"]);
    tx.rollback().await.unwrap();
    assert_eq!(r.account_by_id(&id).await.unwrap().unwrap(), before);
    let mut tx = f.pool.begin().await.unwrap();
    assert!(r
        .apply_access_in(
            &mut tx,
            &id,
            AccessMutation {
                expected_revision: 99,
                status: AccountStatus::Disabled,
                role: "user",
                tier_id: "basic",
                profile_is_admin: false,
            }
        )
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    r.apply_access_in(
        &mut tx,
        &id,
        AccessMutation {
            expected_revision: 1,
            status: AccountStatus::Disabled,
            role: "user",
            tier_id: "basic",
            profile_is_admin: false,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let after = r.account_by_id(&id).await.unwrap().unwrap();
    assert_eq!(after["password_hash"], before["password_hash"]);
    assert_eq!(after["access_revision"], 2);
}
#[tokio::test]
async fn principal_projection_never_exposes_passwords_and_keeps_version() {
    let f = Fixture::new().await;
    let id = f.active("projection").await;
    let r = f.repo();
    let raw = r.account_by_id(&id).await.unwrap().unwrap();
    let state = r.account_state(&id).await.unwrap().unwrap();
    assert!(state.get("password_hash").is_none());
    assert_eq!(state["generation"], raw["generation"]);
    assert_eq!(state["status"], "active");
    assert_eq!(state["role"], "user");
    let guest = r.fixture_guest().await.unwrap();
    assert!(guest["generation"].is_string());
    assert!(guest.get("password_hash").is_none());
}

struct ApiHost {
    admin: std::sync::atomic::AtomicBool,
    gate: tokio::sync::Mutex<()>,
}
impl crate::api::Host for ApiHost {
    fn initial_guest_access(&self) -> InitialAccess<'_> {
        InitialAccess {
            role: "guest",
            tier_id: "anonymous",
            profile_is_admin: false,
        }
    }
    fn administrator_account(&self, _record: &Value) -> crate::api::Result<bool> {
        Ok(self.admin.load(Ordering::SeqCst))
    }
    fn registration_policy(&self) -> crate::api::Result<Value> {
        Ok(json!({"registration":"restricted","revision":1}))
    }
    fn set_registration_policy(
        &self,
        _registration: &str,
        _expected: Option<u64>,
    ) -> crate::api::Result<()> {
        Err(anyhow::anyhow!("Not used by this management fixture").into())
    }
    async fn create_administrator<'a>(
        &'a self,
        _username: &'a str,
        _hash: &'a str,
        _generation: &'a str,
    ) -> crate::api::Result<()> {
        Err(anyhow::anyhow!("Not used by this management fixture").into())
    }
    async fn activate_registered_user<'a>(&'a self, _id: &'a str) -> crate::api::Result<()> {
        Err(anyhow::anyhow!("Not used by this management fixture").into())
    }

    fn initial_user_access(&self) -> crate::InitialAccess<'_> {
        fixture_user_access()
    }
    async fn administrator<'a>(&'a self, _identity: &'a Value) -> crate::api::Result<()> {
        if self.admin.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(crate::api::Error::Public(
                axum::http::StatusCode::FORBIDDEN,
                "Administrator required".into(),
            ))
        }
    }
    fn account_view(&self, record: &Value) -> crate::api::Result<Value> {
        Ok(
            json!({"user_id":record["user_id"],"username":record["username"],"status":record["status"],"role":record["role"],"tier_id":record["tier_id"],"access_revision":record["access_revision"]}),
        )
    }
    async fn user_counts<'a>(&'a self, _user: &'a str) -> crate::api::Result<(i64, i64)> {
        Ok((0, 0))
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }
}
#[tokio::test]
async fn native_account_controller_rechecks_host_authority_and_redacts_credentials() {
    let f = Fixture::new().await;
    let r = f.repo();
    let host = ApiHost {
        admin: std::sync::atomic::AtomicBool::new(false),
        gate: tokio::sync::Mutex::new(()),
    };
    let identity = json!({"_user_id":"admin"});
    let result = crate::api::create_account(
        &r,
        &host,
        &identity,
        crate::api::NewAccount {
            username: "controller".into(),
            password: "fixture-password".into(),
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(crate::api::Error::Public(
            axum::http::StatusCode::FORBIDDEN,
            _
        ))
    ));
    assert!(r.all_user_ids().await.unwrap().is_empty());
    host.admin.store(true, Ordering::SeqCst);
    let result = crate::api::create_account(
        &r,
        &host,
        &identity,
        crate::api::NewAccount {
            username: "controller".into(),
            password: "fixture-password".into(),
        },
    )
    .await
    .unwrap()
    .0;
    assert!(result["account"].get("password_hash").is_none());
    assert!(crate::api::accounts(&r, &host, &identity).await.unwrap().0["accounts"].is_array());
    host.admin.store(false, Ordering::SeqCst);
    assert!(crate::api::accounts(&r, &host, &identity).await.is_err());
    assert!(serde_json::from_value::<crate::api::NewAccount>(
        json!({"username":"other","password":"fixture-password","role":"administrator"})
    )
    .is_err());
}

#[tokio::test]
async fn credentials_and_sessions_cannot_cross_persisted_identity_bindings() {
    let f = Fixture::new().await;
    let r = f.repo();
    let hash = hash_password("fixture-password".into()).await.unwrap();
    let created = r.fixture_user("bound-user", &hash).await.unwrap();
    let id = created["user_id"].as_str().unwrap();
    let mut account = r.account_by_id(id).await.unwrap().unwrap();
    account["status"] = json!("active");
    account["is_approved"] = json!(true);
    f.save(id, &account).await;
    let session = r.fixture_session(id, 100, "browser").await.unwrap();
    account["username"] = json!("different-user");
    f.save(id, &account).await;
    assert!(r
        .verify_credentials("bound-user", "fixture-password".into())
        .await
        .unwrap()
        .is_none());
    account["user_id"] = json!("foreign_identity");
    f.save(id, &account).await;
    assert!(r
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(r.fixture_session(id, 100, "browser").await.is_err());
}
#[tokio::test]
async fn invalid_duration_cannot_overflow_or_issue_a_session() {
    let f = Fixture::new().await;
    let id = f.active("duration-overflow").await;
    let r = f.repo();
    let mut profile = r.user_profile(&id).await.unwrap().unwrap();
    profile["session_days"] = json!(u64::MAX);
    sqlx::query("UPDATE user_profiles SET value=? WHERE user_id=?")
        .bind(f.codec.seal(&profile).unwrap())
        .bind(&id)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(r.fixture_session(&id, 100, "browser").await.is_err());
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM docs WHERE key LIKE 'auth:%'")
            .fetch_one(&f.pool)
            .await
            .unwrap(),
        0
    );
}

fn fixture_user_access() -> InitialAccess<'static> {
    InitialAccess {
        role: "user",
        tier_id: "basic",
        profile_is_admin: false,
    }
}
impl Repository<'_, Codec> {
    async fn fixture_user(&self, username: &str, hash: &str) -> Result<Value> {
        self.create_user_account(username, hash, fixture_user_access())
            .await
    }
    async fn fixture_guest(&self) -> Result<Value> {
        self.create_guest_account(InitialAccess {
            role: "guest",
            tier_id: "anonymous",
            profile_is_admin: false,
        })
        .await
    }
    async fn fixture_admin_in(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
        username: &str,
        hash: &str,
        generation: &str,
    ) -> Result<()> {
        self.create_admin_in(
            tx,
            username,
            hash,
            generation,
            InitialAccess {
                role: "administrator",
                tier_id: "operator",
                profile_is_admin: true,
            },
        )
        .await
    }
}

impl Repository<'_, Codec> {
    async fn fixture_session(
        &self,
        user: &str,
        now: u64,
        user_agent: &str,
    ) -> Result<IssuedSession> {
        let account = self
            .account_by_id(user)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Account not found"))?;
        let identity = crate::identity(&account)?;
        anyhow::ensure!(
            identity.user_id == user && account["is_approved"] == true,
            "Account identity mismatch"
        );
        let generation = account["generation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Account has no version"))?;
        let duration = if account["role"] == "guest" {
            30 * 86400
        } else {
            self.session_days(user)
                .await?
                .checked_mul(86400)
                .ok_or_else(|| anyhow::anyhow!("Session duration overflow"))?
        };
        self.issue_versioned_session(&identity, generation, now, user_agent, duration)
            .await
    }
}
#[tokio::test]
async fn password_rotation_cannot_refresh_an_old_credential_proof() {
    let f = Fixture::new().await;
    let r = f.repo();
    let hash = hash_password("fixture-password".into()).await.unwrap();
    let created = r.fixture_user("proof-owner", &hash).await.unwrap();
    let id = created["user_id"].as_str().unwrap();
    let mut account = r.account_by_id(id).await.unwrap().unwrap();
    account["status"] = json!("active");
    account["is_approved"] = json!(true);
    f.save(id, &account).await;
    let proof = r
        .verify_credentials("proof-owner", "fixture-password".into())
        .await
        .unwrap()
        .unwrap();
    let next_hash = hash_password("next-fixture-password".into()).await.unwrap();
    r.replace_user_password(
        id,
        account["generation"].as_str().unwrap(),
        &next_hash,
        "next-generation",
    )
    .await
    .unwrap();
    let stale_session = r
        .issue_verified_session(proof, 100, "browser")
        .await
        .unwrap();
    assert!(r
        .authenticate_session(&stale_session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(r
        .verify_credentials("proof-owner", "fixture-password".into())
        .await
        .unwrap()
        .is_none());
    let proof = r
        .verify_credentials("proof-owner", "next-fixture-password".into())
        .await
        .unwrap()
        .unwrap();
    let session = r
        .issue_verified_session(proof, 100, "browser")
        .await
        .unwrap();
    assert!(r
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
}

#[test]
fn registration_field_defaults_and_updates_preserve_shared_policy() {
    assert_eq!(
        RegistrationMode::from_legacy(None),
        RegistrationMode::Restricted
    );
    assert_eq!(
        RegistrationMode::from_legacy(Some(&json!({"access_mode":"public_registered"}))),
        RegistrationMode::Open
    );
    let mut policy = json!({"registration":"restricted","revision":42,"pages":{"main":{"chat":"registered"}},"extension":{"future":true}});
    let before = policy.clone();
    RegistrationMode::Open.write_policy(&mut policy).unwrap();
    assert_eq!(policy["registration"], "open");
    for field in ["revision", "pages", "extension"] {
        assert_eq!(policy[field], before[field]);
    }
    assert!(RegistrationMode::from_policy(&json!({"registration":"invalid"})).is_err());
}

#[tokio::test]
async fn administrative_session_controller_redacts_and_revokes_only_the_target_account() {
    let f = Fixture::new().await;
    let first = f.active("session-owner").await;
    let second = f.active("other-session-owner").await;
    let repo = f.repo();
    let first_session = repo
        .fixture_session(&first, 100, "first browser")
        .await
        .unwrap();
    let second_session = repo
        .fixture_session(&second, 100, "second browser")
        .await
        .unwrap();
    let host = ApiHost {
        admin: std::sync::atomic::AtomicBool::new(true),
        gate: tokio::sync::Mutex::new(()),
    };
    let identity = json!({"_user_id":"admin"});
    let listing = crate::api::account_sessions(&repo, &host, &identity, &first, 100)
        .await
        .unwrap()
        .0;
    let sessions = listing["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], first_session.id);
    for private in ["key", "generation", "csrf", "token", "user_id"] {
        assert!(sessions[0].get(private).is_none());
    }
    let wrong = crate::api::revoke_account_session(
        &repo,
        &host,
        &identity,
        &first,
        &second_session.id,
        100,
    )
    .await;
    assert!(matches!(
        wrong,
        Err(crate::api::Error::Public(
            axum::http::StatusCode::BAD_REQUEST,
            _
        ))
    ));
    assert!(repo
        .authenticate_session(&first_session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    assert!(repo
        .authenticate_session(&second_session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        crate::api::revoke_account_session(&repo, &host, &identity, &first, &first_session.id, 100)
            .await
            .unwrap()
            .0,
        json!({"revoked":true})
    );
    assert!(repo
        .authenticate_session(&first_session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(repo
        .authenticate_session(&second_session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
}

struct AuthHost<'a> {
    fixture: &'a Fixture,
    policy: std::sync::Mutex<Value>,
    gate: tokio::sync::Mutex<()>,
    administrator_allowed: std::sync::atomic::AtomicBool,
}
impl<'a> AuthHost<'a> {
    fn new(fixture: &'a Fixture) -> Self {
        Self {
            fixture,
            policy: std::sync::Mutex::new(
                json!({"registration":"restricted","revision":1,"extension":{"preserve":true}}),
            ),
            gate: tokio::sync::Mutex::new(()),
            administrator_allowed: std::sync::atomic::AtomicBool::new(true),
        }
    }
}
impl crate::api::Host for AuthHost<'_> {
    fn initial_user_access(&self) -> InitialAccess<'_> {
        fixture_user_access()
    }
    fn initial_guest_access(&self) -> InitialAccess<'_> {
        InitialAccess {
            role: "guest",
            tier_id: "anonymous",
            profile_is_admin: false,
        }
    }
    async fn administrator<'a>(&'a self, _identity: &'a Value) -> crate::api::Result<()> {
        Ok(())
    }
    fn administrator_account(&self, record: &Value) -> crate::api::Result<bool> {
        Ok(record["user_id"] == "admin" && self.administrator_allowed.load(Ordering::SeqCst))
    }
    fn account_view(&self, record: &Value) -> crate::api::Result<Value> {
        Ok(
            json!({"user_id":record["user_id"],"username":record["username"],"status":record["status"],"role":record["role"],"tier_id":record["tier_id"],"access_revision":record["access_revision"]}),
        )
    }
    async fn user_counts<'a>(&'a self, _user: &'a str) -> crate::api::Result<(i64, i64)> {
        Ok((0, 0))
    }
    fn gate(&self) -> &tokio::sync::Mutex<()> {
        &self.gate
    }
    fn registration_policy(&self) -> crate::api::Result<Value> {
        Ok(self.policy.lock().unwrap().clone())
    }
    fn set_registration_policy(
        &self,
        registration: &str,
        expected: Option<u64>,
    ) -> crate::api::Result<()> {
        let mut policy = self.policy.lock().unwrap();
        let revision = policy["revision"].as_u64().unwrap();
        if expected.is_some_and(|value| value != revision) {
            return Err(anyhow::anyhow!("Policy revision changed").into());
        }
        RegistrationMode::from_policy(&json!({"registration":registration}))?
            .write_policy(&mut policy)?;
        policy["revision"] = json!(revision + 1);
        Ok(())
    }
    async fn create_administrator<'a>(
        &'a self,
        username: &'a str,
        hash: &'a str,
        generation: &'a str,
    ) -> crate::api::Result<()> {
        let repo = self.fixture.repo();
        let mut tx = self
            .fixture
            .pool
            .begin()
            .await
            .map_err(anyhow::Error::from)?;
        repo.create_admin_in(
            &mut tx,
            username,
            hash,
            generation,
            InitialAccess {
                role: "administrator",
                tier_id: "operator",
                profile_is_admin: true,
            },
        )
        .await?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        Ok(())
    }
    async fn activate_registered_user<'a>(&'a self, id: &'a str) -> crate::api::Result<()> {
        let repo = self.fixture.repo();
        let mut tx = self
            .fixture
            .pool
            .begin()
            .await
            .map_err(anyhow::Error::from)?;
        repo.apply_access_in(
            &mut tx,
            id,
            AccessMutation {
                expected_revision: 1,
                status: AccountStatus::Active,
                role: "user",
                tier_id: "basic",
                profile_is_admin: false,
            },
        )
        .await?;
        tx.commit().await.map_err(anyhow::Error::from)?;
        Ok(())
    }
}
async fn controller_response(response: axum::response::Response) -> (axum::http::HeaderMap, Value) {
    let headers = response.headers().clone();
    let bytes = axum::body::to_bytes(response.into_body(), 16384)
        .await
        .unwrap();
    (headers, serde_json::from_slice(&bytes).unwrap())
}
fn controller_token(headers: &axum::http::HeaderMap) -> String {
    headers["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .strip_prefix("v3_session=")
        .unwrap()
        .into()
}
async fn controller_identity(
    repo: &Repository<'_, Codec>,
    token: &str,
    now: u64,
    mode: RegistrationMode,
) -> Value {
    let session = repo
        .authenticate_session(token, now, mode)
        .await
        .unwrap()
        .unwrap();
    let mut doc = session.document;
    doc["_user_id"] = json!(session.identity.user_id);
    doc["_auth_key"] = json!(session.key);
    doc["_role"] = repo
        .account_state(doc["_user_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap()["role"]
        .clone();
    doc
}
#[tokio::test]
async fn native_authentication_controllers_keep_setup_policy_password_and_session_contracts() {
    use crate::api::Error;
    use axum::http::{HeaderMap, StatusCode};
    let f = Fixture::new().await;
    let repo = f.repo();
    let host = AuthHost::new(&f);
    let attempts = LoginAttempts::default();
    let mut headers = HeaderMap::new();
    headers.insert("host", "native.test".parse().unwrap());
    assert_eq!(
        crate::authentication_api::setup_status(&repo)
            .await
            .unwrap()
            .0,
        json!({"initialized":false})
    );
    let response = crate::authentication_api::setup_admin(
        &repo,
        &host,
        &attempts,
        &headers,
        &json!({"username":"controller-owner","password":concat!("fixture-contr", "oller-password")}),
        true, crate::AdmissionContext { now: 100, client: std::net::IpAddr::from([127,0,0,1]) })
    .await
    .unwrap();
    let (cookie, body) = controller_response(response).await;
    assert!(cookie["set-cookie"].to_str().unwrap().ends_with("; Secure"));
    assert!(body["csrf_token"].is_string());
    let token = controller_token(&cookie);
    let doc = controller_identity(&repo, &token, 100, RegistrationMode::Restricted).await;
    assert_eq!(
        crate::authentication_api::setup_status(&repo)
            .await
            .unwrap()
            .0,
        json!({"initialized":true})
    );
    let duplicate = crate::authentication_api::setup_admin(
        &repo,
        &host,
        &attempts,
        &headers,
        &json!({"password":concat!("another-fixt", "ure-password")}),
        true,
        crate::AdmissionContext {
            now: 100,
            client: std::net::IpAddr::from([127, 0, 0, 1]),
        },
    )
    .await;
    assert!(matches!(
        duplicate,
        Err(Error::Public(StatusCode::CONFLICT, _))
    ));
    let me = crate::authentication_api::me(&repo, &host, &doc)
        .await
        .unwrap()
        .0;
    assert_eq!(me["user"], "controller-owner");
    assert_eq!(me["is_admin"], true);
    assert!(me.get("password_hash").is_none());
    assert!(me.get("generation").is_none());
    assert_eq!(
        crate::self_service::session_policy(&repo, &doc)
            .await
            .unwrap()
            .0,
        json!({"days":1})
    );
    assert_eq!(
        crate::self_service::update_session_policy(&repo, &doc, &json!({"days":7}))
            .await
            .unwrap()
            .0,
        json!({"days":7})
    );
    assert!(matches!(
        crate::self_service::update_session_policy(&repo, &doc, &json!({"days":2})).await,
        Err(Error::Public(StatusCode::BAD_REQUEST, _))
    ));
    let second = crate::authentication_api::login(
        &repo,
        &host,
        &attempts,
        &headers,
        &json!({"username":"controller-owner","password":concat!("fixture-contr", "oller-password")}),
        true, crate::AdmissionContext { now: 101, client: std::net::IpAddr::from([127,0,0,1]) })
    .await
    .unwrap();
    let (second_cookie, _) = controller_response(second).await;
    assert!(second_cookie["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=604800"));
    let second_token = controller_token(&second_cookie);
    let second_session = repo
        .authenticate_session(&second_token, 101, RegistrationMode::Restricted)
        .await
        .unwrap()
        .unwrap();
    let second_id = second_session.document["id"].as_str().unwrap();
    let listing = crate::self_service::list_auth_sessions(&repo, &doc, 101)
        .await
        .unwrap()
        .0;
    assert_eq!(listing["sessions"].as_array().unwrap().len(), 2);
    assert_eq!(
        listing["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|session| session["current"] == true)
            .count(),
        1
    );
    assert_eq!(
        crate::self_service::revoke_auth_session(&repo, &doc, second_id, 101)
            .await
            .unwrap()
            .0["current"],
        false
    );
    assert!(repo
        .authenticate_session(&second_token, 101, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    let attempts = crate::LoginAttempts::default();
    let invalid = json!({"current_password":concat!("wrong-", "fixture-value"),"new_password":concat!("next-", "controller-password")});
    for _ in 0..5 {
        assert!(matches!(
            crate::self_service::change_password(&repo, &host, &doc, &invalid, &attempts, 100)
                .await,
            Err(crate::api::Error::Public(
                axum::http::StatusCode::FORBIDDEN,
                _
            ))
        ));
    }
    assert!(matches!(
        crate::self_service::change_password(&repo, &host, &doc, &invalid, &attempts, 100).await,
        Err(crate::api::Error::Public(
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            _
        ))
    ));
    assert!(matches!(
        crate::self_service::change_password(&repo, &host, &doc, &invalid, &attempts, 160).await,
        Err(crate::api::Error::Public(
            axum::http::StatusCode::FORBIDDEN,
            _
        ))
    ));
    assert_eq!(crate::self_service::change_password(&repo,&host,&doc,&json!({"current_password":"fixture-controller-password","new_password":"next-controller-password"}), &crate::LoginAttempts::default(), 100).await.unwrap().0["sign_in_required"],true);
    assert!(repo
        .authenticate_session(&token, 101, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        crate::authentication_api::login(
            &repo,
            &host,
            &attempts,
            &headers,
            &json!({"username":"controller-owner","password":concat!("fixture-contr", "oller-password")}),
            true, crate::AdmissionContext { now: 102, client: std::net::IpAddr::from([127,0,0,1]) })
        .await,
        Err(Error::Public(StatusCode::UNAUTHORIZED, _))
    ));
    let next = crate::authentication_api::login(
        &repo,
        &host,
        &attempts,
        &headers,
        &json!({"username":"controller-owner","password":concat!("next-control", "ler-password")}),
        true,
        crate::AdmissionContext {
            now: 103,
            client: std::net::IpAddr::from([127, 0, 0, 1]),
        },
    )
    .await
    .unwrap();
    let (next_cookie, _) = controller_response(next).await;
    let next_token = controller_token(&next_cookie);
    let mut logout_headers = HeaderMap::new();
    logout_headers.insert(
        "cookie",
        format!("v3_session={next_token}").parse().unwrap(),
    );
    crate::authentication_api::logout(&repo, &logout_headers)
        .await
        .unwrap();
    assert!(repo
        .authenticate_session(&next_token, 103, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn native_registration_controllers_keep_pending_activation_guest_recovery_and_live_policy() {
    use crate::api::{Error, Host};
    use axum::http::{HeaderMap, StatusCode};
    let f = Fixture::new().await;
    let repo = f.repo();
    let host = AuthHost::new(&f);
    let attempts = LoginAttempts::default();
    let hash = hash_password("fixture-controller-password".into())
        .await
        .unwrap();
    host.create_administrator("root", &hash, "initial")
        .await
        .unwrap();
    let headers = HeaderMap::new();
    let response=crate::registration_api::register(&repo,&host,&attempts,&headers,&json!({"username":"pending-member","password":concat!("fixture-mem", "ber-password"),"display_name":"Pending member"}),true, crate::AdmissionContext { now: 100, client: std::net::IpAddr::from([127,0,0,1]) }).await.unwrap();
    let (pending_headers, pending) = controller_response(response).await;
    assert!(!pending_headers.contains_key("set-cookie"));
    assert_eq!(pending["pending_approval"], true);
    assert!(repo
        .verify_credentials("pending-member", "fixture-member-password".into())
        .await
        .unwrap()
        .is_none());
    let admin = json!({"_user_id":"admin"});
    assert_eq!(
        crate::registration_api::set_mode(
            &host,
            &admin,
            &json!({"access_mode":"public_registered","expected_revision":1}),
        )
        .await
        .unwrap()
        .0["guest_access"],
        true
    );
    assert_eq!(
        crate::registration_api::access_mode(&host).await.unwrap().0["guest_access"],
        true
    );
    assert!(matches!(
        crate::registration_api::set_mode(
            &host,
            &admin,
            &json!({"access_mode":"admin_approval","expected_revision":1})
        )
        .await,
        Err(Error::Public(StatusCode::CONFLICT, _))
    ));
    for _ in 0..5 {
        assert!(
            attempts
                .admit(
                    crate::Admission::Login,
                    std::net::IpAddr::from([127, 0, 0, 1]),
                    "active-member",
                    100
                )
                .await
        );
    }
    let response=crate::registration_api::register(&repo,&host,&attempts,&headers,&json!({"username":"active-member","password":concat!("fixture-mem", "ber-password"),"role":"administrator"}),true, crate::AdmissionContext { now: 100, client: std::net::IpAddr::from([127,0,0,1]) }).await.unwrap();
    let (member_headers, _) = controller_response(response).await;
    let member_token = controller_token(&member_headers);
    let member = repo
        .authenticate_session(&member_token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        repo.account_state(&member.identity.user_id)
            .await
            .unwrap()
            .unwrap()["role"],
        "user"
    );
    let guest = crate::registration_api::guest_login(
        &repo,
        &host,
        &attempts,
        &headers,
        true,
        crate::AdmissionContext {
            now: 100,
            client: std::net::IpAddr::from([127, 0, 0, 1]),
        },
    )
    .await
    .unwrap();
    let (first_headers, first) = controller_response(guest).await;
    let first_token = controller_token(&first_headers);
    let mut recovered_headers = HeaderMap::new();
    recovered_headers.insert(
        "cookie",
        format!("v3_session={first_token}").parse().unwrap(),
    );
    let recovered = crate::registration_api::guest_login(
        &repo,
        &host,
        &attempts,
        &recovered_headers,
        true,
        crate::AdmissionContext {
            now: 100,
            client: std::net::IpAddr::from([127, 0, 0, 1]),
        },
    )
    .await
    .unwrap();
    let (next_headers, next) = controller_response(recovered).await;
    let next_token = controller_token(&next_headers);
    assert_eq!(next["user_id"], first["user_id"]);
    assert!(next_token != first_token);
    assert!(repo
        .authenticate_session(&first_token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .is_none());
    assert!(repo
        .authenticate_session(&next_token, 100, RegistrationMode::Open)
        .await
        .unwrap()
        .is_some());
    let mut member_cookie = HeaderMap::new();
    member_cookie.insert(
        "cookie",
        format!("v3_session={member_token}").parse().unwrap(),
    );
    assert!(matches!(
        crate::registration_api::guest_login(
            &repo,
            &host,
            &attempts,
            &member_cookie,
            true,
            crate::AdmissionContext {
                now: 100,
                client: std::net::IpAddr::from([127, 0, 0, 1])
            }
        )
        .await,
        Err(Error::Public(StatusCode::CONFLICT, _))
    ));
    assert_eq!(
        crate::registration_api::set_mode(
            &host,
            &admin,
            &json!({"access_mode":"admin_approval","expected_revision":2}),
        )
        .await
        .unwrap()
        .0["guest_access"],
        false
    );
    assert!(repo
        .authenticate_session(&next_token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    assert!(matches!(
        crate::registration_api::guest_login(
            &repo,
            &host,
            &attempts,
            &headers,
            true,
            crate::AdmissionContext {
                now: 100,
                client: std::net::IpAddr::from([127, 0, 0, 1])
            }
        )
        .await,
        Err(Error::Public(StatusCode::FORBIDDEN, _))
    ));
}
#[tokio::test]
async fn session_candidate_defers_only_generation_check_and_never_exposes_credentials() {
    let f = Fixture::new().await;
    let user = f.active("candidate-owner").await;
    let repo = f.repo();
    let session = repo.fixture_session(&user, 100, "browser").await.unwrap();
    let mut account = repo.account_by_id(&user).await.unwrap().unwrap();
    account["generation"] = json!("rotated");
    f.save(&user, &account).await;
    let candidate = repo
        .inspect_session(&session.token, 100)
        .await
        .unwrap()
        .unwrap();
    assert!(candidate.account_state().get("password_hash").is_none());
    assert_eq!(candidate.account_state()["status"], "active");
    assert!(candidate.finish().is_none());
    account["status"] = json!("disabled");
    f.save(&user, &account).await;
    assert!(repo
        .inspect_session(&session.token, 100)
        .await
        .unwrap()
        .is_none());
}
#[test]
fn authorization_role_catalog_is_not_embedded_in_the_persistence_contract() {
    assert!(InitialAccess {
        role: "external_role",
        tier_id: "external_tier",
        profile_is_admin: false
    }
    .validate()
    .is_ok());
    assert!(InitialAccess {
        role: "bad role",
        tier_id: "external_tier",
        profile_is_admin: false
    }
    .validate()
    .is_err());
}
#[tokio::test]
async fn cloned_login_budgets_share_the_existing_window() {
    let budget = LoginAttempts::default();
    let peer = budget.clone();
    for _ in 0..4 {
        assert!(
            budget
                .admit(
                    crate::Admission::Login,
                    std::net::IpAddr::from([127, 0, 0, 1]),
                    "member",
                    100
                )
                .await
        );
    }
    assert!(
        peer.admit(
            crate::Admission::Login,
            std::net::IpAddr::from([127, 0, 0, 1]),
            "member",
            100
        )
        .await
    );
    assert!(
        !budget
            .admit(
                crate::Admission::Login,
                std::net::IpAddr::from([127, 0, 0, 1]),
                "member",
                100
            )
            .await
    );
    assert!(
        !peer
            .admit(
                crate::Admission::Login,
                std::net::IpAddr::from([127, 0, 0, 1]),
                "member",
                100
            )
            .await
    );
}

#[tokio::test]
async fn existing_admin_import_preserves_credentials_profiles_and_rollback_material() {
    let f = Fixture::new().await;
    let repo = f.repo();
    let legacy = json!({"username":"imported-operator","password_hash":"legacy-fixture-hash","generation":"legacy-generation"});
    let blob = f.codec.seal(&legacy).unwrap();
    sqlx::query("INSERT INTO docs(key,value) VALUES('admin',?)")
        .bind(&blob)
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(repo.legacy_admin_exists().await.unwrap());
    repo.import_original_admin().await.unwrap();
    repo.import_original_admin().await.unwrap();
    let account = repo
        .account_by_username("imported-operator")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(account["user_id"], "admin");
    assert_eq!(account["password_hash"], legacy["password_hash"]);
    assert_eq!(account["generation"], legacy["generation"]);
    assert_eq!(
        repo.user_profile("admin").await.unwrap().unwrap()["default_agent_id"],
        "shared_default"
    );
    let retained: Vec<u8> = sqlx::query_scalar("SELECT value FROM docs WHERE key='admin'")
        .fetch_one(&f.pool)
        .await
        .unwrap();
    assert_eq!(retained, blob);
    let public = repo
        .account_state_by_username("imported-operator")
        .await
        .unwrap()
        .unwrap();
    assert!(public.get("password_hash").is_none());
}
#[tokio::test]
async fn diagnostics_controller_keeps_bound_caller_live_authority_and_ten_minute_expiry() {
    use crate::api::{Error, Host};
    use axum::http::{HeaderMap, StatusCode};
    let f = Fixture::new().await;
    let repo = f.repo();
    let host = AuthHost::new(&f);
    host.create_administrator("operator", "fixture-admin-hash", "generation")
        .await
        .unwrap();
    let fixture_token = "d".repeat(32);
    let bridge = BrowserLoginBridge::configure(&fixture_token, "fixture-operator")
        .unwrap()
        .unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {fixture_token}").parse().unwrap(),
    );
    let request = || crate::browser_login_api::LoginRequest {
        caller_id: "fixture-operator".into(),
    };
    let response = crate::browser_login_api::login(
        &repo,
        &host,
        Some(&bridge),
        &headers,
        request(),
        true,
        100,
    )
    .await
    .unwrap();
    let (cookie, body) = controller_response(response).await;
    assert_eq!(body["expires_in"], 600);
    assert_eq!(body["user_id"], "admin");
    assert_eq!(cookie["cache-control"], "no-store");
    assert!(cookie["set-cookie"]
        .to_str()
        .unwrap()
        .contains("Max-Age=600"));
    let token = controller_token(&cookie);
    assert!(repo
        .authenticate_session(&token, 699, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    assert!(repo
        .authenticate_session(&token, 700, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    host.administrator_allowed.store(false, Ordering::SeqCst);
    assert!(matches!(
        crate::browser_login_api::login(
            &repo,
            &host,
            Some(&bridge),
            &headers,
            request(),
            true,
            100
        )
        .await,
        Err(Error::Public(StatusCode::UNAUTHORIZED, _))
    ));
    host.administrator_allowed.store(true, Ordering::SeqCst);
    headers.insert("origin", "https://native.test".parse().unwrap());
    assert!(matches!(
        crate::browser_login_api::login(
            &repo,
            &host,
            Some(&bridge),
            &headers,
            request(),
            true,
            100
        )
        .await,
        Err(Error::Public(StatusCode::UNAUTHORIZED, _))
    ));
}
#[tokio::test]
async fn trusted_browser_issuance_keeps_inspected_generation_and_account_status() {
    let f = Fixture::new().await;
    let id = f.active("trusted-browser-owner").await;
    let repo = f.repo();
    let mut account = repo.account_by_id(&id).await.unwrap().unwrap();
    let generation = account["generation"].as_str().unwrap().to_owned();
    let session = repo
        .issue_trusted_browser_session(&id, &generation, 100, "bounded trusted browser", 600)
        .await
        .unwrap();
    assert_eq!(session.expires, 700);
    assert!(repo
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_some());
    account["generation"] = json!("rotated-browser-generation");
    f.save(&id, &account).await;
    assert!(repo
        .issue_trusted_browser_session(&id, &generation, 100, "browser", 600)
        .await
        .is_err());
    assert!(repo
        .authenticate_session(&session.token, 100, RegistrationMode::Restricted)
        .await
        .unwrap()
        .is_none());
    account["status"] = json!("disabled");
    f.save(&id, &account).await;
    assert!(repo
        .issue_trusted_browser_session(&id, "rotated-browser-generation", 100, "browser", 600)
        .await
        .is_err());
}
#[tokio::test]
async fn initial_tier_repair_joins_transaction_and_preserves_identity_and_generation() {
    let f = Fixture::new().await;
    let repo = f.repo();
    let guest = repo
        .create_guest_account(InitialAccess {
            role: "guest",
            tier_id: "basic",
            profile_is_admin: false,
        })
        .await
        .unwrap();
    let id = guest["user_id"].as_str().unwrap();
    let before = repo.account_state(id).await.unwrap().unwrap();
    let mut tx = f.pool.begin().await.unwrap();
    repo.initialize_tier_assignment_in(&mut tx, "guest", "anonymous")
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(repo.account_state(id).await.unwrap().unwrap(), before);
    let mut tx = f.pool.begin().await.unwrap();
    repo.initialize_tier_assignment_in(&mut tx, "guest", "anonymous")
        .await
        .unwrap();
    repo.initialize_tier_assignment_in(&mut tx, "guest", "anonymous")
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let after = repo.account_state(id).await.unwrap().unwrap();
    assert_eq!(after["user_id"], before["user_id"]);
    assert_eq!(after["generation"], before["generation"]);
    assert_eq!(after["tier_id"], "anonymous");
    assert_eq!(
        after["access_revision"].as_u64().unwrap(),
        before["access_revision"].as_u64().unwrap() + 1
    );
}

#[tokio::test]
async fn browser_session_references_keep_expiry_revocation_identity_and_namespace() {
    let f = Fixture::new().await;
    let id = f.active("reference-owner").await;
    let repo = f.repo();
    let first = repo
        .fixture_session(&id, 100, "source browser")
        .await
        .unwrap();
    let key = crate::sessions::auth_key(&first.token);
    let generation = repo.account_state(&id).await.unwrap().unwrap()["generation"].clone();
    assert!(repo
        .session_reference_live(&key, &id, &generation, 100)
        .await
        .unwrap());
    assert!(!repo
        .session_reference_live(&key, "other", &generation, 100)
        .await
        .unwrap());
    assert!(!repo
        .session_reference_live(&key, &id, &json!("wrong-generation"), 100)
        .await
        .unwrap());
    assert!(!repo
        .session_reference_live(&key, &id, &generation, first.expires)
        .await
        .unwrap());
    let doc = repo.auth_document(&first.token).await.unwrap().unwrap();
    sqlx::query("INSERT INTO docs(key,value) VALUES('notification_reader:fixture',?)")
        .bind(f.codec.seal(&doc).unwrap())
        .execute(&f.pool)
        .await
        .unwrap();
    assert!(!repo
        .session_reference_live("notification_reader:fixture", &id, &generation, 100)
        .await
        .unwrap());
    let mut account = repo.account_by_id(&id).await.unwrap().unwrap();
    account["generation"] = json!("rotated-reference-generation");
    f.save(&id, &account).await;
    assert!(!repo
        .session_reference_live(&key, &id, &generation, 100)
        .await
        .unwrap());
    let next = repo
        .fixture_session(&id, 100, "next browser")
        .await
        .unwrap();
    let next_key = crate::sessions::auth_key(&next.token);
    let next_generation = account["generation"].clone();
    assert!(repo
        .session_reference_live(&next_key, &id, &next_generation, 100)
        .await
        .unwrap());
    repo.logout(&next.token).await.unwrap();
    assert!(!repo
        .session_reference_live(&next_key, &id, &next_generation, 100)
        .await
        .unwrap());
    let disabled = repo
        .fixture_session(&id, 100, "disabled browser")
        .await
        .unwrap();
    account["status"] = json!("disabled");
    f.save(&id, &account).await;
    assert!(!repo
        .session_reference_live(
            &crate::sessions::auth_key(&disabled.token),
            &id,
            &next_generation,
            100
        )
        .await
        .unwrap());
}

#[test]
fn password_minimum_counts_characters_and_bounds_bytes() {
    assert!(!crate::password_length_ok("short"));
    assert!(!crate::password_length_ok(&"é".repeat(7)));
    assert!(crate::password_length_ok(&"é".repeat(8)));
    assert!(!crate::password_length_ok(&"x".repeat(1025)));
}
