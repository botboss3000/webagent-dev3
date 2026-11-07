//! Egress guard for the signed-out browser: a local proxy that only reaches public
//! addresses.
//!
//! Chromium sends every request here, loopback included (`--proxy-bypass-list=<-loopback>`).
//! Each CONNECT or absolute-URI request is resolved here, refused when any resolved address
//! is not public, and connected to the checked address, so DNS cannot swap in an internal
//! address after the check. One request per client connection, so a reused proxy
//! connection cannot reach a second, unchecked host.
use super::rules::is_global;
use super::url::urlsplit;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const PORTS: [u16; 2] = [80, 443];
const MAX_HEADER: usize = 16384;
const IDLE: Duration = Duration::from_secs(60);
const REFUSALS: usize = 64;

/// True only for globally routable unicast addresses (not private, Tailscale, link-local
/// or reserved); IPv4-mapped IPv6 addresses are judged as IPv4.
#[must_use]
pub fn public(address: IpAddr) -> bool {
    let address = match address {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(address, IpAddr::V4),
        v4 => v4,
    };
    is_global(address) && !address.is_multicast()
}

/// Checks a host's resolved addresses: all must be public. Returns the address to
/// connect to, or the refusal reason.
///
/// # Errors
/// The reason the host is refused.
pub fn decide(port: u16, addresses: &[IpAddr]) -> Result<IpAddr, &'static str> {
    if !PORTS.contains(&port) {
        return Err("only ports 80 and 443 are allowed");
    }
    let Some(first) = addresses.first() else {
        return Err("the name does not resolve");
    };
    if !addresses.iter().all(|address| public(*address)) {
        return Err("it resolves to a private or internal address");
    }
    Ok(*first)
}

async fn resolve(host: &str, port: u16) -> Result<IpAddr, &'static str> {
    if !PORTS.contains(&port) {
        return Err("only ports 80 and 443 are allowed");
    }
    let found: Vec<IpAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(found) => found.map(|address: SocketAddr| address.ip()).collect(),
        Err(_) => return Err("the name does not resolve"),
    };
    decide(port, &found)
}

/// The proxy, bound to loopback; it keeps the latest refusal reason per host for the
/// browser's error text.
pub struct Guard {
    port: u16,
    refusals: Mutex<Vec<(String, String, Instant)>>,
}

