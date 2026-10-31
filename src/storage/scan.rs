//! Periodic scans visit only accounts that can have work. Each open account database
//! holds a connection, and each connection an OS thread, so polling every account kept
//! one thread per account alive. An account not written since it was found idle is
//! skipped; every idle account is still revisited every 5-10 minutes.
use super::Store;
use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use webagent_accounts_host::AccountsStore;

/// Base interval before an idle account is revisited; each account adds its own 0-5 min offset.
const RECHECK: Duration = Duration::from_secs(300);
/// A revisit stays open long enough for the slowest periodic scan (10 s) to see the account.
const WINDOW: Duration = Duration::from_secs(15);
/// File times are coarse; a write this close before a check counts as after it.
const CLOCK_MARGIN: Duration = Duration::from_millis(50);

/// Work a periodic scan acts on without a new write: unfinished runs, leases, deadlines,
/// schedules, held queues, unacknowledged loop commands and unrelayed notifications.
const PENDING: &str =
    "SELECT EXISTS(SELECT 1 FROM run_index WHERE status NOT IN ('completed','cancelled','failed'))
    OR EXISTS(SELECT 1 FROM loop_commands WHERE acknowledged_at IS NULL)
    OR EXISTS(SELECT 1 FROM notification_stage)
    OR EXISTS(SELECT 1 FROM docs WHERE (key>='background_task:' AND key<'background_task;')
        OR (key>='administrator_host:' AND key<'administrator_host;')
        OR (key>='followup_queue:' AND key<'followup_queue;')
        OR (key>='manager_review:' AND key<'manager_review;')
        OR (key>='manager_pending:' AND key<'manager_pending;')
        OR (key>='module_state:' AND key<'module_state;')
        OR (key>='session_naming:' AND key<'session_naming;'))";

struct Idle {
    since: SystemTime,
    recheck_at: Instant,
    open_until: Option<Instant>,
}

enum Decision {
    Skip,
    Visit,
    Evaluate { visit: bool },
}

