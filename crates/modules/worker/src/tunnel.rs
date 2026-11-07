//! Ephemeral authenticated workspace previews (`start_tunnel`, `get_tunnel_url`), ported
//! from the Administrator `remote_tunnel` adapter. A lease runs a quick `cloudflared`
//! tunnel to a private proxy that serves only a port a terminal of the same workspace
//! listens on, behind a one-time token cookie, for at most 30 minutes. The permanent
//! development tunnel is untouched.
use serde_json::{json, Value};
use std::collections::HashMap;
use std::future::Future;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore};

/// The tools this module runs.
pub const TOOLS: [&str; 2] = ["start_tunnel", "get_tunnel_url"];
const LIMIT: usize = 1024 * 1024;
/// The preview cookie; `__Host-` keeps it to this origin, secure and path `/`.
pub const COOKIE: &str = "__Host-v3_preview";
const BLOCKED: [u16; 3] = [8080, 8081, 8082];
const HOP: [&str; 8] = [
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];
const REJECTED: &str = "Authenticated workspace preview unavailable or invalid; no automatic retry";

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Decides whether `port` belongs to the workspace `base` right now.
pub type Validator =
    Arc<dyn Fn(PathBuf, u16) -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

type Failure = Box<dyn std::error::Error + Send + Sync>;

fn invalid(message: &str) -> Failure {
    message.into()
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let mut filled = 0;
    while filled < bytes {
        // SAFETY: getrandom writes at most the remaining length into an owned buffer.
        let got =
            unsafe { libc::getrandom(buffer[filled..].as_mut_ptr().cast(), bytes - filled, 0) };
        if got > 0 {
            filled += got as usize;
        }
    }
    hex::encode(buffer)
}

/// One preview: its private proxy, the connector process and the access token.
pub struct Lease {
    base: PathBuf,
    port: u16,
    id: String,
    token: String,
    url: StdMutex<Option<String>>,
    expires: Instant,
    closed: StdMutex<bool>,
    connections: Semaphore,
    requests: StdMutex<u32>,
    connector: StdMutex<Option<i32>>,
    profile: StdMutex<Option<PathBuf>>,
    tasks: StdMutex<Vec<tokio::task::AbortHandle>>,
    validator: Validator,
}

