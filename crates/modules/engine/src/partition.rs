//! Which agent-loop process serves which account when several loops run (multi-loop).
//!
//! Each loop process has an index `0..loops` and serves only the accounts that hash to it: it
//! scans, recovers, schedules and ticks those accounts and no others. The administrator
//! account is always served by loop 0, together with the once-only duties (board notes, local
//! model warm-up). With one loop every account is owned, exactly as before. The API process
//! has no partition and keeps its views of every account.
//!
//! The assignment is a pure function of the account id and the loop count, so every process
//! (API and loops, any release) computes the same answer without stored state; the member
//! fence in `app.db` keeps two different counts from running at once.
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};

/// Most loop processes one installation may run.
pub const MAX_LOOPS: u32 = 16;
/// The administrator's account id; always served by loop 0.
const ADMINISTRATOR: &str = "admin";

/// One loop process's place: its index and the number of loops it was started with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub index: u32,
    pub loops: u32,
}

impl Default for Member {
    fn default() -> Self {
        Self { index: 0, loops: 1 }
    }
}

impl Member {
    /// A member from its index and loop count; `None` unless `index < loops <= MAX_LOOPS`.
    pub fn new(index: u32, loops: u32) -> Option<Self> {
        (loops >= 1 && loops <= MAX_LOOPS && index < loops).then_some(Self { index, loops })
    }
    /// Read `V3_LOOP_INDEX` and `V3_LOOPS` (both default: one loop, index 0).
    pub fn from_environment() -> anyhow::Result<Self> {
        fn number(name: &str, default: u32) -> anyhow::Result<u32> {
            match std::env::var(name) {
                Ok(value) => value
                    .trim()
                    .parse()
                    .map_err(|_| anyhow::anyhow!("{name} is not a number")),
                Err(_) => Ok(default),
            }
        }
        let index = number("V3_LOOP_INDEX", 0)?;
        let loops = number("V3_LOOPS", 1)?;
        Self::new(index, loops)
            .ok_or_else(|| anyhow::anyhow!("Loop index must be below the loop count (1 to 16)"))
    }
}

/// The loop that serves `account` when `loops` loops run: the first eight bytes of the
/// SHA-256 of the id, big-endian, modulo the count; the administrator is always loop 0.
/// Stable across processes and releases (never Rust's randomly keyed hasher).
pub fn loop_for(account: &str, loops: u32) -> u32 {
    if loops <= 1 || account == ADMINISTRATOR {
        return 0;
    }
    let digest = Sha256::digest(account.as_bytes());
    let mut head = [0u8; 8];
    head.copy_from_slice(&digest[..8]);
    // The remainder is below `loops`, which is a u32.
    (u64::from_be_bytes(head) % u64::from(loops)) as u32
}

fn own_slot() -> &'static OnceLock<Member> {
    static OWN: OnceLock<Member> = OnceLock::new();
    &OWN
}

/// A loop member row's state: written at the start-up fence (`Starting`, before start-up
/// recovery) and by the scheduler's heartbeat (`Serving`). Only serving rows make readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberState {
    Starting,
    Serving,
}

/// Make this process loop `member` (once, at start, before any scan). Returns false when a
/// different member was already set.
pub fn join(member: Member) -> bool {
    set_routes(member.loops);
    own_slot().get_or_init(|| member) == &member
}

/// This process's loop place; `None` in the API process (and in tests that never join).
pub fn own() -> Option<Member> {
    own_slot().get().copied()
}

/// Whether this process serves `account`'s background work: always outside a loop and with
/// one loop. A debug build may switch the filter off for the two-loop safety test.
pub fn owns(account: &str) -> bool {
    match own() {
        Some(member) if member.loops > 1 => {
            filter_off() || loop_for(account, member.loops) == member.index
        }
        _ => true,
    }
}

/// Keep only the accounts this process serves.
pub fn retain_owned(accounts: &mut Vec<String>) {
    if own().is_some_and(|member| member.loops > 1) {
        accounts.retain(|account| owns(account));
    }
}

/// Test hook: `V3_TEST_LOOP_FILTER_OFF=1` lets two loops visit the same accounts, to prove the
/// claim, lease and recovery fences. Only debug builds read it; release images ignore it.
fn filter_off() -> bool {
    #[cfg(debug_assertions)]
    {
        static OFF: OnceLock<bool> = OnceLock::new();
        *OFF.get_or_init(|| {
            std::env::var_os("V3_TEST_LOOP_FILTER_OFF").is_some_and(|value| value == "1")
        })
    }
    #[cfg(not(debug_assertions))]
    {
        false
    }
}

