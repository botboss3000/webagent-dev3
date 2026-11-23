//! Application-wide requester slots. Absent canonical commits remain reserved.
use super::*;
impl Store {
    async fn requester_slots_schema(&self) -> Result<()> {
        // Checked once per state root and process; every admission ran it before.
        static READY: std::sync::LazyLock<
            std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
        > = std::sync::LazyLock::new(Default::default);
        if READY
            .lock()
            .map_err(|_| anyhow!("Slot schema registry unavailable"))?
            .contains(&self.root)
        {
            return Ok(());
        }
        self.create_requester_slots().await?;
        READY
            .lock()
            .map_err(|_| anyhow!("Slot schema registry unavailable"))?
            .insert(self.root.clone());
        Ok(())
    }
    async fn create_requester_slots(&self) -> Result<()> {
        sqlx::query("CREATE TABLE IF NOT EXISTS requester_run_slots(run_id TEXT PRIMARY KEY,requester_id TEXT NOT NULL,owner_id TEXT NOT NULL,session_id TEXT NOT NULL,released INTEGER NOT NULL DEFAULT 0,sequence INTEGER NOT NULL,replaced_by TEXT); CREATE INDEX IF NOT EXISTS requester_run_slots_actor ON requester_run_slots(requester_id,released)").execute(&self.app_pool).await?;
        // `agent_id` and `admin` (the requester was an administrator when last checked) serve the
        // per-agent cap; `released` is 0 while held, 1 once the run finished and 2 while it is
        // stopped, paused or parked (taken again when it resumes).
        for (column, declaration) in [
            ("replaced_by", "TEXT"),
            ("agent_id", "TEXT"),
            ("admin", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            let has: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pragma_table_info('requester_run_slots') WHERE name=?",
            )
            .bind(column)
            .fetch_one(&self.app_pool)
            .await?;
            if has != 0 {
                continue;
            }
            if let Err(error) = sqlx::query(&format!(
                "ALTER TABLE requester_run_slots ADD COLUMN {column} {declaration}"
            ))
            .execute(&self.app_pool)
            .await
            {
                let has: i64 = sqlx::query_scalar(
                    "SELECT count(*) FROM pragma_table_info('requester_run_slots') WHERE name=?",
                )
                .bind(column)
                .fetch_one(&self.app_pool)
                .await?;
                if has == 0 {
                    return Err(error.into());
                }
            }
        }
        sqlx::query("CREATE INDEX IF NOT EXISTS requester_run_slots_agent ON requester_run_slots(agent_id,released,admin)")
            .execute(&self.app_pool)
            .await?;
        Ok(())
    }
    /// The first reconcile of a process reads every account (nothing is marked applied yet);
    /// run it once soon after start-up so no request waits for it. It waits a minute so it
    /// does not open every account while the start-up passes (the scan's first look, storage
    /// limits) still hold theirs: each open connection is a thread, and together they reached
    /// the container's task limit. A failure only leaves that work to the first admission.
    pub(crate) async fn prime_requester_slots(&self) {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        if let Err(error) = self.reconcile_requester_slots().await {
            tracing::warn!("Requester slot reconciliation deferred: {error}");
        }
    }
    pub(crate) async fn reconcile_requester_slots(&self) -> Result<()> {
        // One reconcile at a time per process: a second caller waits and then finds only the
        // accounts written meanwhile, instead of reading the same accounts concurrently.
        static RECONCILE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _one = RECONCILE.lock().await;
        self.requester_slots_schema().await?;
        // Read canonical WAL snapshots before the app writer. New admissions reserve
        // first. Absence cannot prove an interrupted canonical commit did not happen.
        let mut records = Vec::new();
        // Records derive only from account contents, so an account unchanged since it was
        // applied is skipped; it is marked applied only after the slot transaction commits.
        let mut changed = self.changed_user_ids("requester_slots").await?;
        for (owner, mark) in &mut changed {
            let owner = owner.clone();
            let path = self
                .root
                .join("user_data")
                .join(&owner)
                .join(format!("{owner}.db"));
            if !path.exists() {
                continue;
            }
            // Reuse the account's cached pool: a fresh connection per account on every admission
            // spawned a thread and reopened the database. Range bounds use the docs key index.
            let pool = self.for_user(&owner).await?.pool.clone();
            // After opening (which can touch the log) and before reading: a write during
            // the read moves the mark and is applied next time.
            *mark = self.wal_mark(&owner);
            // Runs come from the plain run index (its status is saved with the run): a
            // finished run releases its unreleased slot, and only an unfinished run without a
            // slot is decrypted, to name its requester. This is what decrypting every run
            // record and releasing or inserting each did, since both writes were idempotent.
            let unreleased: Vec<String> = sqlx::query_scalar(
                "SELECT run_id FROM requester_run_slots WHERE owner_id=? AND released=0",
            )
            .bind(&owner)
            .fetch_all(&self.app_pool)
            .await?;
            let held: std::collections::HashSet<String> = unreleased.iter().cloned().collect();
            for rid in unreleased {
                let status: Option<String> =
                    sqlx::query_scalar("SELECT status FROM run_index WHERE id=?")
                        .bind(&rid)
                        .fetch_optional(&pool)
                        .await?;
                match status.as_deref() {
                    Some("completed" | "cancelled" | "failed") => {
                        records.push((
                            rid,
                            owner.clone(),
                            owner.clone(),
                            String::new(),
                            Slot::Finish,
                        ));
                    }
                    // User decision 11-23: a stopped, paused or parked run gives its slot back.
                    Some("stopped" | "paused" | "parked") => {
                        records.push((rid, owner.clone(), owner.clone(), String::new(), Slot::Set));
                    }
                    _ => {}
                }
            }
            // A run that resumed (back to queued, running or waiting) takes its slot again,
            // at the end of the line, without the admission limit: it waits its turn at claim.
            let set_aside: Vec<String> = sqlx::query_scalar(
                "SELECT run_id FROM requester_run_slots WHERE owner_id=? AND released=2",
            )
            .bind(&owner)
            .fetch_all(&self.app_pool)
            .await?;
            for rid in set_aside {
                let status: Option<String> =
                    sqlx::query_scalar("SELECT status FROM run_index WHERE id=?")
                        .bind(&rid)
                        .fetch_optional(&pool)
                        .await?;
                match status.as_deref() {
                    Some("queued" | "running" | "waiting_approval" | "waiting_external") => {
                        records.push((
                            rid,
                            owner.clone(),
                            owner.clone(),
                            String::new(),
                            Slot::Retake,
                        ));
                    }
                    Some("completed" | "cancelled" | "failed") => {
                        records.push((
                            rid,
                            owner.clone(),
                            owner.clone(),
                            String::new(),
                            Slot::Finish,
                        ));
                    }
                    _ => {}
                }
            }
            let open: Vec<String> = sqlx::query_scalar(
                "SELECT id FROM run_index WHERE status NOT IN ('completed','cancelled','failed') AND status NOT IN ('stopped','paused','parked')",
            )
            .fetch_all(&pool)
            .await?;
            for rid in open {
                let slotted: Option<i64> =
                    sqlx::query_scalar("SELECT 1 FROM requester_run_slots WHERE run_id=?")
                        .bind(&rid)
                        .fetch_optional(&self.app_pool)
                        .await?;
                if slotted.is_some() {
                    continue;
                }
                let Some(raw) =
                    sqlx::query_scalar::<_, Vec<u8>>("SELECT value FROM docs WHERE key=?")
                        .bind(format!("run:{rid}"))
                        .fetch_optional(&pool)
                        .await?
                else {
                    continue;
                };
                let run = self.unseal(&raw)?;
                if run["id"].as_str() != Some(rid.as_str())
                    || matches!(
                        run["status"].as_str(),
                        Some("completed" | "cancelled" | "failed")
                    )
                {
                    continue;
                }
                let Some(sid) = run["session_id"].as_str() else {
                    continue;
                };
                records.push((
                    rid.clone(),
                    run["requester_user_id"]
                        .as_str()
                        .unwrap_or(&owner)
                        .to_owned(),
                    owner.clone(),
                    sid.to_owned(),
                    Slot::Hold,
                ));
            }
            let rows=sqlx::query("SELECT key,value FROM docs WHERE (key>='manager_review:' AND key<'manager_review;') OR (key>='followup_queue:' AND key<'followup_queue;')").fetch_all(&pool).await?;
            for row in rows {
                let key: String = row.get("key");
                let value = self.unseal(&row.get::<Vec<u8>, _>("value"))?;
                if key.starts_with("followup_queue:") {
                    for entry in value["entries"].as_array().into_iter().flatten() {
                        let Some(id) = entry["id"].as_str() else {
                            continue;
                        };
                        let Some(sid) = value["session_id"].as_str() else {
                            continue;
                        };
                        let reviewing = entry["review_status"] == "reviewing";
                        let terminal = matches!(
                            entry["review_status"].as_str(),
                            Some("acceptable" | "unacceptable" | "review_error" | "cancelled")
                        ) || matches!(
                            entry["status"].as_str(),
                            Some("cancelled" | "rejected" | "delivered")
                        );
                        // Releasing a slot that is already released or absent changes
                        // nothing; every message admission used to repeat one such update
                        // for each finished follow-up the account ever had.
                        let slot = format!("followup-review:{owner}:{sid}:{id}");
                        if reviewing || (terminal && held.contains(&slot)) {
                            records.push((
                                slot,
                                owner.clone(),
                                owner.clone(),
                                sid.to_owned(),
                                Slot::finished(terminal),
                            ));
                        }
                    }
                    continue;
                }
                let review = key.starts_with("manager_review:");
                let run = if review { &value["run"] } else { &value };
                let Some(rid) = run["id"].as_str() else {
                    continue;
                };
                let Some(sid) = run["session_id"].as_str() else {
                    continue;
                };
                let terminal = if review {
                    matches!(
                        value["status"].as_str(),
                        Some("unacceptable" | "review_error" | "cancelled")
                    )
                } else {
                    matches!(
                        run["status"].as_str(),
                        Some("completed" | "cancelled" | "failed")
                    )
                };
                if review && value["status"] == "acceptable" {
                    continue;
                }
                if terminal && !held.contains(rid) {
                    continue;
                }
                records.push((
                    rid.to_owned(),
                    run["requester_user_id"]
                        .as_str()
                        .unwrap_or(&owner)
                        .to_owned(),
                    owner.clone(),
                    sid.to_owned(),
                    Slot::finished(terminal),
                ));
            }
        }
        let mut tx = self.app_pool.begin().await?;
        for (rid, actor, owner, sid, slot) in records {
            match slot {
                Slot::Finish => {
                    sqlx::query(
                        "UPDATE requester_run_slots SET released=1 WHERE run_id=? AND owner_id=?",
                    )
                    .bind(rid)
                    .bind(owner)
                    .execute(&mut *tx)
                    .await?;
                }
                Slot::Set => {
                    sqlx::query("UPDATE requester_run_slots SET released=2 WHERE run_id=? AND owner_id=? AND released=0")
                        .bind(rid)
                        .bind(owner)
                        .execute(&mut *tx)
                        .await?;
                }
                Slot::Retake => {
                    sqlx::query("UPDATE requester_run_slots SET released=0,sequence=(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots) WHERE run_id=? AND owner_id=? AND released=2")
                        .bind(rid)
                        .bind(owner)
                        .execute(&mut *tx)
                        .await?;
                }
                Slot::Hold => {
                    sqlx::query("INSERT OR IGNORE INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots))").bind(rid).bind(actor).bind(owner).bind(sid).execute(&mut *tx).await?;
                }
            }
        }
        tx.commit().await?;
        Self::mark_seen("requester_slots", changed)
    }
    pub(crate) async fn reserve_requester_run(&self, run: &Value) -> Result<()> {
        self.reserve_requester_slot(run).await.map(|_| ())
    }

