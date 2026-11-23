//! Periodic scans visit only accounts that have work now. Each open account database
//! holds a connection, and each connection an OS thread, so polling every account kept
//! one thread per account alive. An account has work now while a run is queued, running or
//! waiting on an external job, or a command, notice, background task, review, naming job,
//! cleanup or module input waits; stopped, paused and parked runs and finished records add
//! nothing (they change only when someone acts, which writes the account). Deadlines kept in
//! `app.db` (`account_due`: approval expiry, the automation schedule) bring an account back
//! when they pass. An account not written since it was found idle is skipped; one written
//! since stays visible to every scanner for a short window. As the fallback for anything
//! missed, every idle account is evaluated again once every 10 minutes, each at its own point
//! in the interval so the accounts never come back together; it is visited only if that finds
//! work. One result is shared by all of a process's scanners for up to a second, and
//! recomputed after any wake hint.
use super::Store;
use anyhow::{anyhow, Result};
use serde_json::Value;
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use webagent_accounts_host::AccountsStore;
use webagent_engine::due;

/// Every idle account is evaluated again once per this interval, at its own offset in it.
const RECHECK: u64 = 600;
/// A write that left no work stays visible long enough for the slowest periodic scan (10 s)
/// to see the account.
const WINDOW: Duration = Duration::from_secs(15);
/// How long a write that left no work keeps an account visible to every scanner.
const WRITTEN_WINDOW: Duration = if cfg!(test) {
    Duration::from_millis(300)
} else {
    WINDOW
};
/// How long one scan result serves every periodic scanner of the process. Root tests read
/// their own writes at once without a wake hint, so they always scan afresh.
const SHARE: Duration = if cfg!(test) {
    Duration::ZERO
} else {
    Duration::from_secs(1)
};

/// Work a periodic scan acts on now, by plain status or row presence: runs queued, running or
/// waiting on an external job, unacknowledged loop commands, unrelayed notifications, queued
/// reasoning cleanup, live or undelivered background tasks (the background index predicate
/// `background_scan` uses), host leases, queued reviews and naming jobs.
const PENDING: &str =
    "SELECT EXISTS(SELECT 1 FROM run_index WHERE status IN ('queued','running','waiting_external'))
    OR EXISTS(SELECT 1 FROM loop_commands WHERE acknowledged_at IS NULL)
    OR EXISTS(SELECT 1 FROM notification_stage)
    OR EXISTS(SELECT 1 FROM reasoning_cleanup)
    OR EXISTS(SELECT 1 FROM background_task_index WHERE status IN ('queued','running')
        OR (status='completed' AND NOT EXISTS(SELECT 1 FROM background_task_delivery d
            WHERE d.id=background_task_index.id)))
    OR EXISTS(SELECT 1 FROM docs WHERE (key>='administrator_host:' AND key<'administrator_host;')
        OR (key>='manager_pending:' AND key<'manager_pending;')
        OR (key>='session_naming:' AND key<'session_naming;'))";
/// Approval waits: work only through their expiry deadline, which the scheduler stores.
const WAITING: &str =
    "SELECT EXISTS(SELECT 1 FROM run_index WHERE status IN ('waiting_approval','parked'))";
/// Records kept after their work finishes, so their status decides.
const RECORDS: &str =
    "SELECT key,value FROM docs WHERE (key>='followup_queue:' AND key<'followup_queue;')
    OR (key>='manager_review:' AND key<'manager_review;')
    OR (key>='module_state:' AND key<'module_state;')";

/// What a kept record asks of the scan.
#[derive(Debug, PartialEq)]
enum Need {
    /// Nothing until someone writes the account.
    Nothing,
    /// Work now: visit on every scan.
    Now,
    /// Live automation schedules only: the automation service's stored next tick decides.
    Schedule,
}