static IDLE: LazyLock<Mutex<HashMap<String, Idle>>> = LazyLock::new(Default::default);
static SEEN: LazyLock<Mutex<HashMap<(&'static str, String), SystemTime>>> =
    LazyLock::new(Default::default);

fn registry_error<T>(_: T) -> anyhow::Error {
    anyhow!("Account scan registry unavailable")
}

/// Spread rechecks so accounts that went idle together are not reopened together.
fn offset(user: &str) -> Duration {
    let hash = user.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    Duration::from_secs(hash % 300)
}

fn decide(
    entry: Option<&mut Idle>,
    written: impl Fn(SystemTime) -> bool,
    now: Instant,
) -> Decision {
    match entry {
        Some(idle) if !written(idle.since) => {
            if now < idle.recheck_at {
                Decision::Skip
            } else if idle.open_until.is_none() {
                idle.open_until = Some(now + WINDOW);
                Decision::Visit
            } else if idle.open_until.is_some_and(|until| now < until) {
                Decision::Visit
            } else {
                Decision::Evaluate { visit: false }
            }
        }
        _ => Decision::Evaluate { visit: true },
    }
}

impl Store {
    /// Whether the account committed a write after `since`. Commits land in the
    /// write-ahead log; checkpoints only rewrite the main file or empty the log, so
    /// closing an idle connection does not count as a write.
    fn written_since(&self, user: &str, since: SystemTime) -> bool {
        let wal = self
            .root
            .join("user_data")
            .join(user)
            .join(format!("{user}.db-wal"));
        std::fs::metadata(wal)
            .is_ok_and(|m| m.len() > 0 && m.modified().is_ok_and(|t| t + CLOCK_MARGIN > since))
    }

    /// Accounts a periodic scan must visit now. The administrator is always visited; an
    /// account is skipped while it has not been written since it was found to hold no
    /// pending work, until its periodic revisit.
    pub async fn scan_user_ids(&self) -> Result<Vec<String>> {
        let now = Instant::now();
        let mut due = Vec::new();
        for user in self.all_user_ids().await? {
            if user == "admin" {
                due.push(user);
                continue;
            }
            let decision = decide(
                IDLE.lock().map_err(registry_error)?.get_mut(&user),
                |since| self.written_since(&user, since),
                now,
            );
            let visit = match decision {
                Decision::Skip => false,
                Decision::Visit => true,
                Decision::Evaluate { visit } => {
                    let pool = self.for_user(&user).await?.pool.clone();
                    // Taken after opening, so the open's own schema writes are not "new".
                    let since = SystemTime::now();
                    // An unreadable account counts as busy rather than silently skipped.
                    let pending = sqlx::query_scalar::<_, bool>(PENDING)
                        .fetch_one(&pool)
                        .await
                        .unwrap_or(true);
                    let mut idle = IDLE.lock().map_err(registry_error)?;
                    if pending {
                        idle.remove(&user);
                        true
                    } else {
                        let recheck_at = now + RECHECK + offset(&user);
                        idle.insert(
                            user.clone(),
                            Idle {
                                since,
                                recheck_at,
                                open_until: None,
                            },
                        );
                        visit
                    }
                }
            };
            if visit {
                due.push(user);
            }
        }
        Ok(due)
    }

    /// Accounts written since `scan` last applied them, with the time to pass to
    /// [`Store::mark_seen`] once their contents have been applied.
    pub(crate) async fn changed_user_ids(
        &self,
        scan: &'static str,
    ) -> Result<Vec<(String, SystemTime)>> {
        let users = self.all_user_ids().await?;
        // Taken before reading, so a write during the read is seen next time.
        let now = SystemTime::now();
        let seen = SEEN.lock().map_err(registry_error)?;
        Ok(users
            .into_iter()
            .filter(|user| {
                seen.get(&(scan, user.clone()))
                    .is_none_or(|since| self.written_since(user, *since))
            })
            .map(|user| (user, now))
            .collect())
    }

    /// Record that `scan` applied these accounts' contents as of the given times.
    pub(crate) fn mark_seen(scan: &'static str, applied: Vec<(String, SystemTime)>) -> Result<()> {
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

    #[test]
    fn idle_account_is_skipped_until_written_or_revisited() {
        let now = Instant::now();
        let quiet = |_| false;
        assert!(matches!(
            decide(None, quiet, now),
            Decision::Evaluate { visit: true }
        ));
        let since = SystemTime::now();
        let mut idle = Idle {
            since,
            recheck_at: now + RECHECK,
            open_until: None,
        };
        assert!(matches!(
            decide(Some(&mut idle), quiet, now),
            Decision::Skip
        ));
        assert!(matches!(
            decide(Some(&mut idle), |_| true, now),
            Decision::Evaluate { visit: true }
        ));
        // At the revisit every scan sees the account for the window, then it is re-checked.
        let later = now + RECHECK;
        assert!(matches!(
            decide(Some(&mut idle), quiet, later),
            Decision::Visit
        ));
        assert!(matches!(
            decide(Some(&mut idle), quiet, later + Duration::from_secs(5)),
            Decision::Visit
        ));
        assert!(matches!(
            decide(Some(&mut idle), quiet, later + WINDOW),
            Decision::Evaluate { visit: false }
        ));
    }

    #[tokio::test]
    async fn idle_account_stays_closed_until_it_has_work() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::open(root.path()).await.unwrap();
        let guest = store.create_guest_account().await.unwrap();
        let id = guest["user_id"].as_str().unwrap().to_owned();
        // First sight runs the real pending-work query against a new account database.
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        // Writes within the clock margin of a check count once more, then the account rests.
        tokio::time::sleep(CLOCK_MARGIN * 2).await;
        store.scan_user_ids().await.unwrap();
        assert!(!store.scan_user_ids().await.unwrap().contains(&id));
        let user = store.for_user(&id).await.unwrap();
        user.put(
            "background_task:t1",
            &serde_json::json!({"status":"queued"}),
        )
        .await
        .unwrap();
        // Written and now busy: visited on every scan while the work remains.
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        assert!(store.scan_user_ids().await.unwrap().contains(&id));
        tokio::time::sleep(CLOCK_MARGIN * 2).await;
        let changed = store.changed_user_ids("test_scan").await.unwrap();
        assert!(changed.iter().any(|(user, _)| *user == id));
        Store::mark_seen("test_scan", changed).unwrap();
        tokio::time::sleep(CLOCK_MARGIN * 2).await;
        assert!(store
            .changed_user_ids("test_scan")
            .await
            .unwrap()
            .is_empty());
        user.put(
            "background_task:t2",
            &serde_json::json!({"status":"queued"}),
        )
        .await
        .unwrap();
        assert!(store
            .changed_user_ids("test_scan")
            .await
            .unwrap()
            .iter()
            .any(|(u, _)| *u == id));
    }

    #[test]
    fn revisit_offsets_are_spread() {
        assert!(offset("guest-a") < Duration::from_secs(300));
        assert_ne!(offset("guest-a"), offset("guest-b"));
    }
}