fn locked<T>(mutex: &StdMutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Lease {
    fn closed(&self) -> bool {
        *locked(&self.closed) || Instant::now() >= self.expires
    }

    fn public(&self) -> Value {
        let open = !*locked(&self.closed);
        let url = locked(&self.url)
            .as_ref()
            .filter(|_| open)
            .map(|url| format!("{url}/{}/", self.token));
        json!({"lease_id":self.id,"status":if open {"active"} else {"closed"},"url":url,
            "expires_in":self.expires.saturating_duration_since(Instant::now()).as_secs(),
            "authenticated_preview":true})
    }

    /// Stops the proxy and connector and removes the connector's private profile.
    async fn close(&self) {
        {
            let mut closed = locked(&self.closed);
            if *closed {
                return;
            }
            *closed = true;
        }
        for task in locked(&self.tasks).drain(..) {
            task.abort();
        }
        let connector = locked(&self.connector).take();
        if let Some(group) = connector {
            kill(group).await;
        }
        if let Some(profile) = locked(&self.profile).take() {
            let _ = std::fs::remove_dir_all(profile);
        }
    }

    async fn allowed(&self) -> bool {
        !BLOCKED.contains(&self.port) && (self.validator)(self.base.clone(), self.port).await
    }

    fn authenticated(&self, cookie: Option<&String>) -> bool {
        cookie.is_some_and(|cookie| {
            cookie.split(';').any(|pair| {
                pair.trim().split_once('=').is_some_and(|(name, value)| {
                    name == COOKIE && constant_eq(value.trim().as_bytes(), self.token.as_bytes())
                })
            })
        })
    }

    async fn client(self: Arc<Self>, mut stream: TcpStream) {
        let Ok(_permit) =
            tokio::time::timeout(Duration::from_millis(100), self.connections.acquire()).await
        else {
            let _ = respond(&mut stream, 503, b"", &[]).await;
            return;
        };
        let outcome = self.proxy(&mut stream).await;
        if outcome.is_err() {
            let _ = respond(&mut stream, 502, b"Workspace preview request rejected", &[]).await;
        }
        let _ = stream.shutdown().await;
    }

    async fn proxy(&self, stream: &mut TcpStream) -> Result<(), Failure> {
        let over = {
            let mut requests = locked(&self.requests);
            let over = *requests >= 10_000;
            *requests += 1;
            over
        };
        if self.closed() || over {
            return respond(stream, 410, b"", &[]).await;
        }
        let (read, mut write) = stream.split();
        let mut reader = BufReader::new(read);
        let (first, values) = headers(&mut reader).await?;
        let parts: Vec<&str> = first.split(' ').collect();
        if parts.len() != 3 || !matches!(parts[2], "HTTP/1.0" | "HTTP/1.1") {
            return Err(invalid("Invalid request"));
        }
        let (method, target) = (parts[0], parts[1]);
        if !matches!(
            method,
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        ) || !target.starts_with('/')
            || target.starts_with("//")
            || target.chars().any(|c| (c as u32) < 32)
        {
            return Err(invalid("Invalid preview request"));
        }
        // Target ownership is renewed even for token bootstrap; approval does not freeze it.
        if !self.allowed().await {
            respond(&mut write, 410, b"", &[]).await?;
            self.close().await;
            return Ok(());
        }
        if target == format!("/{}/", self.token) && method == "GET" {
            let age = self
                .expires
                .saturating_duration_since(Instant::now())
                .as_secs()
                .max(1);
            let cookie = format!(
                "{COOKIE}={}; Secure; HttpOnly; SameSite=Strict; Path=/; Max-Age={age}",
                self.token
            );
            return respond(
                &mut write,
                302,
                b"",
                &[
                    ("Location".into(), "/".into()),
                    ("Set-Cookie".into(), cookie),
                ],
            )
            .await;
        }
        if !self.authenticated(values.get("cookie")) {
            return respond(&mut write, 403, b"", &[]).await;
        }
        if values.contains_key("transfer-encoding") {
            return Err(invalid("Chunked requests unavailable"));
        }
        let size: usize = values
            .get("content-length")
            .map_or(Ok(0), |size| size.trim().parse())?;
        if size > LIMIT {
            return respond(&mut write, 413, b"", &[]).await;
        }
        let mut body = vec![0; size];
        tokio::time::timeout(Duration::from_secs(10), reader.read_exact(&mut body)).await??;
        if !self.allowed().await {
            respond(&mut write, 410, b"", &[]).await?;
            self.close().await;
            return Ok(());
        }
        let mut upstream = tokio::time::timeout(
            Duration::from_secs(5),
            TcpStream::connect(("127.0.0.1", self.port)),
        )
        .await??;
        // Never forward browser/application authentication or hop-by-hop credentials.
        let named: Vec<String> = values
            .get("connection")
            .map(|v| v.split(',').map(|n| n.trim().to_lowercase()).collect())
            .unwrap_or_default();
        let mut request = format!(
            "{method} {target} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\nContent-Length: {}\r\n",
            self.port,
            body.len()
        );
        for (key, value) in &values {
            if !HOP.contains(&key.as_str())
                && !matches!(
                    key.as_str(),
                    "cookie" | "authorization" | "host" | "content-length"
                )
                && !named.contains(key)
            {
                request.push_str(&format!("{key}: {value}\r\n"));
            }
        }
        request.push_str("\r\n");
        let mut sent = latin1(&request);
        sent.extend_from_slice(&body);
        upstream.write_all(&sent).await?;
        let mut remote = BufReader::new(upstream);
        let (line, result) = headers(&mut remote).await?;
        let status = response_status(&line).ok_or_else(|| invalid("Invalid preview response"))?;
        let content = if method == "HEAD" || matches!(status, 204 | 304) {
            Vec::new()
        } else {
            read_body(&mut remote, &result).await?
        };
        let extra: Vec<(String, String)> = result
            .into_iter()
            .filter(|(key, _)| {
                !HOP.contains(&key.as_str())
                    && !matches!(
                        key.as_str(),
                        "content-length" | "set-cookie" | "cache-control"
                    )
            })
            .collect();
        respond(&mut write, status, &content, &extra).await
    }
}

fn constant_eq(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .fold(0u8, |difference, (a, b)| difference | (a ^ b))
            == 0
}

fn latin1(text: &str) -> Vec<u8> {
    text.chars().map(|c| c as u32 as u8).collect()
}

fn response_status(line: &str) -> Option<u16> {
    let rest = line
        .strip_prefix("HTTP/1.1 ")
        .or_else(|| line.strip_prefix("HTTP/1.0 "))?;
    let code = rest.get(..3)?;
    let tail = &rest[3..];
    let valid = code.as_bytes()[0].is_ascii_digit()
        && (b'1'..=b'5').contains(&code.as_bytes()[0])
        && code.bytes().all(|b| b.is_ascii_digit())
        && (tail.is_empty() || tail.starts_with(' '));
    valid.then(|| code.parse().ok()).flatten()
}

/// Reads a header block of at most 16 KiB within 10 seconds; names are lower-cased and
/// must be unique tokens, values free of control characters.
async fn headers(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
) -> Result<(String, Vec<(String, String)>), Failure> {
    let mut raw = Vec::new();
    let read = async {
        while !raw.ends_with(b"\r\n\r\n") {
            let before = raw.len();
            (&mut *reader)
                .take(16_385 - before as u64)
                .read_until(b'\n', &mut raw)
                .await?;
            if raw.len() == before || raw.len() > 16_384 {
                return Err(invalid("Headers exceed limit"));
            }
        }
        Ok::<_, Failure>(())
    };
    tokio::time::timeout(Duration::from_secs(10), read).await??;
    let text: String = raw[..raw.len() - 4].iter().map(|b| *b as char).collect();
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or_default().to_owned();
    let mut values: Vec<(String, String)> = Vec::new();
    for line in lines {
        let Some((key, value)) = line.split_once(':') else {
            return Err(invalid("Invalid repeated header"));
        };
        let key = key.to_lowercase();
        let token = !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+.^_|~-".contains(&b));
        if !token || values.iter().any(|(existing, _)| *existing == key) {
            return Err(invalid("Invalid repeated header"));
        }
        if value
            .chars()
            .any(|c| (c as u32) < 32 && c != '\t' || c as u32 == 127)
        {
            return Err(invalid("Invalid header value"));
        }
        values.push((key, value.trim().to_owned()));
    }
    Ok((first, values))
}

trait Headers {
    fn get(&self, key: &str) -> Option<&String>;
    fn contains_key(&self, key: &str) -> bool;
}

impl Headers for Vec<(String, String)> {
    fn get(&self, key: &str) -> Option<&String> {
        self.iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }
    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
}

async fn read_body(
    reader: &mut (impl tokio::io::AsyncBufRead + Unpin),
    values: &Vec<(String, String)>,
) -> Result<Vec<u8>, Failure> {
    let step = Duration::from_secs(10);
    if let Some(encoding) = values.get("transfer-encoding") {
        if encoding.to_lowercase() != "chunked" || values.contains_key("content-length") {
            return Err(invalid("Unsupported response framing"));
        }
        let mut out = Vec::new();
        loop {
            let mut line = Vec::new();
            tokio::time::timeout(step, reader.take(129).read_until(b'\n', &mut line)).await??;
            if line.len() > 128 {
                return Err(invalid("Chunk framing exceeds limit"));
            }
            let text = String::from_utf8_lossy(&line);
            let size = usize::from_str_radix(text.split(';').next().unwrap_or("").trim(), 16)?;
            if out.len() + size > LIMIT {
                return Err(invalid("Response exceeds limit"));
            }
            let mut end = [0u8; 2];
            if size == 0 {
                tokio::time::timeout(step, reader.read_exact(&mut end)).await??;
                if &end != b"\r\n" {
                    return Err(invalid("Response trailers unavailable"));
                }
                return Ok(out);
            }
            let mut chunk = vec![0; size];
            tokio::time::timeout(step, reader.read_exact(&mut chunk)).await??;
            out.extend_from_slice(&chunk);
            tokio::time::timeout(step, reader.read_exact(&mut end)).await??;
            if &end != b"\r\n" {
                return Err(invalid("Invalid chunk"));
            }
        }
    }
    if let Some(size) = values.get("content-length") {
        let size: usize = size.trim().parse()?;
        if size > LIMIT {
            return Err(invalid("Response exceeds limit"));
        }
        let mut body = vec![0; size];
        tokio::time::timeout(step, reader.read_exact(&mut body)).await??;
        return Ok(body);
    }
    let mut out = Vec::new();
    let mut chunk = vec![0; 16384];
    loop {
        let read = tokio::time::timeout(step, reader.read(&mut chunk)).await??;
        if read == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&chunk[..read]);
        if out.len() > LIMIT {
            return Err(invalid("Response exceeds limit"));
        }
    }
}

