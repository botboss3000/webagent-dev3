//! Bounded, independent admission windows; proxy identity is trusted only by socket peer.
//!
//! All counters, blocks and slowdowns live in process memory: a restart clears them.
//!
//! The client address is `0.0.0.0` when it is unknown: no socket peer, or a trusted proxy
//! (one peer address shared by many visitors) whose request lacks the edge's proof. Per-address
//! caps then count all unknown clients together, and the per-address sign-in block, which would
//! lock an account for everyone, does not apply; the account-wide slowdown still does.
use anyhow::{bail, Result};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, VecDeque},
    hash::Hash,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Admission {
    Login,
    Registration,
    Guest,
    PasswordChange,
}
/// Connection identity and the host's admission clock belong to one request.
#[derive(Clone, Copy)]
pub struct AdmissionContext {
    pub now: u64,
    pub client: IpAddr,
}

/// Per (client address, account) escalating block, counting failed logins only.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AddressBackoff {
    pub failures: u32,
    pub window_seconds: u64,
    pub block_seconds: u64,
    pub max_block_seconds: u64,
}
impl Default for AddressBackoff {
    fn default() -> Self {
        Self {
            failures: 5,
            window_seconds: 300,
            block_seconds: 60,
            max_block_seconds: 3600,
        }
    }
}
/// Account-wide answer delay across all addresses; it never locks the account.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AccountSlowdown {
    pub failures: u32,
    pub window_seconds: u64,
    pub delay_seconds: u64,
    pub max_delay_seconds: u64,
}
impl Default for AccountSlowdown {
    fn default() -> Self {
        Self {
            failures: 5,
            window_seconds: 300,
            delay_seconds: 5,
            max_delay_seconds: 60,
        }
    }
}
/// The `login_protection` section of the shared access-policy document.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LoginProtection {
    pub address: AddressBackoff,
    pub account: AccountSlowdown,
}
impl LoginProtection {
    /// Interpret the existing shared access-policy document; missing values take the defaults.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let Some(section) = policy.get("login_protection") else {
            return Ok(Self::default());
        };
        if !section.is_object() {
            bail!("Invalid login_protection policy: expected an object");
        }
        let protection: Self = serde_json::from_value(section.clone())
            .map_err(|error| anyhow::anyhow!("Invalid login_protection policy: {error}"))?;
        let a = protection.address;
        let b = protection.account;
        check_failures("address.failures", a.failures)?;
        check_seconds("address.window_seconds", a.window_seconds)?;
        check_seconds("address.block_seconds", a.block_seconds)?;
        check_seconds("address.max_block_seconds", a.max_block_seconds)?;
        check_failures("account.failures", b.failures)?;
        check_seconds("account.window_seconds", b.window_seconds)?;
        check_seconds("account.delay_seconds", b.delay_seconds)?;
        check_seconds("account.max_delay_seconds", b.max_delay_seconds)?;
        if a.max_block_seconds < a.block_seconds {
            bail!("login_protection.address.max_block_seconds must be at least block_seconds");
        }
        if b.max_delay_seconds < b.delay_seconds {
            bail!("login_protection.account.max_delay_seconds must be at least delay_seconds");
        }
        Ok(protection)
    }
}
fn check_failures(name: &str, value: u32) -> Result<()> {
    if !(1..=1000).contains(&value) {
        bail!("login_protection.{name} must be 1..=1000");
    }
    Ok(())
}
fn check_seconds(name: &str, value: u64) -> Result<()> {
    if !(1..=86400).contains(&value) {
        bail!("login_protection.{name} must be 1..=86400 seconds");
    }
    Ok(())
}

/// Per-minute caps for one client address, or for all unknown clients together.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct AddressLimits {
    pub login: u32,
    pub registration: u32,
    pub guest: u32,
    pub password_change: u32,
    pub pairing: u32,
    /// Every `/api/` request; only for addresses a trusted front names.
    pub api: u32,
}
impl Default for AddressLimits {
    fn default() -> Self {
        Self {
            login: 120,
            registration: 10,
            guest: 30,
            password_change: 5,
            pairing: 10,
            api: 600,
        }
    }
}
/// Per-minute caps for one signed-in user, or one messaging token.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct UserLimits {
    pub chat_send: u32,
    pub upload: u32,
    pub messaging_api: u32,
}
impl Default for UserLimits {
    fn default() -> Self {
        Self {
            chat_send: 20,
            upload: 30,
            messaging_api: 60,
        }
    }
}
/// The `request_limits` section of the shared access-policy document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RequestLimits {
    pub per_address_per_minute: AddressLimits,
    pub per_user_per_minute: UserLimits,
    /// Wrong pairing codes from all clients together.
    pub pairing_failures_per_minute: u32,
    /// Registered and guest accounts created, from all clients together.
    pub new_accounts_per_day: u32,
}
impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            per_address_per_minute: AddressLimits::default(),
            per_user_per_minute: UserLimits::default(),
            pairing_failures_per_minute: 20,
            new_accounts_per_day: 50,
        }
    }
}
impl RequestLimits {
    /// Interpret the shared access-policy document; missing values take the defaults.
    pub fn from_policy(policy: &Value) -> Result<Self> {
        let Some(section) = policy.get("request_limits") else {
            return Ok(Self::default());
        };
        if !section.is_object() {
            bail!("Invalid request_limits policy: expected an object");
        }
        let limits: Self = serde_json::from_value(section.clone())
            .map_err(|error| anyhow::anyhow!("Invalid request_limits policy: {error}"))?;
        let a = limits.per_address_per_minute;
        let u = limits.per_user_per_minute;
        for (name, value) in [
            ("per_address_per_minute.login", a.login),
            ("per_address_per_minute.registration", a.registration),
            ("per_address_per_minute.guest", a.guest),
            ("per_address_per_minute.password_change", a.password_change),
            ("per_address_per_minute.pairing", a.pairing),
            ("per_address_per_minute.api", a.api),
            ("per_user_per_minute.chat_send", u.chat_send),
            ("per_user_per_minute.upload", u.upload),
            ("per_user_per_minute.messaging_api", u.messaging_api),
            (
                "pairing_failures_per_minute",
                limits.pairing_failures_per_minute,
            ),
            ("new_accounts_per_day", limits.new_accounts_per_day),
        ] {
            if !(1..=100_000).contains(&value) {
                bail!("request_limits.{name} must be 1..=100000");
            }
        }
        Ok(limits)
    }
}

