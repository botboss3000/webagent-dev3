use super::*;
use crate::{
    repository::{RecordCodec, Repository},
    service::{Access, Effects, Service, Storage},
};
use anyhow::{anyhow, bail};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
    Row, SqlitePool,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
};
use webagent_access::{AccountStatus, Principal, Relationship, Role};

struct Fixture {
    _root: tempfile::TempDir,
    pools: BTreeMap<String, SqlitePool>,
    receipts: Mutex<BTreeMap<String, Value>>,
    registered: Mutex<BTreeSet<String>>,
    allowed: AtomicBool,
    active: AtomicBool,
    guest: AtomicBool,
    fail_receipt: AtomicBool,
    pool_reads: AtomicUsize,
    quiesced: AtomicUsize,
    events: Mutex<Vec<Value>>,
    gate: tokio::sync::Mutex<()>,
}
impl RecordCodec for Fixture {
    // Fixture encoding distinguishes tenant scope; production uses the existing Storage codec.
    fn seal(&self, v: &Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({"scope":"creation","value":v}))?)
    }
    fn unseal(&self, v: &[u8]) -> Result<Value> {
        let record: Value = serde_json::from_slice(v)?;
        if record["scope"] != "creation" {
            bail!("Wrong codec scope");
        }
        Ok(record["value"].clone())
    }
    fn encode_application(&self, v: &Value) -> Result<Vec<u8>> {
        self.seal(v)
    }
    fn seal_agent_for(&self, agent: &str, v: &Value) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&json!({"scope":agent,"value":v}))?)
    }
    fn unseal_agent_for(&self, agent: &str, v: &[u8]) -> Result<Value> {
        let record: Value = serde_json::from_slice(v)?;
        if record["scope"] != agent {
            bail!("Wrong tenant");
        }
        Ok(record["value"].clone())
    }
}
impl Storage for Fixture {
    async fn agent_ids(&self) -> Result<Vec<String>> {
        Ok(self.registered.lock().unwrap().iter().cloned().collect())
    }
    async fn agent_pool(&self, id: &str) -> Result<SqlitePool> {
        self.pool_reads.fetch_add(1, Ordering::SeqCst);
        self.pools
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow!("Agent not found"))
    }
    async fn creation_pool(&self, id: &str) -> Result<SqlitePool> {
        self.agent_pool(id).await
    }
    async fn registered(&self, id: &str) -> Result<bool> {
        Ok(self.registered.lock().unwrap().contains(id))
    }
    async fn register(&self, id: &str, owner: &str) -> Result<()> {
        if !self.registered(id).await? {
            let pool = self.agent_pool(id).await?;
            let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
                .bind(id)
                .fetch_one(&pool)
                .await?;
            let metadata = self.unseal(&blob)?;
            if metadata["owner_id"] != owner {
                bail!("Owner mismatch");
            }
            sqlx::query("UPDATE agents SET metadata=? WHERE id=?")
                .bind(self.seal_agent_for(id, &metadata)?)
                .bind(id)
                .execute(&pool)
                .await?;
            self.registered.lock().unwrap().insert(id.into());
        }
        Ok(())
    }
    async fn receipt(&self, key: &str) -> Result<Option<Value>> {
        Ok(self.receipts.lock().unwrap().get(key).cloned())
    }
    async fn save_receipt(&self, key: &str, value: &Value) -> Result<()> {
        if self.fail_receipt.load(Ordering::SeqCst) {
            bail!("Injected receipt failure");
        }
        self.receipts
            .lock()
            .unwrap()
            .insert(key.into(), value.clone());
        Ok(())
    }
}
impl Access for Fixture {
    async fn principal(&self) -> Result<Principal> {
        Ok(Principal {
            user_id: "owner".into(),
            status: if self.active.load(Ordering::SeqCst) {
                AccountStatus::Active
            } else {
                AccountStatus::Disabled
            },
            role: if self.guest.load(Ordering::SeqCst) {
                Role::Guest
            } else {
                Role::User
            },
            tier_id: "member".into(),
            access_revision: 1,
        })
    }
    async fn authority(&self, agent: &str, _write: bool) -> Result<Value> {
        if !self.allowed.load(Ordering::SeqCst) || agent == "foreign" {
            bail!("Denied");
        }
        Ok(json!({"can_manage":true,"owner_id":"owner"}))
    }
    async fn ensure_owner(
        &self,
        owner: &str,
        _agent: &str,
        relationship: Relationship,
    ) -> Result<()> {
        if owner != "owner" || relationship != Relationship::Owner {
            bail!("Owner mismatch");
        }
        Ok(())
    }
    async fn status_authority(&self, _agent: &str) -> Result<()> {
        Ok(())
    }
}
impl Effects for Fixture {
    async fn seed_model(&self, _owner: &str, _agent: &str) -> Result<bool> {
        Ok(false)
    }
    async fn status_event(&self, value: &Value) -> Result<()> {
        self.events.lock().unwrap().push(value.clone());
        Ok(())
    }
    async fn quiesce(&self, _agent: &str) -> Result<()> {
        self.quiesced.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
fn request() -> NewAgent {
    NewAgent {
        client_request_id: "new-agent".into(),
        name: "  Research  ".into(),
        description: Some("Description".into()),
    }
}
impl Fixture {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut pools = BTreeMap::new();
        for id in [
            "shared_default".to_owned(),
            "foreign".to_owned(),
            request().id("owner").unwrap(),
        ] {
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::new()
                        .filename(root.path().join(format!("{id}.db")))
                        .create_if_missing(true),
                )
                .await
                .unwrap();
            sqlx::raw_sql("CREATE TABLE agents(id TEXT PRIMARY KEY,name TEXT NOT NULL,description TEXT NOT NULL,provider TEXT NOT NULL,model TEXT NOT NULL,status TEXT NOT NULL,metadata BLOB NOT NULL,revision INTEGER NOT NULL,created_at INTEGER NOT NULL,updated_at INTEGER NOT NULL);
                CREATE TABLE retained_work(id TEXT PRIMARY KEY,agent_id TEXT NOT NULL);").execute(&pool).await.unwrap();
            pools.insert(id, pool);
        }
        let f = Self {
            _root: root,
            pools,
            receipts: Mutex::new(BTreeMap::new()),
            registered: Mutex::new(BTreeSet::from(["shared_default".into(), "foreign".into()])),
            allowed: AtomicBool::new(true),
            active: AtomicBool::new(true),
            guest: AtomicBool::new(false),
            fail_receipt: AtomicBool::new(false),
            pool_reads: AtomicUsize::new(0),
            quiesced: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
            gate: tokio::sync::Mutex::new(()),
        };
        for id in ["shared_default", "foreign"] {
            let metadata = json!({"owner_id":if id=="foreign"{"other"}else{"owner"},"ability_revision":8,"member_access":{"visitor_access":false},"tool_output_policy":{"keep":"neighbor"}});
            sqlx::query(
                "INSERT INTO agents VALUES(?,?,'','openai_subscription','','active',?,1,1,1)",
            )
            .bind(id)
            .bind(id)
            .bind(f.seal_agent_for(id, &metadata).unwrap())
            .execute(&f.pools[id])
            .await
            .unwrap();
        }
        f
    }
    fn service(&self) -> Service<'_, Self, Self, Self> {
        Service {
            storage: self,
            access: self,
            effects: self,
            gate: &self.gate,
        }
    }
    fn repo(&self) -> Repository<'_, Self> {
        Repository::new(&self.pools["shared_default"], "shared_default", self).unwrap()
    }
}