    /// Reserve (or confirm) `run`'s slot and return the requester's live access. A new run is
    /// refused past the account level's `max_active_runs`; a run whose slot was set aside while
    /// it was stopped, paused or parked takes it again without that check (it then waits its
    /// turn at claim, so resuming never fails or loses work).
    async fn reserve_requester_slot(&self, run: &Value) -> Result<access::authority::LiveAccess> {
        self.reconcile_requester_slots().await?;
        let rid = run["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run id missing"))?;
        let sid = run["session_id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run session missing"))?;
        let actor = run["requester_user_id"]
            .as_str()
            .unwrap_or(self.current_user_id());
        let agent = run["agent_id"].as_str().unwrap_or("shared_default");
        let access = self.live_access(actor, agent).await?;
        let limit = i64::from(access.max_active_runs());
        let admin = i64::from(access.principal.administrator());
        let mut tx = self.app_pool.begin().await?;
        // Force app writer ownership before reading count; two canonical owners must
        // never both observe the same spare account capacity.
        sqlx::query("UPDATE requester_run_slots SET released=released WHERE run_id=?")
            .bind(rid)
            .execute(&mut *tx)
            .await?;
        let existing=sqlx::query("SELECT requester_id,owner_id,session_id,released FROM requester_run_slots WHERE run_id=?").bind(rid).fetch_optional(&mut *tx).await?;
        if let Some(row) = existing {
            let released = row.get::<i64, _>("released");
            if row.get::<String, _>("requester_id") != actor
                || row.get::<String, _>("owner_id") != self.current_user_id()
                || row.get::<String, _>("session_id") != sid
                || released == 1
                || matches!(
                    run["status"].as_str(),
                    Some("completed" | "cancelled" | "failed")
                )
            {
                return Err(anyhow!("Requester reservation mismatch"));
            }
            if released == 2 {
                sqlx::query("UPDATE requester_run_slots SET released=0,sequence=(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots) WHERE run_id=?")
                    .bind(rid)
                    .execute(&mut *tx)
                    .await?;
            }
            sqlx::query("UPDATE requester_run_slots SET agent_id=?,admin=? WHERE run_id=?")
                .bind(agent)
                .bind(admin)
                .bind(rid)
                .execute(&mut *tx)
                .await?;
        } else {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0",
            )
            .bind(actor)
            .fetch_one(&mut *tx)
            .await?;
            if count >= limit {
                return Err(anyhow!("Account run limit reached"));
            }
            sqlx::query("INSERT INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence,agent_id,admin) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots),?,?)").bind(rid).bind(actor).bind(self.current_user_id()).bind(sid).bind(agent).bind(admin).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(access)
    }

    /// Whether `run` may start now (C5). In one writer transaction on the slot table: the
    /// requester's place among their own held slots is within their level's
    /// `max_active_runs`; for a non-administrator, its place among the agent's held
    /// non-administrator slots is within `run_limits.max_active_runs_per_agent` (default half
    /// the loop's ceiling, at least 2), and the loop has a slot outside the
    /// `reserved_admin_runs`. Places follow the slot sequence, so each line is served in order.
    pub(crate) async fn requester_claim_ready(&self, run: &Value) -> Result<bool> {
        self.requester_claim_ready_within(run, webagent_engine::ports::Capacity::UNBOUNDED)
            .await
    }

    pub(crate) async fn requester_claim_ready_within(
        &self,
        run: &Value,
        capacity: webagent_engine::ports::Capacity,
    ) -> Result<bool> {
        let access = self.reserve_requester_slot(run).await?;
        let rid = run["id"]
            .as_str()
            .ok_or_else(|| anyhow!("Run id missing"))?;
        let limits = access.run_limits;
        let admin = access.principal.administrator();
        if !admin
            && limits
                .ordinary_ceiling(capacity.ceiling)
                .is_some_and(|ordinary| capacity.active >= ordinary)
        {
            return Ok(false);
        }
        let mut tx = self.app_pool.begin_with("BEGIN IMMEDIATE").await?;
        let rank: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=(SELECT requester_id FROM requester_run_slots WHERE run_id=?) AND released=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
            .bind(rid)
            .bind(rid)
            .fetch_one(&mut *tx)
            .await?;
        if rank > i64::from(access.max_active_runs()) {
            return Ok(false);
        }
        if !admin {
            if let Some(cap) = limits.agent_cap(capacity.ceiling) {
                let place: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE agent_id=? AND released=0 AND admin=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
                    .bind(&access.agent_id)
                    .bind(rid)
                    .fetch_one(&mut *tx)
                    .await?;
                if place > i64::try_from(cap).unwrap_or(i64::MAX) {
                    return Ok(false);
                }
            }
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// What a reconcile does to one slot row.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    /// Insert a held slot for an unfinished run that has none.
    Hold,
    /// The run finished: released for good.
    Finish,
    /// The run is stopped, paused or parked: set aside until it resumes.
    Set,
    /// The run resumed: held again, at the end of the line.
    Retake,
}

impl Slot {
    fn finished(terminal: bool) -> Self {
        if terminal {
            Self::Finish
        } else {
            Self::Hold
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn setup() -> (tempfile::TempDir, Store, Store, String) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .create_admin_account("root", "long-hash-placeholder", "generation")
            .await
            .unwrap();
        let mut tier: crate::access::Tier =
            serde_json::from_value(store.read_access_policy().unwrap()["tiers"]["free"].clone())
                .unwrap();
        tier.id = "quota_one".into();
        tier.revision = 0;
        tier.max_active_runs = 1;
        store.save_entitlement_tier(tier, 0).await.unwrap();
        let mut ids = Vec::new();
        for name in ["first-owner", "second-owner", "requester"] {
            let account = store
                .create_user_account(name, "long-hash-placeholder")
                .await
                .unwrap();
            let id = account["user_id"].as_str().unwrap().to_owned();
            store
                .update_account_access(&id, 1, crate::access::AccountStatus::Active, "quota_one")
                .await
                .unwrap();
            ids.push(id);
        }
        let one = store.for_user(&ids[0]).await.unwrap();
        let two = store.for_user(&ids[1]).await.unwrap();
        for owner in [&one, &two] {
            owner
                .put(
                    "session:s",
                    &json!({"id":"s","agent_id":"shared_default","status":"active"}),
                )
                .await
                .unwrap();
        }
        (root, one, two, ids[2].clone())
    }
    fn run(rid: &str, actor: &str) -> Value {
        json!({"id":rid,"session_id":"s","agent_id":"shared_default","requester_user_id":actor,"status":"queued","generation":"","input":[]})
    }
    #[tokio::test]
    async fn concurrent_cross_owner_reservations_enforce_requester_cap_and_retry_after_reopen() {
        let (root, one, two, actor) = setup().await;
        let a = run("first", &actor);
        let b = run("second", &actor);
        let (a_result, b_result) =
            tokio::join!(one.reserve_requester_run(&a), two.reserve_requester_run(&b));
        assert_eq!(a_result.is_ok() as u8 + b_result.is_ok() as u8, 1);
        let (winner, rid) = if a_result.is_ok() {
            (&one, "first")
        } else {
            (&two, "second")
        };
        // No canonical write: absence cannot safely release an in-flight reservation.
        let fresh = Store::open(root.path()).await.unwrap();
        let reopened = fresh.for_user(winner.current_user_id()).await.unwrap();
        reopened
            .reserve_requester_run(&run(rid, &actor))
            .await
            .unwrap();
        assert!(fresh
            .for_user(if winner.current_user_id() == one.current_user_id() {
                two.current_user_id()
            } else {
                one.current_user_id()
            })
            .await
            .unwrap()
            .reserve_requester_run(&run("third", &actor))
            .await
            .unwrap_err()
            .to_string()
            .contains("Account run limit"));
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0",
        )
        .bind(&actor)
        .fetch_one(&fresh.app_pool)
        .await
        .unwrap();
        assert_eq!(count, 1);
        let mut terminal = run(rid, &actor);
        terminal["status"] = json!("cancelled");
        reopened
            .put(&format!("run:{rid}"), &terminal)
            .await
            .unwrap();
        reopened.reconcile_requester_slots().await.unwrap();
        two.reserve_requester_run(&run("after-terminal", &actor))
            .await
            .unwrap();
    }
    #[tokio::test]
    async fn unrelated_owner_runs_do_not_consume_requester_capacity_and_legacy_runs_are_counted() {
        let (_root, one, two, actor) = setup().await;
        let owner_run = run("owner-only", one.current_user_id());
        one.reserve_requester_run(&owner_run).await.unwrap();
        one.put("run:owner-only", &owner_run).await.unwrap();
        two.reserve_requester_run(&run("participant", &actor))
            .await
            .unwrap();
        assert!(one
            .reserve_requester_run(&run("second-participant", &actor))
            .await
            .is_err());
        let mut done = run("participant", &actor);
        done["status"] = json!("cancelled");
        two.put("run:participant", &done).await.unwrap();
        two.reconcile_requester_slots().await.unwrap();
        let legacy = run("legacy", &actor);
        one.put("run:legacy", &legacy).await.unwrap();
        assert!(two
            .reserve_requester_run(&run("blocked-by-legacy", &actor))
            .await
            .is_err());
    }

    // C5 fair admission. Accounts on the fresh-install "free" level (2 active runs each).
    async fn free_accounts(store: &Store, names: &[&str]) -> Vec<Store> {
        let mut owners = Vec::new();
        for name in names {
            let account = store
                .create_user_account(name, "long-hash-placeholder")
                .await
                .unwrap();
            let id = account["user_id"].as_str().unwrap().to_owned();
            store
                .update_account_access(&id, 1, crate::access::AccountStatus::Active, "free")
                .await
                .unwrap();
            let owner = store.for_user(&id).await.unwrap();
            owner
                .put(
                    "session:s",
                    &json!({"id":"s","agent_id":"shared_default","status":"active"}),
                )
                .await
                .unwrap();
            owners.push(owner);
        }
        owners
    }
    fn run_on(rid: &str, actor: &str, agent: &str) -> Value {
        json!({"id":rid,"session_id":"s","agent_id":agent,"requester_user_id":actor,"status":"queued","generation":"","input":[]})
    }
    fn set_run_limits(store: &Store, limits: Value) {
        let mut policy = store.read_access_policy().unwrap();
        policy["run_limits"] = limits;
        policy["revision"] = json!(policy["revision"].as_u64().unwrap_or(0) + 1);
        store.write_access_policy(&policy).unwrap();
    }
    async fn opened() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        store
            .create_admin_account("root", "long-hash-placeholder", "generation")
            .await
            .unwrap();
        (root, store)
    }
    fn capacity(active: usize, ceiling: usize) -> webagent_engine::ports::Capacity {
        webagent_engine::ports::Capacity { active, ceiling }
    }

    #[tokio::test]
    async fn five_free_accounts_cannot_hold_a_sixth_users_turn() {
        let (_root, store) = opened().await;
        set_run_limits(&store, json!({"max_active_runs_per_agent":100}));
        let owners = free_accounts(&store, &["f1", "f2", "f3", "f4", "f5", "f6"]).await;
        // Account order: five accounts with two queued runs each, then the sixth with one.
        let mut queued = Vec::new();
        for (index, owner) in owners.iter().enumerate() {
            let me = owner.current_user_id().to_owned();
            for n in 0..if index < 5 { 2 } else { 1 } {
                let r = run_on(&format!("r{index}-{n}"), &me, "shared_default");
                owner.reserve_requester_run(&r).await.unwrap();
                queued.push((me.clone(), (owner.clone(), r)));
            }
        }
        // The loop's ceiling is 10 with one slot kept for administrators: 9 ordinary runs.
        let mut active = 0;
        let mut started = Vec::new();
        for (requester, (owner, r)) in webagent_engine::runtime::scheduler::fair_order(queued, None)
        {
            if owner
                .requester_claim_ready_within(&r, capacity(active, 10))
                .await
                .unwrap()
            {
                active += 1;
                started.push(requester);
            }
        }
        assert_eq!(active, 9, "the reserved administrator slot stays free");
        let sixth = owners[5].current_user_id();
        assert!(
            started.iter().take(6).any(|r| r == sixth),
            "the sixth user starts in the first round"
        );
        for owner in &owners {
            let held = started
                .iter()
                .filter(|r| *r == owner.current_user_id())
                .count();
            assert!(held <= 2);
        }
    }

    #[tokio::test]
    async fn an_agents_visitors_stay_within_its_cap_and_administrators_are_exempt() {
        let (_root, store) = opened().await;
        let visitors = free_accounts(&store, &["v1", "v2", "v3"]).await;
        let runs: Vec<Value> = visitors
            .iter()
            .enumerate()
            .map(|(i, v)| run_on(&format!("visit-{i}"), v.current_user_id(), "shared_default"))
            .collect();
        for (visitor, r) in visitors.iter().zip(&runs) {
            visitor.reserve_requester_run(r).await.unwrap();
        }
        // Ceiling 4: the default agent cap is half of it, 2.
        let ready = |i: usize| visitors[i].requester_claim_ready_within(&runs[i], capacity(0, 4));
        assert!(ready(0).await.unwrap());
        assert!(ready(1).await.unwrap());
        assert!(
            !ready(2).await.unwrap(),
            "the third visitor waits for the agent cap"
        );
        let admin_run = run_on("admin-run", store.current_user_id(), "shared_default");
        store.reserve_requester_run(&admin_run).await.unwrap();
        assert!(store
            .requester_claim_ready_within(&admin_run, capacity(0, 4))
            .await
            .unwrap());
        // The first visitor's run ends: the third one's turn comes.
        let mut done = runs[0].clone();
        done["status"] = json!("completed");
        visitors[0].put("run:visit-0", &done).await.unwrap();
        visitors[0].reconcile_requester_slots().await.unwrap();
        assert!(ready(2).await.unwrap());
    }

    #[tokio::test]
    async fn an_administrators_run_starts_with_every_ordinary_slot_full() {
        let (_root, store) = opened().await;
        let user = &free_accounts(&store, &["busy"]).await[0];
        let mine = run_on("ordinary", user.current_user_id(), "shared_default");
        user.reserve_requester_run(&mine).await.unwrap();
        let admin_run = run_on("administrator", store.current_user_id(), "shared_default");
        store.reserve_requester_run(&admin_run).await.unwrap();
        // 9 of 10 running, 1 reserved: the ordinary run waits, the administrator's starts.
        assert!(!user
            .requester_claim_ready_within(&mine, capacity(9, 10))
            .await
            .unwrap());
        assert!(store
            .requester_claim_ready_within(&admin_run, capacity(9, 10))
            .await
            .unwrap());
        assert!(user
            .requester_claim_ready_within(&mine, capacity(8, 10))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn daily_limits_refuse_clearly_and_allow_again_the_next_day() {
        let (_root, store) = opened().await;
        let user = &free_accounts(&store, &["daily"]).await[0];
        let mut free: crate::access::Tier =
            serde_json::from_value(store.read_access_policy().unwrap()["tiers"]["free"].clone())
                .unwrap();
        free.daily_turns = Some(2);
        let revision = free.revision;
        store.save_entitlement_tier(free, revision).await.unwrap();
        set_run_limits(&store, json!({"agent_daily_turns":3}));
        let pool = &store.app_pool;
        let me = user.current_user_id().to_owned();
        let access = store.live_access(&me, "shared_default").await.unwrap();
        let day = 20_000;
        use webagent_engine::usage_ledger::{record_on, refusal_on, Turn};
        assert!(refusal_on(pool, &access, day).await.unwrap().is_none());
        for _ in 0..2 {
            record_on(pool, &me, Some("shared_default"), day, Turn::default())
                .await
                .unwrap();
        }
        let refused = refusal_on(pool, &access, day).await.unwrap().unwrap();
        assert!(
            refused.contains("2 model turns a day") && refused.contains("00:00 UTC"),
            "{refused}"
        );
        assert!(refusal_on(pool, &access, day + 1).await.unwrap().is_none());
        // The agent's own limit counts every non-administrator; administrators are never limited.
        let other = &free_accounts(&store, &["daily2"]).await[0];
        let other_access = store
            .live_access(other.current_user_id(), "shared_default")
            .await
            .unwrap();
        record_on(
            pool,
            other.current_user_id(),
            Some("shared_default"),
            day,
            Turn::default(),
        )
        .await
        .unwrap();
        let agent = refusal_on(pool, &other_access, day).await.unwrap().unwrap();
        assert!(agent.contains("this agent allows 3"), "{agent}");
        let admin = store
            .live_access(store.current_user_id(), "shared_default")
            .await
            .unwrap();
        assert!(refusal_on(pool, &admin, day).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_stopped_run_gives_its_slot_back_and_waits_its_turn_when_resumed() {
        let (_root, store) = opened().await;
        let user = &free_accounts(&store, &["resume"]).await[0];
        let me = user.current_user_id().to_owned();
        let (one, two) = (
            run_on("one", &me, "shared_default"),
            run_on("two", &me, "shared_default"),
        );
        for r in [&one, &two] {
            user.reserve_requester_run(r).await.unwrap();
            user.put(&format!("run:{}", r["id"].as_str().unwrap()), r)
                .await
                .unwrap();
        }
        let third = run_on("three", &me, "shared_default");
        assert!(user
            .reserve_requester_run(&third)
            .await
            .unwrap_err()
            .to_string()
            .contains("Account run limit"));
        // Stopping "one" releases its slot, so a new message is admitted.
        let mut stopped = one.clone();
        stopped["status"] = json!("stopped");
        user.put("run:one", &stopped).await.unwrap();
        user.reconcile_requester_slots().await.unwrap();
        user.reserve_requester_run(&third).await.unwrap();
        user.put("run:three", &third).await.unwrap();
        // Resuming "one" never fails: it takes a slot again at the end of the line and waits.
        user.put("run:one", &one).await.unwrap();
        assert!(!user
            .requester_claim_ready_within(&one, capacity(0, 10))
            .await
            .unwrap());
        let held: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM requester_run_slots WHERE run_id='one' AND released=0",
        )
        .fetch_one(&store.app_pool)
        .await
        .unwrap();
        assert_eq!(held, 1);
        let mut finished = two.clone();
        finished["status"] = json!("completed");
        user.put("run:two", &finished).await.unwrap();
        user.reconcile_requester_slots().await.unwrap();
        assert!(user
            .requester_claim_ready_within(&one, capacity(0, 10))
            .await
            .unwrap());
        // Approval waits keep their slot.
        let mut waiting = third.clone();
        waiting["status"] = json!("waiting_approval");
        user.put("run:three", &waiting).await.unwrap();
        user.reconcile_requester_slots().await.unwrap();
        let kept: i64 =
            sqlx::query_scalar("SELECT released FROM requester_run_slots WHERE run_id='three'")
                .fetch_one(&store.app_pool)
                .await
                .unwrap();
        assert_eq!(kept, 0);
    }

    #[tokio::test]
    async fn commands_share_a_budget_per_run_and_per_day_and_a_count_at_once() {
        let (_root, store) = opened().await;
        set_run_limits(
            &store,
            json!({"command_cpu_seconds_per_run":10,"command_cpu_seconds_per_user_day":15,
                "concurrent_commands_per_run":1,"concurrent_commands_per_user":2}),
        );
        let user = &free_accounts(&store, &["commands"]).await[0];
        let me = user.current_user_id().to_owned();
        let access = store.live_access(&me, "shared_default").await.unwrap();
        let pool = &store.app_pool;
        let day = 20_000;
        use webagent_engine::usage_ledger::{command_refusal_on, record_command_on, CommandSlot};
        assert!(command_refusal_on(pool, &access, "r1", day)
            .await
            .unwrap()
            .is_none());
        record_command_on(pool, &me, "r1", day, 10_000)
            .await
            .unwrap();
        let run = command_refusal_on(pool, &access, "r1", day)
            .await
            .unwrap()
            .unwrap();
        assert!(run.contains("this run's commands"), "{run}");
        assert!(command_refusal_on(pool, &access, "r2", day)
            .await
            .unwrap()
            .is_none());
        record_command_on(pool, &me, "r2", day, 5_000)
            .await
            .unwrap();
        let today = command_refusal_on(pool, &access, "r3", day)
            .await
            .unwrap()
            .unwrap();
        assert!(
            today.contains("today") && today.contains("00:00 UTC"),
            "{today}"
        );
        assert!(command_refusal_on(pool, &access, "r3", day + 1)
            .await
            .unwrap()
            .is_none());
        let admin = store
            .live_access(store.current_user_id(), "shared_default")
            .await
            .unwrap();
        record_command_on(pool, store.current_user_id(), "admin-run", day, 99_000)
            .await
            .unwrap();
        assert!(command_refusal_on(pool, &admin, "admin-run", day)
            .await
            .unwrap()
            .is_none());
        // At once: one per run, two per account; a finished command frees its place.
        let first = CommandSlot::take(&access, "r9").unwrap();
        assert!(CommandSlot::take(&access, "r9")
            .unwrap_err()
            .contains("this run may run 1 command"));
        let second = CommandSlot::take(&access, "r8").unwrap();
        assert!(CommandSlot::take(&access, "r7")
            .unwrap_err()
            .contains("your account may run 2 commands"));
        drop(first);
        let third = CommandSlot::take(&access, "r7").unwrap();
        drop((second, third));
    }
}