/// Outcome of the failed-login protections for one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginGate {
    /// Refuse without checking the password; retry after this many seconds.
    Blocked { retry_after: u64 },
    /// Check the password after waiting this many seconds (zero for no slowdown).
    Proceed { delay: u64 },
}

const KEY_LIMIT: usize = 16384;
/// Keys a full [`Recent`] table evicts at once: its least recently counted sixteenth, so the
/// scan that finds them runs once per that many new keys.
const EVICT_BATCH: usize = KEY_LIMIT / 16;

/// Event times under one key inside its window, oldest first.
struct Events {
    span: u64,
    times: VecDeque<u64>,
    /// A shared counter (a daily or app-wide cap) that eviction never drops.
    pinned: bool,
}
impl Events {
    fn prune(&mut self, now: u64) {
        while self
            .times
            .front()
            .is_some_and(|t| now.saturating_sub(*t) >= self.span)
        {
            self.times.pop_front();
        }
    }
}

/// Event counts per key, each in its own sliding window, bounded at `KEY_LIMIT` keys. A call
/// prunes only its own key; every key is swept for expired events at most once a second.
/// A full table evicts its least recently counted keys rather than refusing a new one, so a
/// flood of addresses cannot lock newcomers out. Pinned keys are never evicted. Eviction only
/// resets the evicted keys' counts: to win back a throttled budget a caller needs as many
/// fresh keys (addresses) as the table holds, which would already give it that many budgets.
pub struct Recent<K> {
    keys: HashMap<K, Events>,
    swept: u64,
}
impl<K> Default for Recent<K> {
    fn default() -> Self {
        Self {
            keys: HashMap::new(),
            swept: 0,
        }
    }
}
impl<K: Clone + Eq + Hash> Recent<K> {
    fn sweep(&mut self, now: u64) {
        if self.swept == now {
            return;
        }
        self.swept = now;
        self.keys.retain(|_, events| {
            events.prune(now);
            !events.times.is_empty()
        });
    }
    /// Events under `key` inside its window.
    pub fn count(&mut self, key: &K, now: u64) -> usize {
        self.sweep(now);
        self.keys.get_mut(key).map_or(0, |events| {
            events.prune(now);
            events.times.len()
        })
    }
    /// Counts one event under `key` unless `cap` events already fall inside its last `span`
    /// seconds; false, counting nothing, when they do.
    pub fn hit(&mut self, key: K, cap: usize, span: u64, now: u64) -> bool {
        self.insert(key, cap, span, now, false)
    }
    /// [`Self::hit`] for a shared counter that a full table never evicts.
    pub fn hit_pinned(&mut self, key: K, cap: usize, span: u64, now: u64) -> bool {
        self.insert(key, cap, span, now, true)
    }
    fn insert(&mut self, key: K, cap: usize, span: u64, now: u64, pinned: bool) -> bool {
        self.sweep(now);
        if !self.keys.contains_key(&key) {
            self.make_room();
        }
        let events = self.keys.entry(key).or_insert_with(|| Events {
            span,
            times: VecDeque::new(),
            pinned,
        });
        events.span = span;
        events.pinned |= pinned;
        events.prune(now);
        if events.times.len() >= cap {
            return false;
        }
        events.times.push_back(now);
        true
    }
    /// Evicts the least recently counted unpinned keys when the table is full.
    fn make_room(&mut self) {
        if self.keys.len() < KEY_LIMIT {
            return;
        }
        let mut ages: Vec<(u64, K)> = self
            .keys
            .iter()
            .filter(|(_, events)| !events.pinned)
            .map(|(key, events)| (events.times.back().copied().unwrap_or(0), key.clone()))
            .collect();
        let batch = EVICT_BATCH.min(ages.len());
        if batch == 0 {
            return;
        }
        if batch < ages.len() {
            ages.select_nth_unstable_by_key(batch - 1, |(newest, _)| *newest);
        }
        for (_, key) in ages.into_iter().take(batch) {
            self.keys.remove(&key);
        }
    }
    /// Seconds until a hit on `key` could pass again; zero when it would pass now.
    pub fn retry_after(&self, key: &K, cap: usize, now: u64) -> u64 {
        self.keys.get(key).map_or(0, |events| {
            // Only events still inside the window count; expired ones wait for the next
            // hit to prune them, so a caller that only asks is never refused forever.
            let live = || {
                events
                    .times
                    .iter()
                    .filter(|t| now.saturating_sub(**t) < events.span)
            };
            if live().count() < cap {
                return 0;
            }
            live()
                .next()
                .map_or(0, |first| (first + events.span).saturating_sub(now).max(1))
        })
    }
    /// Forgets `key`'s events.
    pub fn remove(&mut self, key: &K) {
        self.keys.remove(key);
    }
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}
/// Recent failure times, capped at the threshold: only "at least `failures`" matters.
#[derive(Default)]
struct Failures(Vec<u64>);
impl Failures {
    fn prune(&mut self, now: u64, window: u64) {
        self.0.retain(|t| now.saturating_sub(*t) < window);
    }
    fn push(&mut self, now: u64, threshold: u32) {
        let threshold = threshold as usize;
        self.0.push(now);
        if self.0.len() > threshold {
            self.0.drain(..self.0.len() - threshold);
        }
    }
    fn reached(&self, threshold: u32) -> bool {
        self.0.len() >= threshold as usize
    }
}
#[derive(Default)]
struct PairState {
    failures: Failures,
    blocked_until: u64,
    // Zero until the first block; afterwards the next block length.
    block: u64,
}
#[derive(Default)]
struct AccountState {
    failures: Failures,
    delay: u64,
    // While slowed, attempts are spaced `delay` apart across all addresses: the time the
    // next admitted attempt may be checked. Parallel guesses queue instead of overlapping.
    next_check: u64,
}
#[derive(Default)]
struct State {
    windows: Recent<(Admission, IpAddr, String)>,
    pairs: HashMap<(IpAddr, String), PairState>,
    accounts: HashMap<String, AccountState>,
    /// Named counters (`hit`): per user, per token, per address or global, each with its window.
    counters: Recent<String>,
}
impl State {
    fn prune(&mut self, protection: &LoginProtection, now: u64) {
        let window = protection.address.window_seconds;
        self.pairs.retain(|_, pair| {
            pair.failures.prune(now, window);
            // Escalation is remembered for one window after the last block ends.
            !pair.failures.0.is_empty() || now < pair.blocked_until.saturating_add(window)
        });
        self.accounts.retain(|_, account| {
            account
                .failures
                .prune(now, protection.account.window_seconds);
            if !account.failures.reached(protection.account.failures) {
                account.delay = 0;
                account.next_check = 0;
            }
            !account.failures.0.is_empty()
        });
    }
}

