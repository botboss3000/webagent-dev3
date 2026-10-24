//! Bounded, independent admission windows; proxy identity is trusted only by socket peer.
use anyhow::{bail, Result};
use axum::http::HeaderMap;
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
}
/// Connection identity and the host's admission clock belong to one request.
#[derive(Clone, Copy)]
pub struct AdmissionContext {
    pub now: u64,
    pub client: IpAddr,
}
type Windows = HashMap<(Admission, IpAddr, String), Vec<u64>>;
#[derive(Clone, Default)]
pub struct LoginAttempts(Arc<Mutex<Windows>>);
impl LoginAttempts {
    /// Debit a client lane and, for login, a narrower client/account window atomically.
    pub async fn admit(&self, kind: Admission, client: IpAddr, username: &str, now: u64) -> bool {
        let mut windows = self.0.lock().await;
        windows.retain(|_, times| {
            times.retain(|t| now.saturating_sub(*t) < 60);
            !times.is_empty()
        });
        let lane = (kind, client, String::new());
        let account = (kind, client, username.to_owned());
        let cap = match kind {
            Admission::Login => 120,
            Admission::Registration => 10,
            Admission::Guest => 30,
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
        if windows.len() + new_keys > 16384 {
            return false;
        }
        windows.entry(lane).or_default().push(now);
        if kind == Admission::Login && !username.is_empty() {
            windows.entry(account).or_default().push(now);
        }
        true
    }
    pub async fn successful_login(&self, client: IpAddr, username: &str) {
        self.0
            .lock()
            .await
            .remove(&(Admission::Login, client, username.to_owned()));
    }
    pub async fn is_empty(&self) -> bool {
        self.0.lock().await.is_empty()
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