/// Whether a kept record still has work: an unreviewed follow-up, an open manager review
/// (deadlines run on the clock), or a package with pending input or live schedules.
fn record_pending(key: &str, value: &Value) -> Need {
    let now = |busy: bool| if busy { Need::Now } else { Need::Nothing };
    if key.starts_with("followup_queue:") {
        now(value["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|entry| {
                !matches!(
                    entry["review_status"].as_str(),
                    Some("acceptable" | "unacceptable" | "review_error" | "cancelled")
                ) && !matches!(
                    entry["status"].as_str(),
                    Some("cancelled" | "rejected" | "delivered")
                )
            }))
    } else if key.starts_with("manager_review:") {
        now(!matches!(
            value["status"].as_str(),
            Some("acceptable" | "unacceptable" | "review_error" | "cancelled")
        ))
    } else {
        let busy = |field: &Value| {
            field.as_array().is_some_and(|v| !v.is_empty())
                || field.as_object().is_some_and(|v| !v.is_empty())
        };
        let state = &value["state"];
        let unfinished = state["occurrences"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, o)| {
                !matches!(
                    o["status"].as_str(),
                    Some("completed" | "failed" | "cancelled" | "skipped")
                )
            });
        if busy(&value["pending"]) || busy(&value["admission_results"]) || unfinished {
            return Need::Now;
        }
        let scheduled = state["definitions"]
            .as_object()
            .into_iter()
            .flatten()
            .any(|(_, d)| d["status"] == "active");
        match (scheduled, key == "module_state:automation") {
            (false, _) => Need::Nothing,
            (true, true) => Need::Schedule,
            (true, false) => Need::Now,
        }
    }
}

/// An account's write-ahead log as last seen: length and modification time, `None` when it
/// is absent or empty. Any commit changes it; a checkpoint only removes or empties it.
pub(crate) type WalMark = Option<(u64, SystemTime)>;

struct Idle {
    mark: WalMark,
    recheck_at: Instant,
    /// A write that left no work keeps the account visible until then.
    written_until: Option<Instant>,
}

enum Decision {
    Skip,
    Visit,
    /// Open the account and decide from its contents. `written`: it was written since it was
    /// last found idle (otherwise this is its first evaluation in this process).
    Evaluate {
        visit: bool,
        written: bool,
    },
}