/// How many loops wake hints are routed over (0 means not yet known: one).
static ROUTES: AtomicU32 = AtomicU32::new(0);

/// The loop count hints are routed over: this loop's own count, or in the API the count the
/// live loop members last reported (one until then).
pub fn routes() -> u32 {
    ROUTES.load(Ordering::Acquire).clamp(1, MAX_LOOPS)
}

/// Route hints over `loops` loops (a loop's own count; the API's readiness check passes the
/// count its live members report). Out-of-range values are ignored.
pub fn set_routes(loops: u32) {
    if (1..=MAX_LOOPS).contains(&loops) {
        ROUTES.store(loops, Ordering::Release);
    }
}

/// This loop process's lease owner and member row owner: a fresh id per process, recorded by
/// the member fence at start and used by the scheduler for every claim.
pub fn member_owner() -> Option<String> {
    owner_slot().lock().ok().and_then(|owner| owner.clone())
}

/// Record the owner id the member fence admitted (once, at start).
pub fn set_member_owner(owner: &str) {
    if let Ok(mut slot) = owner_slot().lock() {
        *slot = Some(owner.to_owned());
    }
}

fn owner_slot() -> &'static Mutex<Option<String>> {
    static OWNER: Mutex<Option<String>> = Mutex::new(None);
    &OWNER
}

/// A file in the control directory that each loop keeps for itself: loop 0 keeps today's
/// name (`loop.lock`), loop k adds `-k` before the extension (`loop-2.lock`).
pub fn indexed_name(name: &str, index: u32) -> String {
    if index == 0 {
        return name.to_owned();
    }
    match name.rsplit_once('.') {
        Some((stem, extension)) => format!("{stem}-{index}.{extension}"),
        None => format!("{name}-{index}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_assignment_is_fixed_and_puts_the_administrator_on_loop_zero() {
        // Fixed vectors: the API, every loop and every later release must agree on these.
        assert_eq!(loop_for("admin", 2), 0);
        assert_eq!(loop_for("admin", 7), 0);
        for loops in 0..=MAX_LOOPS {
            assert!(loop_for("guest_ab12", loops) < loops.max(1));
        }
        assert_eq!(loop_for("anything", 1), 0);
        let vectors = [
            ("guest_ab12", 2, 1),
            ("guest_ab12", 3, 0),
            ("guest_ab12", 4, 1),
            ("user_one", 2, 0),
            ("user_one", 3, 2),
            ("user_one", 4, 2),
            ("plus_test_3", 2, 0),
            ("plus_test_3", 4, 0),
        ];
        for (account, loops, expected) in vectors {
            assert_eq!(loop_for(account, loops), expected, "{account} over {loops}");
        }
    }

    #[test]
    fn two_loops_split_accounts_between_them_roughly_evenly() {
        let accounts: Vec<String> = (0..400).map(|i| format!("user_{i}")).collect();
        let first = accounts.iter().filter(|a| loop_for(a, 2) == 0).count();
        assert!((150..=250).contains(&first), "{first} of 400 on loop 0");
        for loops in [3, 4] {
            for index in 0..loops {
                assert!(accounts.iter().any(|a| loop_for(a, loops) == index));
            }
        }
    }

    #[test]
    fn members_need_an_index_below_their_count() {
        assert_eq!(Member::new(0, 1), Some(Member::default()));
        assert!(Member::new(1, 1).is_none());
        assert!(Member::new(0, 0).is_none());
        assert!(Member::new(0, MAX_LOOPS + 1).is_none());
        assert_eq!(Member::new(3, 4), Some(Member { index: 3, loops: 4 }));
    }

    #[test]
    fn loop_zero_keeps_todays_file_names() {
        assert_eq!(indexed_name("loop.lock", 0), "loop.lock");
        assert_eq!(indexed_name("loop.lock", 2), "loop-2.lock");
        assert_eq!(
            indexed_name("restart-cause.json", 1),
            "restart-cause-1.json"
        );
        assert_eq!(
            indexed_name("dispatch-wake.sock", 3),
            "dispatch-wake-3.sock"
        );
    }

    #[test]
    fn outside_a_loop_every_account_is_owned() {
        // The test process never joins: like the API, it sees every account.
        assert!(own().is_none() || own().is_some_and(|m| m.loops == 1));
        assert!(owns("guest_ab12"));
        let mut all = vec!["admin".to_owned(), "user_one".to_owned()];
        retain_owned(&mut all);
        assert_eq!(all.len(), 2);
    }
}