impl Guard {
    /// Binds the proxy on a free loopback port and serves it for the life of the process.
    ///
    /// # Errors
    /// When the port cannot be bound.
    pub async fn start() -> std::io::Result<Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let guard = Arc::new(Self {
            port: listener.local_addr()?.port(),
            refusals: Mutex::new(Vec::new()),
        });
        let serving = Arc::clone(&guard);
        tokio::spawn(async move {
            loop {
                let Ok((client, _)) = listener.accept().await else {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                };
                let guard = Arc::clone(&serving);
                tokio::spawn(async move { guard.serve(client).await });
            }
        });
        Ok(guard)
    }

    /// The proxy's loopback port.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Records why `host` was refused; the oldest of more than 64 hosts is forgotten.
    pub fn refused(&self, host: &str, reason: &str) {
        let mut refusals = self.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        if let Some(entry) = refusals.iter_mut().find(|(seen, _, _)| seen == host) {
            entry.1 = reason.to_owned();
            entry.2 = now;
        } else {
            refusals.push((host.to_owned(), reason.to_owned(), now));
        }
        while refusals.len() > REFUSALS {
            refusals.remove(0);
        }
    }

    /// The refusal reason for `host` from the last 30 seconds, if any.
    #[must_use]
    pub fn reason(&self, host: &str) -> Option<String> {
        let refusals = self.refusals.lock().unwrap_or_else(PoisonError::into_inner);
        refusals
            .iter()
            .find(|(seen, _, _)| seen == host)
            .filter(|(_, _, at)| at.elapsed() <= Duration::from_secs(30))
            .map(|(_, reason, _)| reason.clone())
    }

    async fn serve(&self, mut client: TcpStream) {
        let mut head = Vec::new();
        let mut chunk = [0u8; 4096];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = match tokio::time::timeout(IDLE, client.read(&mut chunk)).await {
                Ok(Ok(read)) => read,
                _ => return,
            };
            if read == 0 || head.len() + read > MAX_HEADER {
                return;
            }
            head.extend_from_slice(&chunk[..read]);
        }
        let (line, rest) = match head.windows(2).position(|window| window == b"\r\n") {
            Some(at) => (&head[..at], &head[at + 2..]),
            None => (&head[..], &[][..]),
        };
        // latin-1: every byte is one character.
        let line: String = line.iter().map(|byte| char::from(*byte)).collect();
        let mut parts = line.splitn(3, ' ');
        let (Some(method), Some(target), Some(version)) =
            (parts.next(), parts.next(), parts.next())
        else {
            return;
        };
        let connect = method == "CONNECT";
        let (host, port, path) = if connect {
            let (host, port) = target.rsplit_once(':').unwrap_or(("", target));
            let host = host.trim_matches(|c| c == '[' || c == ']').to_lowercase();
            let port = if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
                port.parse::<u16>().unwrap_or(0)
            } else {
                0
            };
            (host, port, String::new())
        } else {
            let Ok(parsed) = urlsplit(target) else {
                return;
            };
            let host = parsed.hostname();
            let Some(host) = host.filter(|_| parsed.scheme == "http") else {
                let _ = client
                    .write_all(b"HTTP/1.1 400 Bad Request\r\nConnection: close\r\n\r\n")
                    .await;
                return;
            };
            let Ok(port) = parsed.port() else {
                return;
            };
            let path = format!(
                "{}{}",
                if parsed.path.is_empty() {
                    "/"
                } else {
                    &parsed.path
                },
                if parsed.query.is_empty() {
                    String::new()
                } else {
                    format!("?{}", parsed.query)
                }
            );
            (host, port.filter(|port| *port != 0).unwrap_or(80), path)
        };
        let address = match resolve(&host, port).await {
            Ok(address) => address,
            Err(reason) => {
                self.refused(&host, reason);
                let _ = client
                    .write_all(
                        b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
                return;
            }
        };
        let upstream = match tokio::time::timeout(IDLE, TcpStream::connect((address, port))).await {
            Ok(Ok(upstream)) => upstream,
            _ => {
                let _ = client
                    .write_all(
                        b"HTTP/1.1 502 Bad Gateway\r\nConnection: close\r\nContent-Length: 0\r\n\r\n",
                    )
                    .await;
                return;
            }
        };
        let mut upstream = upstream;
        let after_head = |data: &[u8]| -> (Vec<u8>, Vec<u8>) {
            match data.windows(4).position(|window| window == b"\r\n\r\n") {
                Some(at) => (data[..at].to_vec(), data[at + 4..].to_vec()),
                None => (data.to_vec(), Vec::new()),
            }
        };
        let (block, body) = after_head(rest);
        if connect {
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .is_err()
            {
                return;
            }
            if !body.is_empty() && upstream.write_all(&body).await.is_err() {
                return;
            }
        } else {
            let mut lines = Vec::new();
            let mut start = 0;
            while let Some(at) = block[start..].windows(2).position(|w| w == b"\r\n") {
                lines.push(&block[start..start + at]);
                start += at + 2;
            }
            lines.push(&block[start..]);
            let headers: Vec<&[u8]> = lines
                .into_iter()
                .filter(|header| {
                    let lower = header.to_ascii_lowercase();
                    !header.is_empty()
                        && !lower.starts_with(b"proxy-connection:")
                        && !lower.starts_with(b"connection:")
                        && !lower.starts_with(b"keep-alive:")
                })
                .collect();
            // Connection: close keeps one checked host per client connection.
            let mut request: Vec<u8> = format!("{method} {path} {version}\r\n")
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                .collect();
            request.extend_from_slice(&headers.join(&b"\r\n"[..]));
            request.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
            request.extend_from_slice(&body);
            if upstream.write_all(&request).await.is_err() {
                return;
            }
        }
        relay(client, upstream).await;
    }
}

