//! Application-wide requester slots. Absent canonical commits remain reserved.
use super::*;
/// A new person refused because the agent serves its most people of the last 30 days.
pub(crate) const AGENT_USERS_FULL: &str = "Agent user limit reached";
/// A new person refused because the agent serves its most people at once.
pub(crate) const AGENT_USERS_BUSY: &str = "Agent users at once limit reached";
/// A new run refused because the agent's owner has their plan's most runs going at once on all
/// their agents together.
pub(crate) const OWNER_RUNS_FULL: &str = "Owner run limit reached";
/// Whether `error` is a run or user limit refusing a new run: the run waits, or the sender is
/// told why; nothing failed.
pub(crate) fn limit_refusal(error: &anyhow::Error) -> bool {
    let text = error.to_string();
    text == "Account run limit reached"
        || text == AGENT_USERS_FULL
        || text == AGENT_USERS_BUSY
        || text == OWNER_RUNS_FULL
}
/// How long a person counts as one of an agent's users after their last message (seconds).
const AGENT_USER_WINDOW: i64 = 30 * 86_400;
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
        // stopped, paused or parked (taken again when it resumes). `agent_owner` (the agent's
        // owner, who pays) serves the owner's total runs at once; `paid` (the owner is on a paid
        // tier) and `first_at` the paid-tier priority: `first_at` is when the slot goes first in
        // line and is no longer held back for paid owners (when it was taken for a paid owner's
        // agent, `free_max_wait_seconds` later for any other).
        for (column, declaration) in [
            ("replaced_by", "TEXT"),
            ("agent_id", "TEXT"),
            ("admin", "INTEGER NOT NULL DEFAULT 0"),
            ("agent_owner", "TEXT"),
            ("paid", "INTEGER NOT NULL DEFAULT 0"),
            ("first_at", "INTEGER NOT NULL DEFAULT 0"),
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
        sqlx::query("CREATE INDEX IF NOT EXISTS requester_run_slots_owner ON requester_run_slots(agent_owner,released,admin)")
            .execute(&self.app_pool)
            .await?;
        // An agent's users of the last 30 days (plain ids and a time only), written by the
        // admission that counts them; rows past the window are deleted there, per agent.
        sqlx::query("CREATE TABLE IF NOT EXISTS agent_users(agent_id TEXT NOT NULL,user_id TEXT NOT NULL,last_active INTEGER NOT NULL,PRIMARY KEY(agent_id,user_id)) WITHOUT ROWID")
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
        // The agent of each unfinished run found without a slot, so the level's runs at once
        // on that agent counts it.
        let mut agents: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
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
                if let Some(agent) = run["agent_id"].as_str() {
                    agents.insert(rid.clone(), agent.to_owned());
                }
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
            // Follow-up review slots whose entry still exists; a held one whose entry was
            // pruned from its queue (C6) or whose queue is gone is released below.
            let mut entries_seen = std::collections::HashSet::new();
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
                        entries_seen.insert(slot.clone());
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
            let prefix = format!("followup-review:{owner}:");
            for slot in held.iter().filter(|slot| slot.starts_with(&prefix)) {
                if !entries_seen.contains(slot) {
                    records.push((
                        slot.clone(),
                        owner.clone(),
                        owner.clone(),
                        String::new(),
                        Slot::Finish,
                    ));
                }
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
                    let agent = agents.get(&rid).cloned();
                    sqlx::query("INSERT OR IGNORE INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence,agent_id) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots),?)").bind(rid).bind(actor).bind(owner).bind(sid).bind(agent).execute(&mut *tx).await?;
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
    /// refused on the agent past the caller's level's runs at once, its people at once, its
    /// people of the last 30 days (people already counted keep working) and the owner's total
    /// runs at once (a test account also past its own cap); a run whose slot was set aside while
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
        let admin = i64::from(access.principal.administrator());
        let now = crate::now() as i64;
        let paid = i64::from(access.owner_paid());
        let first_at = if access.owner_paid() {
            now
        } else {
            now + i64::from(access.run_limits.free_max_wait_seconds)
        };
        let mut owner_alert = None;
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
                sqlx::query("UPDATE requester_run_slots SET released=0,sequence=(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots),first_at=? WHERE run_id=?")
                    .bind(first_at)
                    .bind(rid)
                    .execute(&mut *tx)
                    .await?;
            }
            // A slot from before the owner was recorded takes its class now.
            sqlx::query("UPDATE requester_run_slots SET agent_id=?,admin=?,agent_owner=?,paid=?,first_at=CASE WHEN agent_owner IS NULL THEN ? ELSE first_at END WHERE run_id=?")
                .bind(agent)
                .bind(admin)
                .bind(&access.agent_owner_id)
                .bind(paid)
                .bind(first_at)
                .bind(rid)
                .execute(&mut *tx)
                .await?;
        } else {
            if let Some(limit) = access.account_run_limit() {
                let count: i64 = sqlx::query_scalar(
                    "SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND released=0",
                )
                .bind(actor)
                .fetch_one(&mut *tx)
                .await?;
                if count >= i64::from(limit) {
                    return Err(anyhow!("Account run limit reached"));
                }
            }
            agent_admission(&mut tx, &access, actor).await?;
            if let Some(runs) = access.owner_totals().runs_at_once {
                let going: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE agent_owner=? AND released=0 AND admin=0")
                    .bind(&access.agent_owner_id)
                    .fetch_one(&mut *tx)
                    .await?;
                if going >= i64::from(runs) {
                    drop(tx);
                    self.owner_runs_alert(&access, 100, runs).await;
                    return Err(anyhow!(OWNER_RUNS_FULL));
                }
                let before = u64::try_from(going).unwrap_or(0);
                owner_alert =
                    webagent_engine::usage_ledger::crossed(before, before + 1, u64::from(runs))
                        .into_iter()
                        .next()
                        .map(|level| (level, runs));
            }
            sqlx::query("INSERT INTO requester_run_slots(run_id,requester_id,owner_id,session_id,sequence,agent_id,admin,agent_owner,paid,first_at) VALUES(?,?,?,?,(SELECT COALESCE(MAX(sequence),0)+1 FROM requester_run_slots),?,?,?,?,?)").bind(rid).bind(actor).bind(self.current_user_id()).bind(sid).bind(agent).bind(admin).bind(&access.agent_owner_id).bind(paid).bind(first_at).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        if let Some((level, runs)) = owner_alert {
            self.owner_runs_alert(&access, level, runs).await;
        }
        Ok(access)
    }

    /// Raise the owner's runs-at-once alert (a notice for them, once a day per level). A failed
    /// write is logged: the alert is a courtesy, the limit itself already held.
    async fn owner_runs_alert(
        &self,
        access: &access::authority::LiveAccess,
        level: u32,
        runs: u32,
    ) {
        let day = webagent_engine::usage_ledger::day(crate::now());
        if let Err(error) = webagent_engine::usage_ledger::raise_owner_alert(
            &self.app_pool,
            &access.agent_owner_id,
            webagent_engine::usage_ledger::ALERT_RUNS,
            day,
            level,
            runs,
        )
        .await
        {
            tracing::warn!("Owner alert not raised: {error}");
        }
    }

    /// Whether `run` may start now (C5). In one writer transaction on the slot table: the
    /// requester's place among their held slots on the agent is within their level's runs at
    /// once there (a test account's also within its cap); for a non-administrator, its place
    /// among the owner's held slots is within the owner's total, among the agent's held
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
        if let Some(limit) = access.account_run_limit() {
            let rank: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=(SELECT requester_id FROM requester_run_slots WHERE run_id=?) AND released=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
                .bind(rid)
                .bind(rid)
                .fetch_one(&mut *tx)
                .await?;
            if rank > i64::from(limit) {
                return Ok(false);
            }
        }
        // The caller's level's runs at once on this agent, in the same line order.
        if let Some(runs) = access.agent_limits.runs_at_once {
            let place: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=(SELECT requester_id FROM requester_run_slots WHERE run_id=?) AND agent_id=? AND released=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
                .bind(rid)
                .bind(&access.agent_id)
                .bind(rid)
                .fetch_one(&mut *tx)
                .await?;
            if place > i64::from(runs) {
                return Ok(false);
            }
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
            // The owner's runs at once on all their agents, in the same line order.
            if let Some(runs) = access.owner_totals().runs_at_once {
                let place: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE agent_owner=? AND released=0 AND admin=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
                    .bind(&access.agent_owner_id)
                    .bind(rid)
                    .fetch_one(&mut *tx)
                    .await?;
                if place > i64::from(runs) {
                    return Ok(false);
                }
            }
            // Paid-tier priority (spec 8.3): Free runs (agents of owners not on a paid tier)
            // together hold at most `free_cap` slots; a Free run waits for a place among them
            // until its longest wait has passed (slowed, never starved). Started runs are never
            // stopped, and with no paid reserve nothing is held back.
            if let Some(cap) = limits
                .free_cap(capacity.ceiling)
                .filter(|_| !access.owner_paid())
            {
                let held_back: Option<i64> = sqlx::query_scalar(
                    "SELECT 1 FROM requester_run_slots WHERE run_id=? AND first_at>?",
                )
                .bind(rid)
                .bind(crate::now() as i64)
                .fetch_optional(&mut *tx)
                .await?;
                if held_back.is_some() {
                    let place: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM requester_run_slots WHERE released=0 AND admin=0 AND paid=0 AND sequence <= (SELECT sequence FROM requester_run_slots WHERE run_id=?)")
                        .bind(rid)
                        .fetch_one(&mut *tx)
                        .await?;
                    if place > i64::try_from(cap).unwrap_or(i64::MAX) {
                        return Ok(false);
                    }
                }
            }
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// The agent's limits for a new run of `actor`, inside the slot reservation's transaction:
/// their level's runs at once there, then (for a person with no run going there) its people
/// at once, then (for a person not seen there in 30 days) its people of the last 30 days,
/// whom this run then counts. Operators and administrators have no such limits.
async fn agent_admission(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    access: &access::authority::LiveAccess,
    actor: &str,
) -> Result<()> {
    let limits = access.agent_limits;
    if limits == access::authority::AgentLimits::default() {
        return Ok(());
    }
    let agent = access.agent_id.as_str();
    let mine: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM requester_run_slots WHERE requester_id=? AND agent_id=? AND released=0",
    )
    .bind(actor)
    .bind(agent)
    .fetch_one(&mut **tx)
    .await?;
    if limits
        .runs_at_once
        .is_some_and(|runs| mine >= i64::from(runs))
    {
        return Err(anyhow!("Account run limit reached"));
    }
    if let (0, Some(at_once)) = (mine, limits.users_at_once) {
        let others: i64 = sqlx::query_scalar("SELECT COUNT(DISTINCT requester_id) FROM requester_run_slots WHERE agent_id=? AND released=0 AND admin=0 AND requester_id<>? AND requester_id<>?")
            .bind(agent)
            .bind(actor)
            .bind(&access.agent_owner_id)
            .fetch_one(&mut **tx)
            .await?;
        if others >= i64::from(at_once) {
            return Err(anyhow!(AGENT_USERS_BUSY));
        }
    }
    let now = crate::now() as i64;
    sqlx::query("DELETE FROM agent_users WHERE agent_id=? AND last_active<?")
        .bind(agent)
        .bind(now - AGENT_USER_WINDOW)
        .execute(&mut **tx)
        .await?;
    if let Some(users) = limits.users {
        let known: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM agent_users WHERE agent_id=? AND user_id=?")
                .bind(agent)
                .bind(actor)
                .fetch_optional(&mut **tx)
                .await?;
        if known.is_none() {
            let count: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM agent_users WHERE agent_id=?")
                    .bind(agent)
                    .fetch_one(&mut **tx)
                    .await?;
            if count >= i64::from(users) {
                return Err(anyhow!(AGENT_USERS_FULL));
            }
        }
    }
    sqlx::query("INSERT INTO agent_users(agent_id,user_id,last_active) VALUES(?,?,?) ON CONFLICT(agent_id,user_id) DO UPDATE SET last_active=excluded.last_active")
        .bind(agent)
        .bind(actor)
        .bind(now)
        .execute(&mut **tx)
        .await?;
    Ok(())
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
        // One run at once on the default agent for its members and visitors: the limit these
        // cross-owner races must hold (a person's own tier no longer limits it, 11-23 2:40 PM).
        level_runs(&store, "shared_default", 1).await;
        let mut ids = Vec::new();
        for name in ["first-owner", "second-owner", "requester"] {
            let account = store
                .create_user_account(name, "long-hash-placeholder")
                .await
                .unwrap();
            let id = account["user_id"].as_str().unwrap().to_owned();
            store
                .update_account_access(&id, 1, crate::access::AccountStatus::Active, "free")
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
    /// Set the member and visitor levels' runs at once on `agent`.
    async fn level_runs(store: &Store, agent: &str, runs: u32) {
        for id in ["member", "visitor"] {
            let levels = store.capability_levels(agent).await.unwrap();
            let mut level = levels.levels[id].clone();
            level.runs_at_once = Some(runs);
            store
                .save_capability_level(agent, level, levels.revision)
                .await
                .unwrap();
        }
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

    /// An agent `owner` owns, whose Member level has `runs` at once, with the owner's own
    /// `member_access` limits.
    async fn owned_agent(
        store: &Store,
        owner: &Store,
        runs: Option<u32>,
        member_access: Value,
    ) -> String {
        let agent = format!("agent-{}", owner.current_user_id().to_lowercase());
        let mut levels = crate::access::capabilities::CapabilityLevels::default();
        levels.revision = 1;
        if let Some(level) = levels.levels.get_mut("member") {
            level.runs_at_once = runs;
        }
        let metadata = json!({"owner_id":owner.current_user_id(),"member_access":member_access,
            "capability_levels":levels});
        let pool = crate::agent_storage::open_agent_pool(&store.root, &agent)
            .await
            .unwrap();
        sqlx::query("INSERT INTO agents(id,name,description,provider,model,status,metadata,revision,created_at,updated_at) VALUES(?,'Owned','','openai_subscription','fixture','active',?,1,0,0)")
            .bind(&agent)
            .bind(store.seal(&metadata).unwrap())
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
        store
            .register_agent(&agent, owner.current_user_id())
            .await
            .unwrap();
        agent
    }
    async fn join(store: &Store, users: &[Store], agent: &str) {
        for user in users {
            store
                .set_agent_membership(
                    user.current_user_id(),
                    agent,
                    crate::access::Relationship::Member,
                    0,
                )
                .await
                .unwrap();
        }
    }

    #[tokio::test]
    async fn a_level_runs_at_once_on_an_agent_refuses_a_new_run_and_queues_a_resumed_one() {
        let (_root, store) = opened().await;
        let accounts = free_accounts(&store, &["owner", "member"]).await;
        let (owner, member) = (&accounts[0], &accounts[1]);
        let agent = owned_agent(&store, owner, Some(1), json!({})).await;
        join(&store, std::slice::from_ref(member), &agent).await;
        let me = member.current_user_id().to_owned();
        let first = run_on("level-1", &me, &agent);
        member.reserve_requester_run(&first).await.unwrap();
        // Refused: a second run of this member on this agent at once.
        let refused = member
            .reserve_requester_run(&run_on("level-2", &me, &agent))
            .await;
        assert!(refused.is_err_and(|e| limit_refusal(&e)));
        // Allowed: the same member elsewhere, and the owner (an operator) on the agent.
        member
            .reserve_requester_run(&run_on("elsewhere", &me, "shared_default"))
            .await
            .unwrap();
        let mine = owner.current_user_id().to_owned();
        owner
            .reserve_requester_run(&run_on("owner-1", &mine, &agent))
            .await
            .unwrap();
        owner
            .reserve_requester_run(&run_on("owner-2", &mine, &agent))
            .await
            .unwrap();
        assert!(member
            .requester_claim_ready_within(&first, capacity(0, 10))
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn an_agent_takes_new_people_up_to_its_limits_and_keeps_those_it_has() {
        let (_root, store) = opened().await;
        let names = ["owner", "p1", "p2", "p3", "p4"];
        let accounts = free_accounts(&store, &names).await;
        // The owner allows 2 people at once and 3 in 30 days (a fresh install's Free has none).
        let agent = owned_agent(
            &store,
            &accounts[0],
            None,
            json!({"max_users":3,"max_users_at_once":2}),
        )
        .await;
        join(&store, &accounts[1..], &agent).await;
        let id = |i: usize| accounts[i].current_user_id().to_owned();
        let p1 = run_on("p1-a", &id(1), &agent);
        accounts[1].reserve_requester_run(&p1).await.unwrap();
        accounts[2]
            .reserve_requester_run(&run_on("p2-a", &id(2), &agent))
            .await
            .unwrap();
        // Refused: a third person at once (the owner's 2), with a clear reason.
        let busy = accounts[3]
            .reserve_requester_run(&run_on("p3-a", &id(3), &agent))
            .await;
        assert_eq!(busy.unwrap_err().to_string(), AGENT_USERS_BUSY);
        // Allowed: a person already served adds a run (Free's tier allows 2 runs at once).
        accounts[1]
            .reserve_requester_run(&run_on("p1-b", &id(1), &agent))
            .await
            .unwrap();
        // p1 finishes everything: p3 gets in as the third person of the last 30 days.
        for rid in ["p1-a", "p1-b"] {
            let mut done = run_on(rid, &id(1), &agent);
            done["status"] = json!("completed");
            accounts[1].put(&format!("run:{rid}"), &done).await.unwrap();
        }
        accounts[1].reconcile_requester_slots().await.unwrap();
        accounts[3]
            .reserve_requester_run(&run_on("p3-b", &id(3), &agent))
            .await
            .unwrap();
        // Refused: a fourth person in 30 days, though someone is free at once.
        for rid in ["p2-a", "p3-b"] {
            let i = if rid == "p2-a" { 2 } else { 3 };
            let mut done = run_on(rid, &id(i), &agent);
            done["status"] = json!("completed");
            accounts[i].put(&format!("run:{rid}"), &done).await.unwrap();
            accounts[i].reconcile_requester_slots().await.unwrap();
        }
        let full = accounts[4]
            .reserve_requester_run(&run_on("p4-a", &id(4), &agent))
            .await;
        assert_eq!(full.unwrap_err().to_string(), AGENT_USERS_FULL);
        // Allowed: people already counted keep working; the administrator is never counted.
        accounts[1]
            .reserve_requester_run(&run_on("p1-c", &id(1), &agent))
            .await
            .unwrap();
        let admin = store.current_user_id().to_owned();
        store
            .reserve_requester_run(&run_on("admin-a", &admin, &agent))
            .await
            .unwrap();
        // A person last seen over 30 days ago leaves the count: p4 gets in.
        sqlx::query("UPDATE agent_users SET last_active=last_active-31*86400 WHERE user_id=?")
            .bind(id(2))
            .execute(&store.app_pool)
            .await
            .unwrap();
        accounts[4]
            .reserve_requester_run(&run_on("p4-b", &id(4), &agent))
            .await
            .unwrap();
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
        level_runs(&store, "shared_default", 2).await;
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
        record_command_on(pool, &me, "r1", None, day, 10_000)
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
        record_command_on(pool, &me, "r2", None, day, 5_000)
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
        record_command_on(
            pool,
            store.current_user_id(),
            "admin-run",
            None,
            day,
            99_000,
        )
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

    #[tokio::test]
    async fn a_follow_up_review_slot_is_released_once_its_entry_is_pruned() {
        let (_root, store) = opened().await;
        let user = &free_accounts(&store, &["followups"]).await[0];
        let me = user.current_user_id().to_owned();
        let slot = format!("followup-review:{me}:s:e1");
        let queue = |entries: Value| json!({"schema_version":1,"session_id":"s","revision":0,"held":false,"entries":entries});
        user.put(
            "followup_queue:s",
            &queue(json!([{"id":"e1","status":"pending","review_status":"reviewing"}])),
        )
        .await
        .unwrap();
        user.reconcile_requester_slots().await.unwrap();
        let released = || async {
            sqlx::query_scalar::<_, i64>("SELECT released FROM requester_run_slots WHERE run_id=?")
                .bind(&slot)
                .fetch_one(&store.app_pool)
                .await
                .unwrap()
        };
        assert_eq!(released().await, 0, "a reviewing entry holds its slot");
        // The entry is pruned from its queue (C6): the slot is released, not leaked.
        user.put("followup_queue:s", &queue(json!([])))
            .await
            .unwrap();
        user.reconcile_requester_slots().await.unwrap();
        assert_eq!(released().await, 1);
    }

    /// Give the fixture policy a Plus tier above Free and set owner limits on both.
    fn owner_tiers(store: &Store, free: Value, plus: Value) {
        let mut policy = store.read_access_policy().unwrap();
        let mut plus_tier = policy["tiers"]["free"].clone();
        plus_tier["id"] = json!("plus");
        plus_tier["allowed_models"] = json!(["*"]);
        plus_tier["max_active_runs"] = json!(9);
        plus_tier["owner_limits"] = plus;
        policy["tiers"]["plus"] = plus_tier;
        policy["tiers"]["free"]["owner_limits"] = free;
        let mut order: Vec<Value> = policy["tier_order"].as_array().cloned().unwrap_or_default();
        order.retain(|tier| tier != "plus");
        order.insert(1, json!("plus"));
        policy["tier_order"] = json!(order);
        policy["revision"] = json!(policy["revision"].as_u64().unwrap_or(0) + 1);
        store.write_access_policy(&policy).unwrap();
    }

    async fn set_tier(store: &Store, user: &Store, tier: &str) {
        store
            .update_account_access(
                user.current_user_id(),
                store
                    .account_by_id(user.current_user_id())
                    .await
                    .unwrap()
                    .unwrap()["access_revision"]
                    .as_u64()
                    .unwrap(),
                crate::access::AccountStatus::Active,
                tier,
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn an_owners_runs_at_once_are_shared_by_everyone_on_their_agents() {
        let (_root, store) = opened().await;
        owner_tiers(&store, json!({"runs_at_once":2}), json!({"runs_at_once":9}));
        let accounts = free_accounts(&store, &["owner", "p1", "p2", "p3"]).await;
        let agent = owned_agent(&store, &accounts[0], None, json!({})).await;
        join(&store, &accounts[1..], &agent).await;
        let id = |i: usize| accounts[i].current_user_id().to_owned();
        let one = run_on("o-1", &id(1), &agent);
        accounts[1].reserve_requester_run(&one).await.unwrap();
        accounts[2]
            .reserve_requester_run(&run_on("o-2", &id(2), &agent))
            .await
            .unwrap();
        // Refused: a third person's run, past the Free owner's 2 at once, with a clear reason.
        let full = accounts[3]
            .reserve_requester_run(&run_on("o-3", &id(3), &agent))
            .await
            .unwrap_err();
        assert_eq!(full.to_string(), OWNER_RUNS_FULL);
        assert!(limit_refusal(&full));
        // Allowed: an administrator is neither limited nor counted.
        let admin_run = run_on("o-admin", store.current_user_id(), &agent);
        store.reserve_requester_run(&admin_run).await.unwrap();
        assert!(store
            .requester_claim_ready_within(&admin_run, capacity(0, 10))
            .await
            .unwrap());
        // A finished run frees a place for the next person.
        sqlx::query("UPDATE requester_run_slots SET released=1 WHERE run_id='o-1'")
            .execute(&store.app_pool)
            .await
            .unwrap();
        let three = run_on("o-3", &id(3), &agent);
        accounts[3].reserve_requester_run(&three).await.unwrap();
        assert!(accounts[3]
            .requester_claim_ready_within(&three, capacity(0, 10))
            .await
            .unwrap());
        // The owner moved to Plus from the administrator's sign-in: 9 at once, so a fourth
        // run is allowed beside the two going.
        assert!(accounts[1]
            .reserve_requester_run(&run_on("o-4", &id(1), &agent))
            .await
            .is_err_and(|error| error.to_string() == OWNER_RUNS_FULL));
        set_tier(&store, &accounts[0], "plus").await;
        accounts[1]
            .reserve_requester_run(&run_on("o-4", &id(1), &agent))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn paid_owners_runs_go_first_and_free_holds_at_most_the_rest_until_its_wait_ends() {
        let (_root, store) = opened().await;
        owner_tiers(&store, json!({}), json!({}));
        set_run_limits(
            &store,
            json!({"max_active_runs_per_agent":100,"plus_reserved_runs":5,"free_max_wait_seconds":3600}),
        );
        let accounts = free_accounts(&store, &["paid", "f1", "f2", "f3", "f4", "f5", "pm"]).await;
        set_tier(&store, &accounts[0], "plus").await;
        let paid_agent = owned_agent(&store, &accounts[0], None, json!({})).await;
        join(&store, &accounts[6..], &paid_agent).await;
        // Five Free runs (the default agent's owner is the administrator, not a paid tier).
        let mut free_runs = Vec::new();
        for (index, account) in accounts[1..6].iter().enumerate() {
            let run = run_on(
                &format!("free-{index}"),
                account.current_user_id(),
                "shared_default",
            );
            account.reserve_requester_run(&run).await.unwrap();
            free_runs.push((account, run));
        }
        let paid = run_on("paid-1", accounts[6].current_user_id(), &paid_agent);
        accounts[6].reserve_requester_run(&paid).await.unwrap();
        // Ceiling 10, one kept for administrators, 5 for paid owners: Free holds at most 4.
        for (account, run) in &free_runs[..4] {
            assert!(account
                .requester_claim_ready_within(run, capacity(0, 10))
                .await
                .unwrap());
        }
        let (fifth, fifth_run) = &free_runs[4];
        assert!(!fifth
            .requester_claim_ready_within(fifth_run, capacity(0, 10))
            .await
            .unwrap());
        // The paid owner's run is not held back, and it goes first in the scheduler's line.
        assert!(accounts[6]
            .requester_claim_ready_within(&paid, capacity(0, 10))
            .await
            .unwrap());
        let ids = ["free-0", "free-4", "paid-1"];
        let first = webagent_engine::runtime::scheduler::first_in_line(&store.app_pool, &ids).await;
        assert_eq!(
            first,
            std::collections::HashSet::from(["paid-1".to_owned()])
        );
        // Past its longest wait the Free run goes first too and is no longer held back.
        sqlx::query("UPDATE requester_run_slots SET first_at=0 WHERE run_id='free-4'")
            .execute(&store.app_pool)
            .await
            .unwrap();
        assert!(fifth
            .requester_claim_ready_within(fifth_run, capacity(0, 10))
            .await
            .unwrap());
        let first = webagent_engine::runtime::scheduler::first_in_line(&store.app_pool, &ids).await;
        assert!(first.contains("free-4") && !first.contains("free-0"));
    }

    #[tokio::test]
    async fn an_owners_daily_compute_is_shared_by_everyone_on_their_agents() {
        let (_root, store) = opened().await;
        owner_tiers(&store, json!({"compute_seconds_per_day":2}), json!({}));
        let accounts = free_accounts(&store, &["owner", "p1", "p2"]).await;
        let agent = owned_agent(&store, &accounts[0], None, json!({})).await;
        join(&store, &accounts[1..], &agent).await;
        let pool = &store.app_pool;
        let day = 20_000;
        use webagent_engine::usage_ledger::{command_refusal_on, record_command_on};
        let p1 = store
            .live_access(accounts[1].current_user_id(), &agent)
            .await
            .unwrap();
        let p2 = store
            .live_access(accounts[2].current_user_id(), &agent)
            .await
            .unwrap();
        assert!(command_refusal_on(pool, &p2, "c2", day)
            .await
            .unwrap()
            .is_none());
        // p1's commands use up the owner's two seconds; p2 is refused on the same agent.
        record_command_on(
            pool,
            &p1.principal.user_id,
            "c1",
            Some(&p1.agent_owner_id),
            day,
            2_000,
        )
        .await
        .unwrap();
        let refused = command_refusal_on(pool, &p2, "c2", day)
            .await
            .unwrap()
            .unwrap();
        assert!(refused.starts_with("Compute limit reached"), "{refused}");
        // Allowed: the next day, the default agent (another owner) and an administrator.
        assert!(command_refusal_on(pool, &p2, "c2", day + 1)
            .await
            .unwrap()
            .is_none());
        let elsewhere = store
            .live_access(accounts[2].current_user_id(), "shared_default")
            .await
            .unwrap();
        assert!(command_refusal_on(pool, &elsewhere, "c3", day)
            .await
            .unwrap()
            .is_none());
        let admin = store
            .live_access(store.current_user_id(), &agent)
            .await
            .unwrap();
        assert!(command_refusal_on(pool, &admin, "c4", day)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_persons_own_tier_no_longer_limits_their_runs_on_someone_elses_agent() {
        // User decision 11-23 2:40 PM: the agent's settings and its owner's tier decide.
        let (_root, store) = opened().await;
        owner_tiers(&store, json!({}), json!({"runs_at_once":9}));
        let accounts = free_accounts(&store, &["plus-owner", "free-person"]).await;
        set_tier(&store, &accounts[0], "plus").await;
        let agent = owned_agent(&store, &accounts[0], Some(4), json!({})).await;
        join(&store, &accounts[1..], &agent).await;
        let me = accounts[1].current_user_id().to_owned();
        // Allowed: four runs at once (the member level's 4), past Free's own 2.
        let mut runs = Vec::new();
        for n in 0..4 {
            let run = run_on(&format!("lv-{n}"), &me, &agent);
            accounts[1].reserve_requester_run(&run).await.unwrap();
            runs.push(run);
        }
        assert!(accounts[1]
            .requester_claim_ready_within(&runs[3], capacity(3, 10))
            .await
            .unwrap());
        // Refused: a fifth (the level's limit) and a start with the server's slots full.
        let fifth = accounts[1]
            .reserve_requester_run(&run_on("lv-4", &me, &agent))
            .await
            .unwrap_err();
        assert!(limit_refusal(&fifth));
        assert!(!accounts[1]
            .requester_claim_ready_within(&runs[0], capacity(9, 10))
            .await
            .unwrap());
    }
}