static IDLE: LazyLock<Mutex<HashMap<String, Idle>>> = LazyLock::new(Default::default);
static SEEN: LazyLock<Mutex<HashMap<(&'static str, String), WalMark>>> =
    LazyLock::new(Default::default);

/// The account list of one state root and the `(count, max rowid)` of `user_accounts` it was
/// read at. Accounts are only ever added (`account_rows.rs` is the one writer and deletes
/// none), so an unchanged pair means an unchanged list.
struct Accounts {
    root: PathBuf,
    version: (i64, i64),
    users: Vec<String>,
}

static ACCOUNTS: LazyLock<Mutex<Option<Accounts>>> = LazyLock::new(Default::default);

/// The latest scan result of this process: its state root, the wake sequence read before it
/// was computed, when it was computed, the accounts it found and every stored deadline.
struct Shared {
    root: PathBuf,
    wake: u64,
    at: Instant,
    users: Vec<String>,
    deadlines: Vec<(String, u64)>,
}

/// Held while scanning, so concurrent scanners wait for one scan and reuse it.
static SHARED: LazyLock<tokio::sync::Mutex<Option<Shared>>> = LazyLock::new(Default::default);

fn registry_error<T>(_: T) -> anyhow::Error {
    anyhow!("Account scan registry unavailable")
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Each account's own point in the recheck interval, so accounts that went idle together
/// (a restart) are not evaluated together.
fn offset(user: &str) -> u64 {
    let hash = user.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    hash % RECHECK
}

/// The account's next recheck after `now` (Unix seconds `unix`): the next time its offset
/// comes round, at least one second away.
fn next_recheck(user: &str, now: Instant, unix: u64) -> Instant {
    let into = (unix + offset(user)) % RECHECK;
    now + Duration::from_secs(RECHECK - into)
}

fn decide(entry: Option<&mut Idle>, written: impl Fn(&WalMark) -> bool, now: Instant) -> Decision {
    match entry {
        Some(idle) if !written(&idle.mark) => {
            if idle.written_until.is_some_and(|until| now < until) {
                Decision::Visit
            } else if now < idle.recheck_at {
                Decision::Skip
            } else {
                Decision::Evaluate {
                    visit: false,
                    written: false,
                }
            }
        }
        Some(_) => Decision::Evaluate {
            visit: true,
            written: true,
        },
        None => Decision::Evaluate {
            visit: true,
            written: false,
        },
    }
}

/// Merge a scan's accounts with the deadlines that have passed by `now` (Unix seconds).
fn with_deadlines(users: &[String], deadlines: &[(String, u64)], now: u64) -> Vec<String> {
    let mut seen: HashSet<&str> = users.iter().map(String::as_str).collect();
    let mut out = users.to_vec();
    for (user, at) in deadlines {
        if *at <= now && seen.insert(user.as_str()) {
            out.push(user.clone());
        }
    }
    out
}

/// Whether the log changed from `recorded`: a new or longer log, or a later time. An absent or
/// emptied log is a checkpoint, not a write.
fn moved(recorded: &WalMark, current: &WalMark) -> bool {
    current.is_some() && current != recorded
}

impl Store {
    pub(crate) fn wal_mark(&self, user: &str) -> WalMark {
        let wal = self
            .root
            .join("user_data")
            .join(user)
            .join(format!("{user}.db-wal"));
        std::fs::metadata(wal)
            .ok()
            .filter(|m| m.len() > 0)
            .and_then(|m| Some((m.len(), m.modified().ok()?)))
    }

    /// Every account id, read again only when `user_accounts` gained a row (one counting
    /// statement instead of listing every account on each scan and slot reconcile).
    pub(crate) async fn account_ids(&self) -> Result<Vec<String>> {
        let version: (i64, i64) =
            sqlx::query_as("SELECT count(*),coalesce(max(rowid),0) FROM user_accounts")
                .fetch_one(&self.app_pool)
                .await?;
        if let Some(cached) = ACCOUNTS
            .lock()
            .map_err(registry_error)?
            .as_ref()
            .filter(|cached| cached.root == self.root && cached.version == version)
        {
            return Ok(cached.users.clone());
        }
        let users = self.all_user_ids().await?;
        *ACCOUNTS.lock().map_err(registry_error)? = Some(Accounts {
            root: self.root.clone(),
            version,
            users: users.clone(),
        });
        Ok(users)
    }

    /// What this account handle asks of a periodic scan. `scheduled`: the deadline kinds
    /// stored for it, which replace "work now" for approval waits and automation schedules.
    async fn pending_work(&self, scheduled: &HashSet<&str>) -> Result<bool> {
        if sqlx::query_scalar::<_, bool>(PENDING)
            .fetch_one(&self.pool)
            .await?
        {
            return Ok(true);
        }
        // An approval wait the scheduler has not yet seen has no stored expiry: visit it.
        if !scheduled.contains(due::APPROVAL)
            && sqlx::query_scalar::<_, bool>(WAITING)
                .fetch_one(&self.pool)
                .await?
        {
            return Ok(true);
        }
        for row in sqlx::query(RECORDS).fetch_all(&self.pool).await? {
            let key: String = row.get("key");
            match record_pending(&key, &self.unseal(&row.get::<Vec<u8>, _>("value"))?) {
                Need::Nothing => {}
                Need::Schedule if scheduled.contains(due::AUTOMATION) => {}
                Need::Now | Need::Schedule => return Ok(true),
            }
        }
        Ok(false)
    }

    /// Accounts a periodic scan must visit now, shared by every scanner of the process: a
    /// result is reused for up to [`SHARE`] unless a wake hint arrived since it was taken
    /// (message admission and run ends send one), so new work is never served stale. Stored
    /// deadlines are compared with the clock on every call, so a reused result still brings
    /// an account back the moment its deadline passes.
    pub async fn scan_user_ids(&self) -> Result<Vec<String>> {
        self.shared_scan(SHARE).await
    }

    async fn shared_scan(&self, share: Duration) -> Result<Vec<String>> {
        let mut slot = SHARED.lock().await;
        // Read before scanning: a hint during the scan makes this result stale at once.
        let wake = webagent_engine::wake::sequence();
        if let Some(shared) = slot.as_ref().filter(|shared| {
            shared.root == self.root && shared.wake == wake && shared.at.elapsed() < share
        }) {
            return Ok(with_deadlines(&shared.users, &shared.deadlines, unix_now()));
        }
        let at = Instant::now();
        let (users, deadlines) = self.scan_user_ids_now().await?;
        let result = with_deadlines(&users, &deadlines, unix_now());
        *slot = Some(Shared {
            root: self.root.clone(),
            wake,
            at,
            users,
            deadlines,
        });
        Ok(result)
    }

    /// Accounts a periodic scan must visit now, and every stored deadline. The administrator
    /// is always visited; an account is skipped while it has not been written since it was
    /// found to have no work now, until its periodic revisit or one of its deadlines.
    async fn scan_user_ids_now(&self) -> Result<(Vec<String>, Vec<(String, u64)>)> {
        let now = Instant::now();
        // Only the deadlines of work the scan's consumers do; keep-alive reads its own.
        let rows: Vec<_> = due::all(&self.app_pool)
            .await?
            .into_iter()
            .filter(|(_, kind, _)| kind == due::APPROVAL || kind == due::AUTOMATION)
            .collect();
        let mut kinds: HashMap<&str, HashSet<&str>> = HashMap::new();
        for (user, kind, _) in &rows {
            kinds
                .entry(user.as_str())
                .or_default()
                .insert(kind.as_str());
        }
        let none = HashSet::new();
        let mut visits = Vec::new();
        for user in self.account_ids().await? {
            if user == "admin" {
                visits.push(user);
                continue;
            }
            let decision = decide(
                IDLE.lock().map_err(registry_error)?.get_mut(&user),
                |mark| moved(mark, &self.wal_mark(&user)),
                now,
            );
            let visit = match decision {
                Decision::Skip => false,
                Decision::Visit => true,
                Decision::Evaluate { visit, written } => {
                    let account = self.for_user(&user).await?;
                    // Taken after opening (its schema writes are not "new") and before reading,
                    // so a write during the read moves the mark and is evaluated next time.
                    let mark = self.wal_mark(&user);
                    let scheduled = kinds.get(user.as_str()).unwrap_or(&none);
                    // An unreadable account counts as busy rather than silently skipped.
                    let pending = account.pending_work(scheduled).await.unwrap_or(true);
                    let mut idle = IDLE.lock().map_err(registry_error)?;
                    if pending {
                        idle.remove(&user);
                        true
                    } else {
                        let recheck_at = next_recheck(&user, now, unix_now());
                        idle.insert(
                            user.clone(),
                            Idle {
                                mark,
                                recheck_at,
                                // Every scanner sees a written account once, whichever
                                // of them happened to evaluate it.
                                written_until: written.then_some(now + WRITTEN_WINDOW),
                            },
                        );
                        visit
                    }
                }
            };
            if visit {
                visits.push(user);
            }
        }
        let deadlines = rows.into_iter().map(|(user, _, at)| (user, at)).collect();
        Ok((visits, deadlines))
    }

    /// Accounts written since `scan` last applied them, each with its log mark now. A caller
    /// that opens the account replaces the mark with [`Store::wal_mark`] taken after opening
    /// and before reading (opening can touch the log), then passes the list to
    /// [`Store::mark_seen`] once the contents have been applied.
    pub(crate) async fn changed_user_ids(
        &self,
        scan: &'static str,
    ) -> Result<Vec<(String, WalMark)>> {
        let users = self.account_ids().await?;
        let seen = SEEN.lock().map_err(registry_error)?;
        Ok(users
            .into_iter()
            .filter_map(|user| {
                let mark = self.wal_mark(&user);
                seen.get(&(scan, user.clone()))
                    .is_none_or(|applied| moved(applied, &mark))
                    .then_some((user, mark))
            })
            .collect())
    }

    /// Record that `scan` applied these accounts' contents as of the given log marks.
    pub(crate) fn mark_seen(scan: &'static str, applied: Vec<(String, WalMark)>) -> Result<()> {
        let mut seen = SEEN.lock().map_err(registry_error)?;
        for (user, since) in applied {
            seen.insert((scan, user), since);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn idle(mark: WalMark, recheck_at: Instant) -> Idle {
        Idle {
            mark,
            recheck_at,
            written_until: None,
        }
    }

    #[test]
    fn idle_account_is_skipped_until_written_or_revisited() {
        let now = Instant::now();
        let quiet = |_: &WalMark| false;
        assert!(matches!(
            decide(None, quiet, now),
            Decision::Evaluate {
                visit: true,
                written: false
            }
        ));
        let recheck = Duration::from_secs(RECHECK);
        let mut entry = idle(None, now + recheck);
        assert!(matches!(
            decide(Some(&mut entry), quiet, now),
            Decision::Skip
        ));
        assert!(matches!(
            decide(Some(&mut entry), |_: &WalMark| true, now),
            Decision::Evaluate {
                visit: true,
                written: true
            }
        ));
        // A write that left no work keeps the account visible for its window.
        entry.written_until = Some(now + WINDOW);
        assert!(matches!(
            decide(Some(&mut entry), quiet, now),
            Decision::Visit
        ));
        entry.written_until = None;
        // At its recheck the account is evaluated, and visited only if that finds work.
        assert!(matches!(
            decide(Some(&mut entry), quiet, now + recheck),
            Decision::Evaluate {
                visit: false,
                written: false
            }
        ));
    }

    #[test]
    fn a_checkpoint_is_not_a_write() {
        let t = SystemTime::now();
        assert!(!moved(&Some((10, t)), &None));
        assert!(!moved(&Some((10, t)), &Some((10, t))));
        assert!(moved(&Some((10, t)), &Some((20, t))));
        assert!(moved(&None, &Some((10, t))));
        assert!(moved(
            &Some((10, t)),
            &Some((10, t + Duration::from_millis(4)))
        ));
    }

    async fn rest(store: &Store, id: &str) {
        store.scan_user_ids().await.unwrap();
        tokio::time::sleep(WRITTEN_WINDOW + Duration::from_millis(50)).await;
        store.scan_user_ids().await.unwrap();
        assert!(!store
            .scan_user_ids()
            .await
            .unwrap()
            .contains(&id.to_owned()));
    }

    #[tokio::test]
    async fn idle_account_stays_closed_until_it_has_work() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let guest = store.create_guest_account().await.unwrap();
        let id = guest["user_id"].as_str().unwrap().to_owned();
        // First sight runs the real pending-work query against a new account database.
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        rest(&store, &id).await;
        let user = store.for_user(&id).await.unwrap();
        user.put("session_naming:s1", &json!({"session_id":"s1"}))
            .await
            .unwrap();
        // Written and busy: visited on every scan while the work remains.
        for _ in 0..3 {
            assert!(store.scan_user_ids().await.unwrap().contains(&id));
            tokio::time::sleep(WRITTEN_WINDOW).await;
        }
        user.delete("session_naming:s1").await.unwrap();
        // A finished background task record is not work: once its write is seen, it rests.
        user.put("background_task:t1", &json!({"status":"completed"}))
            .await
            .unwrap();
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        rest(&store, &id).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let changed = store.changed_user_ids("test_scan").await.unwrap();
        assert!(changed.iter().any(|(user, _)| *user == id));
        Store::mark_seen("test_scan", changed).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(store
            .changed_user_ids("test_scan")
            .await
            .unwrap()
            .is_empty());
        user.put("background_task:t2", &json!({"status":"queued"}))
            .await
            .unwrap();
        assert!(store
            .changed_user_ids("test_scan")
            .await
            .unwrap()
            .iter()
            .any(|(u, _)| *u == id));
    }

    async fn index_run(user: &Store, id: &str, status: &str) {
        sqlx::query("INSERT INTO run_index(id,session_id,status,updated_at) VALUES(?,'s',?,1) ON CONFLICT(id) DO UPDATE SET status=excluded.status")
            .bind(id)
            .bind(status)
            .execute(&user.pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn only_current_work_and_passed_deadlines_bring_an_account_back() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let guest = store.create_guest_account().await.unwrap();
        let id = guest["user_id"].as_str().unwrap().to_owned();
        let user = store.for_user(&id).await.unwrap();
        // Stopped, paused and stored recovery runs are nothing to do.
        for (run, status) in [("a", "stopped"), ("b", "paused"), ("c", "recovery")] {
            index_run(&user, run, status).await;
        }
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        rest(&store, &id).await;
        // A queued run is work now.
        index_run(&user, "d", "queued").await;
        for _ in 0..2 {
            assert!(store.scan_user_ids().await.unwrap().contains(&id));
            tokio::time::sleep(WRITTEN_WINDOW).await;
        }
        // Waiting for approval: work until the scheduler stores its expiry ...
        index_run(&user, "d", "waiting_approval").await;
        for _ in 0..2 {
            assert!(store.scan_user_ids().await.unwrap().contains(&id));
            tokio::time::sleep(WRITTEN_WINDOW).await;
        }
        let later = unix_now() + 3600;
        due::set(&store.app_pool, &id, due::APPROVAL, Some(later))
            .await
            .unwrap();
        index_run(&user, "d", "parked").await;
        rest(&store, &id).await;
        // ... and visited again the moment that deadline has passed.
        due::set(&store.app_pool, &id, due::APPROVAL, Some(unix_now()))
            .await
            .unwrap();
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        due::set(&store.app_pool, &id, due::APPROVAL, None)
            .await
            .unwrap();
        index_run(&user, "d", "completed").await;
        rest(&store, &id).await;
        // A live automation schedule waits for the automation service's stored next tick.
        let schedule = json!({"pending":[],"state":{"definitions":{"a":{"status":"active"}}}});
        user.put("module_state:automation", &schedule)
            .await
            .unwrap();
        for _ in 0..2 {
            assert!(store.scan_user_ids().await.unwrap().contains(&id));
            tokio::time::sleep(WRITTEN_WINDOW).await;
        }
        due::set(&store.app_pool, &id, due::AUTOMATION, Some(later))
            .await
            .unwrap();
        user.put("module_state:automation", &schedule)
            .await
            .unwrap();
        rest(&store, &id).await;
    }

    #[test]
    fn finished_records_do_not_keep_an_account_busy() {
        assert_eq!(
            record_pending("manager_review:r", &json!({"status":"acceptable"})),
            Need::Nothing
        );
        assert_eq!(
            record_pending("manager_review:r", &json!({"status":"reviewing"})),
            Need::Now
        );
        let done = json!({"entries":[{"status":"delivered"},{"review_status":"cancelled"}]});
        assert_eq!(record_pending("followup_queue:s", &done), Need::Nothing);
        assert_eq!(
            record_pending(
                "followup_queue:s",
                &json!({"entries":[{"status":"queued"}]})
            ),
            Need::Now
        );
        let quiet = json!({"pending":[],"admission_results":{},"state":{
            "definitions":{"a":{"status":"paused"}},"occurrences":{"o":{"status":"completed"}}}});
        assert_eq!(
            record_pending("module_state:automation", &quiet),
            Need::Nothing
        );
        let scheduled = json!({"pending":[],"state":{"definitions":{"a":{"status":"active"}}}});
        assert_eq!(
            record_pending("module_state:automation", &scheduled),
            Need::Schedule
        );
        assert_eq!(
            record_pending("module_state:outbound_notifications", &scheduled),
            Need::Now
        );
        assert_eq!(
            record_pending("module_state:x", &json!({"pending":[{"id":1}]})),
            Need::Now
        );
        let running = json!({"state":{"definitions":{"a":{"status":"active"}},
            "occurrences":{"o":{"status":"queued"}}}});
        assert_eq!(
            record_pending("module_state:automation", &running),
            Need::Now
        );
    }

    #[test]
    fn rechecks_are_spread_over_the_interval() {
        assert!(offset("guest_a") < RECHECK);
        assert_ne!(offset("guest_a"), offset("guest_b"));
        let now = Instant::now();
        for unix in [0, 1, 599, 600, 1_795_000_000] {
            let next = next_recheck("guest_a", now, unix);
            assert!(next > now && next <= now + Duration::from_secs(RECHECK));
            // The account's point comes round at the same place in every interval.
            let at = unix + (next - now).as_secs();
            assert_eq!((at + offset("guest_a")) % RECHECK, 0);
        }
        // 400 accounts land in many different seconds of the interval.
        let seconds: HashSet<u64> = (0..400).map(|i| offset(&format!("guest_{i}"))).collect();
        assert!(seconds.len() > 200);
    }

    #[test]
    fn passed_deadlines_join_a_shared_result() {
        let users = vec!["a".to_owned()];
        let deadlines = vec![
            ("b".to_owned(), 10),
            ("c".to_owned(), 20),
            ("a".to_owned(), 5),
        ];
        assert_eq!(with_deadlines(&users, &deadlines, 15), vec!["a", "b"]);
    }

    #[tokio::test]
    async fn the_account_list_is_read_again_only_when_an_account_is_added() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let before = store.account_ids().await.unwrap();
        assert_eq!(store.account_ids().await.unwrap(), before);
        let guest = store.create_guest_account().await.unwrap();
        let id = guest["user_id"].as_str().unwrap().to_owned();
        assert!(store.account_ids().await.unwrap().contains(&id));
    }

    #[tokio::test]
    async fn shared_scan_sees_new_work_after_a_wake_hint() {
        let share = Duration::from_secs(60);
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let guest = store.create_guest_account().await.unwrap();
        let id = guest["user_id"].as_str().unwrap().to_owned();
        assert!(store.shared_scan(share).await.unwrap().contains(&id));
        store.scan_user_ids_now().await.unwrap();
        assert!(!store.scan_user_ids_now().await.unwrap().0.contains(&id));
        // A shared result taken while the account rests.
        assert!(!store.shared_scan(share).await.unwrap().contains(&id));
        let user = store.for_user(&id).await.unwrap();
        user.put("session_naming:s1", &json!({"session_id":"s1"}))
            .await
            .unwrap();
        // Writers of new work send a hint, which makes the shared result stale at once.
        webagent_engine::wake::notify(root.path()).await;
        assert!(store.shared_scan(share).await.unwrap().contains(&id));
    }
}