/// Copies bytes both ways until either side closes or the connection idles out.
async fn relay(mut left: TcpStream, mut right: TcpStream) {
    let (mut left_buffer, mut right_buffer) = (vec![0u8; 65536], vec![0u8; 65536]);
    loop {
        tokio::select! {
            read = left.read(&mut left_buffer) => match read {
                Ok(read) if read > 0 => {
                    if right.write_all(&left_buffer[..read]).await.is_err() {
                        return;
                    }
                }
                _ => return,
            },
            read = right.read(&mut right_buffer) => match read {
                Ok(read) if read > 0 => {
                    if left.write_all(&right_buffer[..read]).await.is_err() {
                        return;
                    }
                }
                _ => return,
            },
            () = tokio::time::sleep(IDLE) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().expect("address")
    }

    #[test]
    fn only_wholly_public_hosts_on_web_ports_pass() {
        assert_eq!(decide(443, &[ip("93.184.216.34")]), Ok(ip("93.184.216.34")));
        assert_eq!(decide(80, &[ip("2606:4700::1")]), Ok(ip("2606:4700::1")));
        assert_eq!(
            decide(8080, &[ip("93.184.216.34")]),
            Err("only ports 80 and 443 are allowed")
        );
        assert_eq!(decide(443, &[]), Err("the name does not resolve"));
        for internal in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "100.87.9.65",
            "169.254.169.254",
            "0.0.0.0",
            "224.0.0.1",
            "::1",
            "fe80::1",
            "fd7a:115c:a1e0::1",
            "::ffff:127.0.0.1",
            "ff02::1",
            "192.0.2.1",
        ] {
            assert_eq!(
                decide(443, &[ip("93.184.216.34"), ip(internal)]),
                Err("it resolves to a private or internal address"),
                "{internal}"
            );
        }
        assert!(public(ip("::ffff:93.184.216.34")));
    }

    #[tokio::test]
    async fn the_proxy_refuses_internal_targets_and_names_the_reason() {
        let guard = Guard::start().await.expect("guard");
        for request in [
            "CONNECT 127.0.0.1:443 HTTP/1.1\r\nHost: 127.0.0.1:443\r\n\r\n",
            "GET http://localhost/ HTTP/1.1\r\nHost: localhost\r\n\r\n",
            "CONNECT example.com:22 HTTP/1.1\r\n\r\n",
        ] {
            let mut client = TcpStream::connect(("127.0.0.1", guard.port()))
                .await
                .expect("connect");
            client.write_all(request.as_bytes()).await.expect("send");
            let mut reply = Vec::new();
            client.read_to_end(&mut reply).await.expect("reply");
            assert!(reply.starts_with(b"HTTP/1.1 403 Forbidden"), "{request}");
        }
        assert_eq!(
            guard.reason("127.0.0.1").as_deref(),
            Some("it resolves to a private or internal address")
        );
        assert_eq!(
            guard.reason("example.com").as_deref(),
            Some("only ports 80 and 443 are allowed")
        );
        let mut client = TcpStream::connect(("127.0.0.1", guard.port()))
            .await
            .expect("connect");
        client
            .write_all(b"GET ftp://example.com/ HTTP/1.1\r\n\r\n")
            .await
            .expect("send");
        let mut reply = Vec::new();
        client.read_to_end(&mut reply).await.expect("reply");
        assert!(reply.starts_with(b"HTTP/1.1 400 Bad Request"));
        for index in 0..70 {
            guard.refused(&format!("h{index}.test"), "r");
        }
        assert!(guard.reason("h0.test").is_none());
        assert!(guard.reason("h69.test").is_some());
    }
}