async fn respond(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    status: u16,
    body: &[u8],
    extra: &[(String, String)],
) -> Result<(), Failure> {
    let phrase = match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        403 => "Forbidden",
        410 => "Gone",
        413 => "Payload Too Large",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Response",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {phrase}\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n",
        body.len()
    );
    for (key, value) in extra {
        head.push_str(&format!("{key}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = latin1(&head);
    out.extend_from_slice(body);
    tokio::time::timeout(Duration::from_secs(10), writer.write_all(&out)).await??;
    Ok(())
}

/// Ends the connector's process group: SIGTERM, then SIGKILL after three seconds.
async fn kill(group: i32) {
    // SAFETY: killpg takes plain integers; the group leader is reaped by its own task.
    unsafe {
        libc::killpg(group, libc::SIGTERM);
    }
    for _ in 0..30 {
        // SAFETY: signal 0 only checks that the group still exists.
        if unsafe { libc::killpg(group, 0) } != 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // SAFETY: as above.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
}

/// The first quick-tunnel URL in `output`, as `https://NAME.trycloudflare.com` followed by
/// whitespace, `/`, `|` or the end of what was read.
fn tunnel_url(output: &[u8]) -> Option<String> {
    const SUFFIX: &[u8] = b".trycloudflare.com";
    let mut from = 0;
    while let Some(at) = output[from..]
        .windows(8)
        .position(|window| window == b"https://")
    {
        let start = from + at + 8;
        let run = output[start..]
            .iter()
            .take_while(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.')
            })
            .count();
        let host = &output[start..start + run];
        let next = output.get(start + run);
        if host.ends_with(SUFFIX)
            && next.is_none_or(|b| b.is_ascii_whitespace() || matches!(b, b'/' | b'|'))
        {
            let name = &host[..host.len() - SUFFIX.len()];
            let valid = !name.is_empty()
                && name.split(|b| *b == b'-').all(|label| !label.is_empty())
                && !name.contains(&b'.');
            if valid {
                return Some(format!("https://{}", String::from_utf8_lossy(host)));
            }
        }
        from = start;
    }
    None
}

/// Every workspace's lease, with the connector and the ownership check they use.
pub struct Tunnels {
    leases: StdMutex<HashMap<PathBuf, Arc<Lease>>>,
    locks: StdMutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    executable: PathBuf,
    validator: Validator,
}

impl Tunnels {
    #[must_use]
    pub fn new(executable: PathBuf, validator: Validator) -> Self {
        Self {
            leases: StdMutex::default(),
            locks: StdMutex::default(),
            executable,
            validator,
        }
    }

    /// The workspace's lease, for tests.
    #[must_use]
    pub fn lease(&self, base: &Path) -> Option<Arc<Lease>> {
        locked(&self.leases).get(base).cloned()
    }

    /// Open preview tunnels in this workspace; trusted worker status only.
    #[must_use]
    pub fn count(&self, base: &Path) -> usize {
        usize::from(locked(&self.leases).contains_key(base))
    }

    /// Closes the workspace's lease, if any.
    pub async fn close_workspace(&self, base: &Path) {
        let lease = locked(&self.leases).remove(base);
        if let Some(lease) = lease {
            lease.close().await;
        }
    }

    /// Runs one tunnel tool; every refusal is the same uninformative error.
    pub async fn execute(&self, name: &str, args: &Value, base: &Path) -> Value {
        let lock = Arc::clone(locked(&self.locks).entry(base.to_owned()).or_default());
        let _held = lock.lock().await;
        match self.run(name, args, base).await {
            Ok(output) => json!({"status":"success","output":output}),
            Err(_) => {
                json!({"status":"error","error_kind":"preview_tunnel_rejected","output":REJECTED})
            }
        }
    }

    async fn run(&self, name: &str, args: &Value, base: &Path) -> Result<Value, Failure> {
        let Value::Object(args) = args else {
            return Err(invalid("Typed tunnel arguments required"));
        };
        let current = self.lease(base);
        let current = match current {
            Some(lease) if lease.closed() => {
                lease.close().await;
                locked(&self.leases).remove(base);
                None
            }
            current => current,
        };
        if name == "get_tunnel_url" {
            if !args.is_empty() {
                return Err(invalid("Tunnel status accepts an empty object"));
            }
            return Ok(current.map_or_else(
                || json!({"status":"absent","url":null,"reason":"No active lease; worker restart never replays a tunnel"}),
                |lease| lease.public(),
            ));
        }
        if name != "start_tunnel"
            || args
                .keys()
                .any(|key| key != "port" && key != "duration_seconds")
        {
            return Err(invalid("Invalid preview start"));
        }
        let port = args
            .get("port")
            .and_then(Value::as_u64)
            .filter(|port| (1024..=65535).contains(port))
            .ok_or_else(|| invalid("Preview port or duration exceeds limit"))?;
        let duration = args
            .get("duration_seconds")
            .map_or(Some(300), Value::as_u64)
            .filter(|seconds| (30..=1800).contains(seconds))
            .ok_or_else(|| invalid("Preview port or duration exceeds limit"))?;
        if current.is_some() {
            return Err(invalid("Workspace already has an active preview lease"));
        }
        let lease = Arc::new(Lease {
            base: base.to_owned(),
            port: port as u16,
            id: random_hex(16),
            token: random_hex(32),
            url: StdMutex::new(None),
            expires: Instant::now() + Duration::from_secs(duration),
            closed: StdMutex::new(false),
            connections: Semaphore::new(8),
            requests: StdMutex::new(0),
            connector: StdMutex::new(None),
            profile: StdMutex::new(None),
            tasks: StdMutex::new(Vec::new()),
            validator: Arc::clone(&self.validator),
        });
        if !lease.allowed().await {
            return Err(invalid("Port is not owned by this workspace terminal"));
        }
        // Closes the half-started lease if this call fails or its caller hangs up.
        struct Guard(Option<Arc<Lease>>);
        impl Drop for Guard {
            fn drop(&mut self) {
                if let Some(lease) = self.0.take() {
                    tokio::spawn(async move { lease.close().await });
                }
            }
        }
        let mut guard = Guard(Some(Arc::clone(&lease)));
        let result = self.start(&lease).await?;
        guard.0 = None;
        locked(&self.leases).insert(base.to_owned(), lease);
        Ok(result)
    }

    async fn start(&self, lease: &Arc<Lease>) -> Result<Value, Failure> {
        let meta = std::fs::symlink_metadata(&self.executable)?;
        if !meta.is_file() {
            return Err(invalid("Installed cloudflared CLI unavailable"));
        }
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let proxy_port = listener.local_addr()?.port();
        let serving = Arc::clone(lease);
        let server = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(Arc::clone(&serving).client(stream));
            }
        });
        locked(&lease.tasks).push(server.abort_handle());
        let profile = std::env::temp_dir().join(format!("v3-private-preview-{}", random_hex(8)));
        std::fs::DirBuilder::new().mode(0o700).create(&profile)?;
        *locked(&lease.profile) = Some(profile.clone());
        // SAFETY: getpid has no preconditions.
        let parent = unsafe { libc::getpid() };
        let mut command = tokio::process::Command::new(&self.executable);
        command
            .args([
                "tunnel",
                "--url",
                &format!("http://127.0.0.1:{proxy_port}"),
                "--no-autoupdate",
            ])
            .current_dir(&profile)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("HOME", &profile)
            .env("TMPDIR", &profile)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // SAFETY: only async-signal-safe calls between fork and exec.
        unsafe {
            command.pre_exec(move || {
                // Output and errors share one stream; the worker's death ends the connector.
                libc::dup2(1, 2);
                libc::setsid();
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::getppid() != parent {
                    libc::_exit(125);
                }
                Ok(())
            });
        }
        let mut child = command.spawn()?;
        let group = child.id().map_or(0, |pid| pid as i32);
        *locked(&lease.connector) = Some(group);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| invalid("Connector output"))?;
        let mut output: Vec<u8> = Vec::new();
        let wait_url = async {
            let mut chunk = vec![0; 4096];
            loop {
                let read = stdout.read(&mut chunk).await?;
                if read == 0 {
                    return Err(invalid("Cloudflare preview failed before URL"));
                }
                output.extend_from_slice(&chunk[..read]);
                if output.len() > 65536 {
                    return Err(invalid("Cloudflared startup output exceeds limit"));
                }
                if let Some(url) = tunnel_url(&output) {
                    return Ok(url);
                }
            }
        };
        let url = tokio::time::timeout(Duration::from_secs(60), wait_url).await??;
        tokio::task::yield_now().await;
        if !matches!(child.try_wait(), Ok(None)) || Instant::now() >= lease.expires {
            return Err(invalid("Preview expired before startup"));
        }
        *locked(&lease.url) = Some(url);
        let expiring = Arc::clone(lease);
        let expiry = tokio::spawn(async move {
            tokio::time::sleep(expiring.expires.saturating_duration_since(Instant::now())).await;
            expiring.close().await;
        });
        // The connector may print at most 64 KiB more; its exit ends the lease.
        let watched = Arc::clone(lease);
        let watch = tokio::spawn(async move {
            let mut total = 0;
            let mut chunk = vec![0; 4096];
            while let Ok(read) = stdout.read(&mut chunk).await {
                if read == 0 {
                    break;
                }
                total += read;
                if total > 65536 {
                    break;
                }
            }
            let _ = child.wait().await;
            watched.close().await;
        });
        locked(&lease.tasks).extend([expiry.abort_handle(), watch.abort_handle()]);
        Ok(lease.public())
    }
}