#[test]
fn metadata_validation_and_mode_selection() {
    let mut metadata = json!({"owner_id":"owner","ability_revision":7,"manager_incoming_message":{"enabled":true}});
    let update = IdentityUpdate {
        revision: 1,
        execution_modes: Some(
            json!([{"id":"review","label":" Review ","prompt":"Read only","permission_policy":"read_only"}]),
        ),
        default_execution_mode: Some("review".into()),
        ..Default::default()
    };
    update.validate().unwrap();
    update.apply(&mut metadata).unwrap();
    assert_eq!(metadata["owner_id"], "owner");
    assert_eq!(metadata["ability_revision"], 7);
    assert_eq!(metadata["manager_incoming_message"]["enabled"], true);
    let config = modes::config(&metadata).unwrap();
    assert_eq!(
        modes::resolve(&config, "review").unwrap()["label"],
        "Review"
    );
    assert!(modes::resolve(&json!({}), "ask").is_err());
    assert!(modes::resolve(&config, "missing").is_err());
    assert!(IdentityUpdate {
        default_execution_mode: Some("missing".into()),
        ..Default::default()
    }
    .apply(&mut metadata)
    .is_err());
    let remove = IdentityUpdate {
        execution_modes: Some(json!([])),
        ..Default::default()
    };
    remove.apply(&mut metadata).unwrap();
    assert_eq!(metadata["default_execution_mode"], "ask");
}
#[test]
fn invalid_fields_and_mode_boundaries() {
    for raw in [
        json!({}),
        json!([{"id":"ask"},{"id":"ask"}]),
        json!([{"id":"Bad"}]),
        json!([{"id":"x","permission_policy":"admin"}]),
        json!([{"id":"x","label":" "}]),
    ] {
        assert!(modes::normalize(&raw).is_err());
    }
    let custom: Vec<Value> = (0..17)
        .map(|i| json!({"id":format!("custom{i}")}))
        .collect();
    assert_eq!(
        modes::normalize(&json!(custom))
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        20
    );
    let too_many: Vec<Value> = (0..18)
        .map(|i| json!({"id":format!("custom{i}")}))
        .collect();
    assert!(modes::normalize(&json!(too_many)).is_err());
    for name in [" ".to_owned(), "é".repeat(101)] {
        assert!(validate_identity(Some(&name), None).is_err());
    }
    assert!(validate_identity(None, Some(&"x".repeat(2001))).is_err());
    assert!(IdentityUpdate::default().validate().is_err());
    assert!(IdentityUpdate {
        revision: i64::MAX,
        name: Some("Valid".into()),
        ..Default::default()
    }
    .validate()
    .is_err());
    assert!(
        serde_json::from_value::<IdentityUpdate>(json!({"revision":1,"owner_id":"other"})).is_err()
    );
    assert!(serde_json::from_value::<NewAgent>(
        json!({"name":"Name","client_request_id":"id","model":"private"})
    )
    .is_err());
    assert!(validate_status(1, "deleted").is_err());
    assert!(validate_status(i64::MAX as u64, "active").is_err());
}
#[tokio::test]
async fn revision_conflicts_preserve_neighbor_settings_and_other_tenants() {
    let f = Fixture::new().await;
    let service = f.service();
    let update = IdentityUpdate {
        revision: 1,
        name: Some(" Renamed ".into()),
        ..Default::default()
    };
    let saved = service.update("shared_default", &update, 5).await.unwrap();
    assert_eq!(saved["name"], "Renamed");
    assert_eq!(saved["revision"], 2);
    assert!(service.update("shared_default", &update, 6).await.is_err());
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id='shared_default'")
        .fetch_one(&f.pools["shared_default"])
        .await
        .unwrap();
    let metadata = f.unseal_agent_for("shared_default", &blob).unwrap();
    assert_eq!(metadata["owner_id"], "owner");
    assert_eq!(metadata["ability_revision"], 8);
    assert_eq!(metadata["tool_output_policy"]["keep"], "neighbor");
    let row = sqlx::query("SELECT name,revision FROM agents")
        .fetch_one(&f.pools["foreign"])
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("name"), "foreign");
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert!(f.unseal_agent_for("foreign", &blob).is_err());
}
#[tokio::test]
async fn unauthorized_and_inactive_callers_do_not_open_tenant_storage() {
    let f = Fixture::new().await;
    assert!(f.service().identity("foreign").await.is_err());
    assert_eq!(f.pool_reads.load(Ordering::SeqCst), 0);
    f.allowed.store(false, Ordering::SeqCst);
    assert!(f
        .service()
        .update(
            "shared_default",
            &IdentityUpdate {
                revision: 1,
                name: Some("Wrong".into()),
                ..Default::default()
            },
            2
        )
        .await
        .is_err());
    assert_eq!(f.pool_reads.load(Ordering::SeqCst), 0);
    f.active.store(false, Ordering::SeqCst);
    assert!(f.service().create(&request(), 2).await.is_err());
    assert!(f.service().list().await.is_err());
    assert_eq!(f.pool_reads.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn creation_retries_reconcile_registered_row_after_receipt_failure() {
    let f = Fixture::new().await;
    f.fail_receipt.store(true, Ordering::SeqCst);
    assert!(f.service().create(&request(), 2).await.is_err());
    f.fail_receipt.store(false, Ordering::SeqCst);
    let saved = f.service().create(&request(), 3).await.unwrap();
    assert_eq!(saved["assignment_required"], true);
    assert_eq!(f.service().create(&request(), 4).await.unwrap(), saved);
    let mut changed = request();
    changed.name = "Other".into();
    assert!(f.service().create(&changed, 5).await.is_err());
    let id = request().id("owner").unwrap();
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM agents")
        .fetch_one(&f.pools[&id])
        .await
        .unwrap();
    assert_eq!(count, 1);
    let mut metadata: Value = f
        .unseal_agent_for(
            &id,
            &sqlx::query_scalar::<_, Vec<u8>>("SELECT metadata FROM agents")
                .fetch_one(&f.pools[&id])
                .await
                .unwrap(),
        )
        .unwrap();
    assert_eq!(metadata["ability_modes"]["codebase_admin"], "off");
    assert_eq!(metadata["owner_id"], "owner");
    metadata["creation_digest"] = json!("mismatch");
    sqlx::query("UPDATE agents SET metadata=?")
        .bind(f.seal_agent_for(&id, &metadata).unwrap())
        .execute(&f.pools[&id])
        .await
        .unwrap();
    f.receipts.lock().unwrap().clear();
    assert!(f.service().create(&request(), 6).await.is_err());
}
#[tokio::test]
async fn pre_registration_retry_and_guest_creation_rejection() {
    let f = Fixture::new().await;
    let req = request();
    let id = req.id("owner").unwrap();
    repository::create_record(&f.pools[&id], &f, &id, "owner", &req, false, 2)
        .await
        .unwrap();
    repository::create_record(&f.pools[&id], &f, &id, "owner", &req, false, 3)
        .await
        .unwrap();
    assert!(
        repository::create_record(&f.pools[&id], &f, &id, "other", &req, false, 3)
            .await
            .is_err()
    );
    assert!(
        repository::create_record(&f.pools[&id], &f, &id, "owner", &req, true, 3)
            .await
            .is_err()
    );
    f.guest.store(true, Ordering::SeqCst);
    assert!(f.service().create(&req, 4).await.is_err());
    f.guest.store(false, Ordering::SeqCst);
    assert_eq!(f.service().create(&req, 5).await.unwrap()["id"], id);
}
#[tokio::test]
async fn status_change_retains_references_and_quiesces_active_work() {
    let f = Fixture::new().await;
    sqlx::query("INSERT INTO retained_work VALUES('run-1','shared_default')")
        .execute(&f.pools["shared_default"])
        .await
        .unwrap();
    let saved = f
        .service()
        .set_status("shared_default", 1, "disabled")
        .await
        .unwrap();
    assert_eq!(saved["revision"], 2);
    assert_eq!(f.quiesced.load(Ordering::SeqCst), 1);
    assert_eq!(f.events.lock().unwrap().len(), 1);
    assert!(f
        .service()
        .set_status("shared_default", 1, "active")
        .await
        .is_err());
    assert_eq!(f.quiesced.load(Ordering::SeqCst), 1);
    assert_eq!(f.repo().status().await.unwrap()["status"], "disabled");
    let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM retained_work")
        .fetch_one(&f.pools["shared_default"])
        .await
        .unwrap();
    assert_eq!(count, 1);
    f.service()
        .set_status("shared_default", 2, "active")
        .await
        .unwrap();
    assert_eq!(f.repo().status().await.unwrap()["revision"], 3);
}
#[tokio::test]
async fn listing_filters_foreign_agents_and_projects_no_private_metadata() {
    let f = Fixture::new().await;
    let listed = f.service().list().await.unwrap();
    assert_eq!(listed["agents"].as_array().unwrap().len(), 1);
    assert_eq!(listed["agents"][0]["id"], "shared_default");
    assert!(listed["agents"][0].get("owner_id").is_none());
    assert!(listed["agents"][0].get("execution_modes").is_none());
}
#[tokio::test]
async fn metadata_reload_is_immediate_and_outer_transaction_rollback_is_preserved() {
    let f = Fixture::new().await;
    let update = IdentityUpdate {
        revision: 1,
        default_execution_mode: Some("plan".into()),
        ..Default::default()
    };
    let mut tx = f.pools["shared_default"].begin().await.unwrap();
    f.repo().update_in(&mut tx, &update, 2).await.unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        f.repo().identity().await.unwrap()["default_execution_mode"],
        "ask"
    );
    f.repo().update(&update, 3).await.unwrap();
    assert_eq!(
        f.repo().identity().await.unwrap()["default_execution_mode"],
        "plan"
    );
    let mut tx = f.pools["shared_default"].begin().await.unwrap();
    let next = IdentityUpdate {
        revision: 2,
        default_execution_mode: Some("auto".into()),
        ..Default::default()
    };
    f.repo().update_in(&mut tx, &next, 4).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.repo().identity().await.unwrap()["default_execution_mode"],
        "auto"
    );
}

#[test]
fn stable_identity_matches_existing_request_digest_contract() {
    assert_eq!(
        request().id("owner").unwrap(),
        "agent-b96beffb9894ac16f293ad30dfcd08f8"
    );
    assert_ne!(
        request().id("owner").unwrap(),
        request().id("other").unwrap()
    );
    assert_eq!(
        request().receipt_key("owner"),
        "receipt:model-agent:owner:new-agent"
    );
}
