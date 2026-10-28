//! Bounded, independent admission windows; proxy identity is trusted only by socket peer.
//!
//! All counters, blocks and slowdowns live in process memory: a restart clears them.
use anyhow::{bail, Result};
use axum::http::HeaderMap;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
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

/// Outcome of the failed-login protections for one attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginGate {
    /// Refuse without checking the password; retry after this many seconds.
    Blocked { retry_after: u64 },
    /// Check the password after waiting this many seconds (zero for no slowdown).
    Proceed { delay: u64 },
}

const KEY_LIMIT: usize = 16384;
type Windows = HashMap<(Admission, IpAddr, String), Vec<u64>>;
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
}
#[derive(Default)]
struct State {
    windows: Windows,
    pairs: HashMap<(IpAddr, String), PairState>,
    accounts: HashMap<String, AccountState>,
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
            }
            !account.failures.0.is_empty()
        });
    }
}

#[derive(Clone, Default)]
pub struct LoginAttempts(Arc<Mutex<State>>);
impl LoginAttempts {
    /// Debit a client lane and, for login, a narrower client/account window atomically.
    pub async fn admit(&self, kind: Admission, client: IpAddr, username: &str, now: u64) -> bool {
        let mut state = self.0.lock().await;
        let windows = &mut state.windows;

        windows.retain(|_, times| {
            times.retain(|t| now.saturating_sub(*t) < 60);
            !times.is_empty()
        });
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
        let cap = match kind {
            Admission::Login => 120,
            Admission::Registration => 10,
            Admission::Guest => 30,
            Admission::PasswordChange => 5,
        };
        if windows.get(&lane).is_some_and(|v| v.len() >= cap)
            || (kind == Admission::Login && windows.get(&account).is_some_and(|v| v.len() >= 5))
        {
            return false;
        }
        let new_keys = usize::from(!windows.contains_key(&lane))
            + usize::from(
                kind == Admission::Login && account != lane && !windows.contains_key(&account),
            );
        // Never evict active counters: random account names must not reset a throttled window.
        if windows.len() + new_keys > KEY_LIMIT {
            return false;
        }
        windows.entry(lane).or_default().push(now);
        if kind == Admission::Login && !username.is_empty() {
            windows.entry(account).or_default().push(now);
        }
        true
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
        let mut state = self.0.lock().await;
        state.prune(protection, now);
        let pair_key = (client, username.to_owned());
        if let Some(pair) = state.pairs.get(&pair_key) {
            if now < pair.blocked_until {
                return LoginGate::Blocked {
                    retry_after: pair.blocked_until - now,
                };
            }
        }
        let new_keys = usize::from(!state.pairs.contains_key(&pair_key))
            + usize::from(!state.accounts.contains_key(username));
        // Never evict active state: a flood of names must not reset a block or slowdown.
        if state.pairs.len() + state.accounts.len() + new_keys > KEY_LIMIT {
            return LoginGate::Blocked { retry_after: 60 };
        }
        let delay = state.accounts.get(username).map_or(0, |a| a.delay);
        LoginGate::Proceed { delay }
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
        let mut state = self.0.lock().await;
        let address = protection.address;
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
        let mut state = self.0.lock().await;
        state
            .windows
            .remove(&(Admission::Login, client, username.to_owned()));
        state.pairs.remove(&(client, username.to_owned()));
        state.accounts.remove(username);
    }
    pub async fn is_empty(&self) -> bool {
        let state = self.0.lock().await;
        state.windows.is_empty() && state.pairs.is_empty() && state.accounts.is_empty()
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
        let expected = settings["authentication"]["proxy_header_sha256"]
            .as_str()
            .filter(|v| v.len() == 64 && v.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow::anyhow!("Trusted proxy proof is not configured"))?
            .to_ascii_lowercase();
        let mut proofs = headers.get_all("x-webagent-client-proof").iter();
        let Some(proof) = proofs
            .next()
            .filter(|v| (32..=256).contains(&v.as_bytes().len()))
        else {
            return Ok(peer);
        };
        if proofs.next().is_some() {
            return Ok(peer);
        }
        let actual = format!("{:x}", Sha256::digest(proof.as_bytes()));
        if actual
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            != 0
        {
            return Ok(peer);
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
        for n in 5..12 {
            assert_eq!(b.login_gate(&p, ip(n), "carol", 100).await, gate(expected));
            b.failed_login(&p, ip(n), "carol", 100).await;
            expected = (expected * 2).min(60);
        }
        assert_eq!(expected, 60);
        assert_eq!(b.login_gate(&p, ip(99), "carol", 100).await, gate(60));
        assert_eq!(b.login_gate(&p, ip(99), "dave", 100).await, gate(0));
        // The slowdown never blocks: a correct password is still checked, and success clears it.
        b.successful_login(ip(99), "carol").await;
        assert_eq!(b.login_gate(&p, ip(1), "carol", 100).await, gate(0));
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
        // Untrusted direct traffic behind the same Docker peer cannot forge its client identity.
        assert_eq!(client_ip(Some(peer), &h, &settings).unwrap(), peer.ip());
        h.insert(
            "x-webagent-client-proof",
            "wrong-proof-with-at-least-thirty-two-bytes"
                .parse()
                .unwrap(),
        );
        assert_eq!(client_ip(Some(peer), &h, &settings).unwrap(), peer.ip());
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