/// Whether a process of one of the workspace's running terminals listens on
/// `127.0.0.1:port` or `0.0.0.0:port`; `roots` are the terminals' supervisor ids.
#[must_use]
pub fn terminal_listens(roots: &[i32], port: u16) -> bool {
    let mut parents = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|n| n.parse::<i32>().ok())
            else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            if let Some(parent) = stat
                .rfind(')')
                .and_then(|at| stat[at + 2..].split_whitespace().nth(1))
                .and_then(|parent| parent.parse::<i32>().ok())
            {
                parents.push((pid, parent));
            }
        }
    }
    let mut owned: Vec<i32> = roots.to_vec();
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, parent) in &parents {
            if !owned.contains(pid) && owned.contains(parent) {
                owned.push(*pid);
                changed = true;
            }
        }
    }
    let Ok(table) = std::fs::read_to_string("/proc/net/tcp") else {
        return false;
    };
    let inodes: Vec<String> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let (address, local) = fields.get(1)?.split_once(':')?;
            let listening = fields.get(3) == Some(&"0A")
                && u16::from_str_radix(local, 16).ok() == Some(port)
                && matches!(address, "0100007F" | "00000000");
            listening.then(|| fields.get(9).map(|inode| (*inode).to_owned()))?
        })
        .collect();
    if inodes.is_empty() {
        return false;
    }
    owned.iter().any(|pid| {
        std::fs::read_dir(format!("/proc/{pid}/fd")).is_ok_and(|entries| {
            entries.flatten().any(|fd| {
                std::fs::read_link(fd.path()).is_ok_and(|target| {
                    let target = target.to_string_lossy();
                    target
                        .strip_prefix("socket:[")
                        .and_then(|rest| rest.strip_suffix(']'))
                        .is_some_and(|inode| inodes.iter().any(|i| i == inode))
                })
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_quick_tunnel_urls_are_taken() {
        let found = |text: &str| tunnel_url(text.as_bytes());
        assert_eq!(
            found("INF |  https://real-shaped-preview.trycloudflare.com  |").as_deref(),
            Some("https://real-shaped-preview.trycloudflare.com")
        );
        assert_eq!(found("https://a.b.trycloudflare.com/"), None);
        assert_eq!(found("https://-a.trycloudflare.com "), None);
        assert_eq!(found("https://a.trycloudflare.com.evil "), None);
        assert_eq!(found("no tunnel URL"), None);
    }

    #[test]
    fn response_lines_need_a_valid_status() {
        assert_eq!(response_status("HTTP/1.1 200 OK"), Some(200));
        assert_eq!(response_status("HTTP/1.0 404"), Some(404));
        assert_eq!(response_status("HTTP/1.1 600 X"), None);
        assert_eq!(response_status("HTTP/2 200"), None);
    }
}