#[derive(Clone, Default)]
pub struct LoginAttempts(Arc<Mutex<State>>);
impl LoginAttempts {
    /// `admit_with` under the default limits.
    pub async fn admit(&self, kind: Admission, client: IpAddr, username: &str, now: u64) -> bool {
        self.admit_with(&RequestLimits::default(), kind, client, username, now)
            .await
    }
    /// Debit a client lane and, for login, a narrower client/account window atomically.
    /// An unknown client (`0.0.0.0`) shares one lane and has no account window, so other
    /// visitors cannot use it to shut an account out.
    pub async fn admit_with(
        &self,
        limits: &RequestLimits,
        kind: Admission,
        client: IpAddr,
        username: &str,
        now: u64,
    ) -> bool {
        let client = client_group(client);
        let mut state = self.0.lock().await;
        let windows = &mut state.windows;
        let lane = (
            kind,
            client,
            if kind == Admission::PasswordChange {
                username.to_owned()
            } else {
                String::new()
            },
        );
        let account = (kind, client, username.to_owned());
        let caps = limits.per_address_per_minute;
        let cap = match kind {
            Admission::Login => caps.login,
            Admission::Registration => caps.registration,
            Admission::Guest => caps.guest,
            Admission::PasswordChange => caps.password_change,
        } as usize;
        let per_account = kind == Admission::Login && !client.is_unspecified();
        if windows.count(&lane, now) >= cap || (per_account && windows.count(&account, now) >= 5) {
            return false;
        }
        windows.hit(lane, usize::MAX, 60, now);
        if per_account && !username.is_empty() {
            windows.hit(account, usize::MAX, 60, now);
        }
        true
    }
    /// Count one event under a per-client, per-user or per-token `key`; false, counting
    /// nothing, when `cap` events already fall inside the last `window` seconds. A full table
    /// evicts its least recently counted keys ([`Recent`]).
    pub async fn hit(&self, key: String, cap: u32, window: u64, now: u64) -> bool {
        let mut state = self.0.lock().await;
        state.counters.hit(key, cap as usize, window, now)
    }
    /// [`Self::hit`] for a shared counter (a daily or app-wide cap), which eviction never
    /// resets.
    pub async fn hit_shared(&self, key: String, cap: u32, window: u64, now: u64) -> bool {
        let mut state = self.0.lock().await;
        state.counters.hit_pinned(key, cap as usize, window, now)
    }
    /// Seconds until a `hit` on `key` could pass again; zero when it would pass now.
    pub async fn retry_after(&self, key: &str, cap: u32, now: u64) -> u64 {
        let state = self.0.lock().await;
        state
            .counters
            .retry_after(&key.to_owned(), cap as usize, now)
    }
    /// Apply the failed-login block and slowdown before any password check.
    /// Unknown usernames are tracked exactly like real ones.
    pub async fn login_gate(
        &self,
        protection: &LoginProtection,
        client: IpAddr,
        username: &str,
        now: u64,
    ) -> LoginGate {
        let client = client_group(client);
        let mut state = self.0.lock().await;
        state.prune(protection, now);
        let pair_key = (client, username.to_owned());
        let known = !client.is_unspecified();
        if let Some(pair) = state.pairs.get(&pair_key).filter(|_| known) {
            if now < pair.blocked_until {
                return LoginGate::Blocked {
                    retry_after: pair.blocked_until - now,
                };
            }
        }
        let new_keys = usize::from(known && !state.pairs.contains_key(&pair_key))
            + usize::from(!state.accounts.contains_key(username));
        // Never evict active state: a flood of names must not reset a block or slowdown. After
        // pruning, every entry is lockout state (failures still inside their window, a block, or
        // a block's remembered escalation), so evicting even the oldest would let a flood of
        // names erase progress toward a lockout; a full table keeps refusing new names instead.
        if state.pairs.len() + state.accounts.len() + new_keys > KEY_LIMIT {
            return LoginGate::Blocked { retry_after: 60 };
        }
        // Room for the attempt already waiting plus one more, so the owner's correct password
        // is still checked behind a guess in progress.
        let max_wait = protection.account.max_delay_seconds.saturating_mul(2);
        let Some(account) = state.accounts.get_mut(username).filter(|a| a.delay > 0) else {
            return LoginGate::Proceed { delay: 0 };
        };
        // One check per `delay` for the whole account, so concurrent requests cannot
        // multiply the guessing rate. A queue longer than two of the longest delays is refused.
        let check_at = account.next_check.max(now).saturating_add(account.delay);
        if check_at - now > max_wait {
            return LoginGate::Blocked {
                retry_after: check_at - now - max_wait,
            };
        }
        account.next_check = check_at;
        LoginGate::Proceed {
            delay: check_at - now,
        }
    }
    /// Record a rejected password. Gate-admitted attempts insert even at the key bound;
    /// the overshoot is limited by concurrent password verifications.
    pub async fn failed_login(
        &self,
        protection: &LoginProtection,
        client: IpAddr,
        username: &str,
        now: u64,
    ) {
        let client = client_group(client);
        let mut state = self.0.lock().await;
        let address = protection.address;
        if !client.is_unspecified() {
            let pair = state
                .pairs
                .entry((client, username.to_owned()))
                .or_default();
            pair.failures.prune(now, address.window_seconds);
            pair.failures.push(now, address.failures);
            if pair.block > 0 {
                pair.block = pair.block.saturating_mul(2).min(address.max_block_seconds);
                pair.blocked_until = now.saturating_add(pair.block);
            } else if pair.failures.reached(address.failures) {
                pair.block = address.block_seconds;
                pair.blocked_until = now.saturating_add(pair.block);
            }
        }
        let account = protection.account;
        let entry = state.accounts.entry(username.to_owned()).or_default();
        entry.failures.prune(now, account.window_seconds);
        entry.failures.push(now, account.failures);
        if entry.failures.reached(account.failures) {
            entry.delay = if entry.delay == 0 {
                account.delay_seconds
            } else {
                entry.delay.saturating_mul(2).min(account.max_delay_seconds)
            };
        }
    }
    /// A correct password clears the pair's minute window, its block and the account slowdown.
    pub async fn successful_login(&self, client: IpAddr, username: &str) {
        let client = client_group(client);
        let mut state = self.0.lock().await;
        state
            .windows
            .remove(&(Admission::Login, client, username.to_owned()));
        state.pairs.remove(&(client, username.to_owned()));
        state.accounts.remove(username);
    }
    pub async fn is_empty(&self) -> bool {
        let state = self.0.lock().await;
        state.windows.is_empty()
            && state.pairs.is_empty()
            && state.accounts.is_empty()
            && state.counters.is_empty()
    }
    pub fn shares_state_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

/// Missing connection metadata never grants trust to a client-supplied forwarding header.
pub fn client_ip(
    peer: Option<SocketAddr>,
    headers: &HeaderMap,
    settings: &Value,
) -> Result<IpAddr> {
    let configured = &settings["authentication"]["trusted_proxy_ips"];
    let mut trusted = Vec::new();
    if !configured.is_null() {
        let values = configured
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Invalid trusted proxy configuration"))?;
        if values.len() > 32 {
            bail!("Too many trusted proxies");
        }
        for value in values {
            trusted.push(
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid trusted proxy"))?
                    .parse::<IpAddr>()?,
            );
        }
    }
    let Some(peer) = peer else {
        return Ok(Ipv4Addr::UNSPECIFIED.into());
    };
    let peer = normalize(peer.ip());
    if trusted.into_iter().map(normalize).any(|ip| ip == peer) {
        // Docker/Tailscale can merge unrelated connections onto one gateway peer.
        // A Cloudflare-set proof is required in addition to that peer's address.
        // The peer stands for many visitors. Without the edge's proof the client is unknown,
        // never the shared peer: counting everyone as one address would let any visitor
        // trip a per-address block for all of them.
        let unknown = Ipv4Addr::UNSPECIFIED.into();
        let Some(expected) = settings["authentication"]["proxy_header_sha256"]
            .as_str()
            .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .map(str::to_ascii_lowercase)
        else {
            return Ok(unknown);
        };
        let mut proofs = headers.get_all("x-webagent-client-proof").iter();
        let Some(proof) = proofs
            .next()
            .filter(|v| (32..=256).contains(&v.as_bytes().len()))
        else {
            return Ok(unknown);
        };
        if proofs.next().is_some() {
            return Ok(unknown);
        }
        let actual = format!("{:x}", Sha256::digest(proof.as_bytes()));
        if actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
        {
            return Ok(unknown);
        }

        let mut values = headers.get_all("cf-connecting-ip").iter();
        let first = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Trusted proxy omitted client address"))?;
        if values.next().is_some() {
            bail!("Multiple client addresses");
        }
        return Ok(normalize(first.to_str()?.parse()?));
    }
    Ok(peer)
}
/// The address a throttle counts a client under: IPv4 as it is, IPv6 by its /64 network,
/// which one subscriber or server usually holds whole, so rotating addresses inside it gains
/// no extra budget.
#[must_use]
pub fn client_group(ip: IpAddr) -> IpAddr {
    match normalize(ip) {
        IpAddr::V6(v6) => IpAddr::V6(Ipv6Addr::from(u128::from(v6) & !u128::from(u64::MAX))),
        v4 => v4,
    }
}
fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(ip),
        _ => ip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lanes_accounts_clients_and_expiry_are_independent() {
        let b = LoginAttempts::default();
        let a = IpAddr::from([192, 0, 2, 1]);
        let c = IpAddr::from([192, 0, 2, 2]);
        for _ in 0..5 {
            assert!(b.admit(Admission::Login, a, "alice", 100).await);
        }
        assert!(!b.clone().admit(Admission::Login, a, "alice", 159).await);
        assert!(b.admit(Admission::Login, a, "bob", 100).await);
        assert!(b.admit(Admission::Login, c, "alice", 100).await);
        assert!(b.admit(Admission::Registration, a, "", 100).await);
        assert!(b.admit(Admission::Guest, a, "", 100).await);
        assert!(b.admit(Admission::Login, a, "alice", 160).await);
        b.successful_login(a, "alice").await;
        assert!(b.admit(Admission::Login, a, "alice", 160).await);
    }
    #[tokio::test]
    async fn rotating_names_do_not_bypass_client_budget() {
        let b = LoginAttempts::default();
        let ip = IpAddr::from([192, 0, 2, 3]);
        for n in 0..120 {
            assert!(b.admit(Admission::Login, ip, &n.to_string(), 10).await);
        }
        assert!(!b.admit(Admission::Login, ip, "next", 10).await);
        assert!(b.admit(Admission::Guest, ip, "", 10).await);
        assert!(
            b.admit(Admission::Login, IpAddr::from([192, 0, 2, 4]), "next", 10)
                .await
        );
    }
    #[tokio::test]
    async fn password_changes_are_account_scoped_and_expire() {
        let b = LoginAttempts::default();
        let ip = IpAddr::from([0, 0, 0, 0]);
        for _ in 0..5 {
            assert!(b.admit(Admission::PasswordChange, ip, "alice", 100).await);
        }
        assert!(
            !b.clone()
                .admit(Admission::PasswordChange, ip, "alice", 159)
                .await
        );
        assert!(b.admit(Admission::PasswordChange, ip, "bob", 159).await);
        assert!(b.admit(Admission::Login, ip, "alice", 159).await);
        assert!(b.admit(Admission::PasswordChange, ip, "alice", 160).await);
    }
    fn gate(delay: u64) -> LoginGate {
        LoginGate::Proceed { delay }
    }
    async fn fail(b: &LoginAttempts, p: &LoginProtection, ip: IpAddr, user: &str, now: u64) {
        assert!(matches!(
            b.login_gate(p, ip, user, now).await,
            LoginGate::Proceed { .. }
        ));
        b.failed_login(p, ip, user, now).await;
    }
    #[tokio::test]
    async fn address_backoff_blocks_doubles_caps_and_clears() {
        let p = LoginProtection::default();
        let b = LoginAttempts::default();
        let ip = IpAddr::from([192, 0, 2, 10]);
        for t in 0..4 {
            fail(&b, &p, ip, "alice", 100 + t).await;
        }
        assert!(matches!(
            b.login_gate(&p, ip, "alice", 104).await,
            LoginGate::Proceed { .. }
        ));
        b.failed_login(&p, ip, "alice", 104).await;
        assert_eq!(
            b.login_gate(&p, ip, "alice", 104).await,
            LoginGate::Blocked { retry_after: 60 }
        );
        assert_eq!(
            b.login_gate(&p, ip, "alice", 150).await,
            LoginGate::Blocked { retry_after: 14 }
        );
        // Other addresses and accounts are not blocked by this pair.
        assert!(matches!(
            b.login_gate(&p, IpAddr::from([192, 0, 2, 11]), "alice", 150)
                .await,
            LoginGate::Proceed { .. }
        ));
        assert_eq!(b.login_gate(&p, ip, "bob", 150).await, gate(0));
        let mut now = 164;
        let mut expected = 60;
        for _ in 0..8 {
            fail(&b, &p, ip, "alice", now).await;
            expected = (expected * 2).min(3600);
            assert_eq!(
                b.login_gate(&p, ip, "alice", now).await,
                LoginGate::Blocked {
                    retry_after: expected
                }
            );
            now += expected;
        }
        assert_eq!(expected, 3600);
        b.successful_login(ip, "alice").await;
        assert!(b.is_empty().await);
        assert_eq!(b.login_gate(&p, ip, "alice", now).await, gate(0));
    }
    #[tokio::test]
    async fn failures_outside_the_window_do_not_count() {
        let p = LoginProtection::default();
        let b = LoginAttempts::default();
        let ip = IpAddr::from([192, 0, 2, 12]);
        for t in [0, 100, 200, 299] {
            fail(&b, &p, ip, "ghost", 1000 + t).await;
        }
        // The first failure has aged out when the fifth arrives.
        fail(&b, &p, ip, "ghost", 1300).await;
        assert_eq!(b.login_gate(&p, ip, "ghost", 1300).await, gate(0));
        fail(&b, &p, ip, "ghost", 1301).await;
        assert!(matches!(
            b.login_gate(&p, ip, "ghost", 1301).await,
            LoginGate::Blocked { .. }
        ));
        // Block and window both expire; the pair starts from scratch.
        assert_eq!(b.login_gate(&p, ip, "ghost", 1700).await, gate(0));
        assert!(b.is_empty().await);
    }
    #[tokio::test]
    async fn account_slowdown_spans_addresses_doubles_caps_and_clears() {
        let p = LoginProtection::default();
        let b = LoginAttempts::default();
        let ip = |n: u8| IpAddr::from([198, 51, 100, n]);
        for n in 0..5 {
            assert_eq!(b.login_gate(&p, ip(n), "carol", 100).await, gate(0));
            b.failed_login(&p, ip(n), "carol", 100).await;
        }
        let mut expected = 5;
        let mut now = 100;
        for n in 5..12 {
            assert_eq!(b.login_gate(&p, ip(n), "carol", now).await, gate(expected));
            now += expected;
            b.failed_login(&p, ip(n), "carol", now).await;
            expected = (expected * 2).min(60);
        }
        assert_eq!(expected, 60);
        assert_eq!(b.login_gate(&p, ip(99), "carol", now).await, gate(60));
        assert_eq!(b.login_gate(&p, ip(99), "dave", now).await, gate(0));
        // Concurrent guesses queue behind the one already waiting: the account is checked at
        // most once per delay, and a queue longer than two of the longest delays is refused.
        assert_eq!(b.login_gate(&p, ip(98), "carol", now).await, gate(120));
        assert_eq!(
            b.login_gate(&p, ip(97), "carol", now).await,
            LoginGate::Blocked { retry_after: 60 }
        );
        // A correct password is still checked once its turn comes, and success clears it.
        b.successful_login(ip(99), "carol").await;
        assert_eq!(b.login_gate(&p, ip(1), "carol", now).await, gate(0));
    }
    #[tokio::test]
    async fn account_slowdown_ends_with_its_window() {
        let p = LoginProtection::default();
        let b = LoginAttempts::default();
        for n in 0..6 {
            b.failed_login(
                &p,
                IpAddr::from([203, 0, 113, n]),
                "erin",
                100 + u64::from(n),
            )
            .await;
        }
        let ip = IpAddr::from([203, 0, 113, 50]);
        assert_eq!(b.login_gate(&p, ip, "erin", 400).await, gate(10));
        // At t=401 only four failures remain inside the 300 s window.
        assert_eq!(b.login_gate(&p, ip, "erin", 401).await, gate(0));
        assert_eq!(b.login_gate(&p, ip, "erin", 406).await, gate(0));
        assert!(b.is_empty().await);
    }
    #[tokio::test]
    async fn protection_state_is_bounded_without_eviction() {
        let p = LoginProtection::default();
        let b = LoginAttempts::default();
        let ip = IpAddr::from([192, 0, 2, 20]);
        for n in 0..5 {
            b.failed_login(&p, ip, "target", n).await;
        }
        {
            let mut state = b.0.lock().await;
            for n in 0..(KEY_LIMIT - 2) {
                state.accounts.insert(
                    n.to_string(),
                    AccountState {
                        failures: Failures(vec![10]),
                        delay: 0,
                        next_check: 0,
                    },
                );
            }
        }
        assert_eq!(
            b.login_gate(&p, ip, "fresh", 10).await,
            LoginGate::Blocked { retry_after: 60 }
        );
        assert!(matches!(
            b.login_gate(&p, ip, "target", 10).await,
            LoginGate::Blocked { retry_after: 54 }
        ));
        assert_eq!(b.0.lock().await.pairs.len(), 1);
    }
    #[test]
    fn login_protection_defaults_and_bounds() {
        let defaults = LoginProtection::default();
        assert_eq!(
            LoginProtection::from_policy(&serde_json::json!({})).unwrap(),
            defaults
        );
        let partial = LoginProtection::from_policy(&serde_json::json!({
            "login_protection": {"account": {"delay_seconds": 2}}
        }))
        .unwrap();
        assert_eq!(partial.address, defaults.address);
        assert_eq!(partial.account.delay_seconds, 2);
        assert_eq!(partial.account.max_delay_seconds, 60);
        assert_eq!(defaults.address.window_seconds, 300);
        assert_eq!(defaults.address.max_block_seconds, 3600);
        assert_eq!(defaults.account.failures, 5);
        for section in [
            serde_json::json!(null),
            serde_json::json!([]),
            serde_json::json!({"address": {"failures": 0}}),
            serde_json::json!({"account": {"failures": 1001}}),
            serde_json::json!({"address": {"window_seconds": 0}}),
            serde_json::json!({"account": {"max_delay_seconds": 86401}}),
            serde_json::json!({"account": {"delay_seconds": 61}}),
            serde_json::json!({"address": {"block_seconds": "60"}}),
            serde_json::json!({"extra": {}}),
        ] {
            assert!(
                LoginProtection::from_policy(&serde_json::json!({"login_protection": section}))
                    .is_err(),
                "{section}"
            );
        }
    }
    #[tokio::test]
    async fn an_unknown_client_slows_the_account_but_never_blocks_it() {
        let attempts = LoginAttempts::default();
        let protection = LoginProtection::default();
        let unknown = IpAddr::from([0, 0, 0, 0]);
        for t in 0..20 {
            attempts
                .failed_login(&protection, unknown, "admin", t)
                .await;
        }
        // Everyone behind a shared proxy is unknown: a block here would lock admin for all.
        match attempts.login_gate(&protection, unknown, "admin", 21).await {
            LoginGate::Proceed { delay } => assert!(delay > 0),
            LoginGate::Blocked { .. } => panic!("an unknown client must not block the account"),
        }
        // Nor does the per-address, per-account minute window apply.
        let limits = RequestLimits::default();
        for _ in 0..30 {
            assert!(
                attempts
                    .admit_with(&limits, Admission::Login, unknown, "admin", 100)
                    .await
            );
        }
        // A known address is still blocked after its failures.
        let known = IpAddr::from([198, 51, 100, 7]);
        for t in 0..5 {
            attempts
                .failed_login(&protection, known, "admin", 200 + t)
                .await;
        }
        assert!(matches!(
            attempts.login_gate(&protection, known, "admin", 205).await,
            LoginGate::Blocked { .. }
        ));
    }
    #[tokio::test]
    async fn named_counters_refuse_past_their_cap_and_report_the_wait() {
        let attempts = LoginAttempts::default();
        for _ in 0..3 {
            assert!(attempts.hit("user:chat_send:u1".into(), 3, 60, 100).await);
        }
        assert!(!attempts.hit("user:chat_send:u1".into(), 3, 60, 110).await);
        assert_eq!(attempts.retry_after("user:chat_send:u1", 3, 110).await, 50);
        assert!(attempts.hit("user:chat_send:u2".into(), 3, 60, 110).await);
        assert!(attempts.hit("user:chat_send:u1".into(), 3, 60, 160).await);
    }
    #[tokio::test]
    async fn a_full_counter_table_admits_newcomers_by_evicting_its_oldest_keys() {
        let attempts = LoginAttempts::default();
        // A shared daily cap counted first, so it is the oldest key of all.
        assert!(
            attempts
                .hit_shared("new_accounts".into(), 1, 86_400, 100)
                .await
        );
        for n in 0..KEY_LIMIT - 2 {
            assert!(attempts.hit(format!("address:api:{n}"), 3, 60, 101).await);
        }
        // A throttled client counted most recently.
        for _ in 0..3 {
            assert!(attempts.hit("address:api:busy".into(), 3, 60, 102).await);
        }
        assert_eq!(attempts.0.lock().await.counters.keys.len(), KEY_LIMIT);
        // Before: a new key at the cap was refused for everyone. Now it is admitted.
        assert!(attempts.hit("address:api:new".into(), 3, 60, 103).await);
        let state = attempts.0.lock().await;
        assert_eq!(state.counters.keys.len(), KEY_LIMIT - EVICT_BATCH + 1);
        drop(state);
        // The cap still holds where it matters: the recent throttled key, and the pinned
        // daily cap, survive the eviction.
        assert!(!attempts.hit("address:api:busy".into(), 3, 60, 103).await);
        assert_eq!(attempts.retry_after("address:api:busy", 3, 103).await, 59);
        assert!(
            !attempts
                .hit_shared("new_accounts".into(), 1, 86_400, 103)
                .await
        );
    }
    #[tokio::test]
    async fn a_full_admission_table_admits_newcomers_and_keeps_recent_limits() {
        let b = LoginAttempts::default();
        let throttled = IpAddr::from([198, 51, 100, 1]);
        for n in 0..(KEY_LIMIT - 1) as u32 {
            let ip = IpAddr::from((0xC000_0000u32 + n).to_be_bytes());
            assert!(b.admit(Admission::Registration, ip, "", 10).await);
        }
        let cap = RequestLimits::default().per_address_per_minute.registration;
        for _ in 0..cap {
            assert!(b.admit(Admission::Registration, throttled, "", 11).await);
        }
        assert!(!b.admit(Admission::Registration, throttled, "", 11).await);
        let newcomer = IpAddr::from([203, 0, 113, 9]);
        assert!(b.admit(Admission::Registration, newcomer, "", 12).await);
        assert!(!b.admit(Admission::Registration, throttled, "", 12).await);
    }
    #[tokio::test]
    async fn ipv6_clients_count_by_their_64_network() {
        let b = LoginAttempts::default();
        let cap = RequestLimits::default().per_address_per_minute.registration;
        for n in 0..cap {
            let ip = IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, n as u16 + 1]);
            assert!(b.admit(Admission::Registration, ip, "", 10).await);
        }
        let same_64 = IpAddr::from([0x2001, 0xdb8, 0, 1, 0xffff, 1, 2, 3]);
        assert!(!b.admit(Admission::Registration, same_64, "", 10).await);
        let next_64 = IpAddr::from([0x2001, 0xdb8, 0, 2, 0, 0, 0, 1]);
        assert!(b.admit(Admission::Registration, next_64, "", 10).await);
        assert_eq!(
            client_group(same_64),
            IpAddr::from([0x2001, 0xdb8, 0, 1, 0, 0, 0, 0])
        );
        let v4 = IpAddr::from([192, 0, 2, 7]);
        assert_eq!(client_group(v4), v4);
        let mapped = IpAddr::from(std::net::Ipv4Addr::new(192, 0, 2, 7).to_ipv6_mapped());
        assert_eq!(client_group(mapped), v4);
    }
    #[test]
    fn request_limits_default_and_reject_out_of_range_values() {
        let defaults = RequestLimits::from_policy(&serde_json::json!({})).unwrap();
        assert_eq!(defaults.per_user_per_minute.chat_send, 20);
        assert_eq!(defaults.new_accounts_per_day, 50);
        let set = RequestLimits::from_policy(
            &serde_json::json!({"request_limits":{"per_user_per_minute":{"upload":7}}}),
        )
        .unwrap();
        assert_eq!(set.per_user_per_minute.upload, 7);
        assert_eq!(set.per_user_per_minute.chat_send, 20);
        for section in [
            serde_json::json!([]),
            serde_json::json!({"per_user_per_minute":{"chat_send":0}}),
            serde_json::json!({"new_accounts_per_day":100001}),
            serde_json::json!({"per_address_per_minute":{"unknown":1}}),
        ] {
            assert!(
                RequestLimits::from_policy(&serde_json::json!({"request_limits": section}))
                    .is_err(),
                "{section}"
            );
        }
    }
    #[test]
    fn forwarded_identity_requires_explicit_socket_peer_trust() {
        let mut h = HeaderMap::new();
        h.insert("cf-connecting-ip", "198.51.100.7".parse().unwrap());
        let peer = "192.0.2.8:1234".parse().unwrap();
        assert_eq!(
            client_ip(Some(peer), &h, &serde_json::json!({})).unwrap(),
            peer.ip()
        );
        let proof = concat!("test-edge-", "identity-proof-with-sufficient-entropy");
        let settings = serde_json::json!({"authentication":{"trusted_proxy_ips":["192.0.2.8"],"proxy_header_sha256":format!("{:x}", Sha256::digest(proof.as_bytes()))}});
        // Untrusted direct traffic behind the same Docker peer cannot forge its client identity,
        // and is never counted as the shared peer itself.
        let unknown = IpAddr::from([0, 0, 0, 0]);
        assert_eq!(client_ip(Some(peer), &h, &settings).unwrap(), unknown);
        let unproven = serde_json::json!({"authentication":{"trusted_proxy_ips":["192.0.2.8"]}});
        assert_eq!(client_ip(Some(peer), &h, &unproven).unwrap(), unknown);
        h.insert(
            "x-webagent-client-proof",
            "wrong-proof-with-at-least-thirty-two-bytes"
                .parse()
                .unwrap(),
        );
        assert_eq!(client_ip(Some(peer), &h, &settings).unwrap(), unknown);
        h.insert("x-webagent-client-proof", proof.parse().unwrap());
        assert_eq!(
            client_ip(Some(peer), &h, &settings).unwrap(),
            "198.51.100.7".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            client_ip(None, &h, &settings).unwrap(),
            IpAddr::from([0, 0, 0, 0])
        );
        h.insert("cf-connecting-ip", "invalid".parse().unwrap());
        assert!(client_ip(Some(peer), &h, &settings).is_err());
        h.remove("cf-connecting-ip");
        assert!(client_ip(Some(peer), &h, &settings).is_err());
    }
}
