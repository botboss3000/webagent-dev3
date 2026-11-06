//! `shell` and `python` in a session workspace, ported from `worker.py` `command` and the
//! Codebase Admin runner's command route. Each command runs under its own supervisor
//! ([`crate::runner`], `webagent-v3 --tool-runner`) in a new session, with the
//! workspace as home and temporary directory, its claimed preview ports, merged output
//! capped at 1 MiB and a timeout of 1–600 seconds.
use crate::files::{self, Roots};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::fd::OwnedFd;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

/// The tools this module runs.
pub const TOOLS: [&str; 2] = ["shell", "python"];
const LIMIT: usize = 1024 * 1024;
/// Commands get their own preview ports from this range, four per workspace.
const PREVIEW_PORTS: std::ops::RangeInclusive<u16> = 20000..=29999;

/// Whether this module runs `name`.
#[must_use]
pub fn handles(name: &str) -> bool {
    TOOLS.contains(&name)
}

/// Runs one `shell` or `python` request. Dropping the future (the caller hung up) stops
/// the command: SIGTERM to its session, then SIGKILL five seconds later.
pub async fn call(roots: &Roots, runner: &[String], request: &Value) -> Value {
    let (base, name, args) = {
        let roots = roots.clone();
        let request = request.clone();
        let admitted = tokio::task::spawn_blocking(move || {
            files::admit(&roots, &request)
                .map(|(base, name, args)| (base, name.to_owned(), args.clone()))
                .map_err(|failure| files::failed(&failure))
        })
        .await;
        match admitted {
            Ok(Ok(admitted)) => admitted,
            Ok(Err(failed)) => return failed,
            Err(_) => return json!({"status":"failed","output":"Command admission failed"}),
        }
    };
    let argv = match plan(&name, &args) {
        Ok(argv) => argv,
        Err(output) => {
            return json!({"status":"failed","output":output,
                "implementation_version":files::IMPLEMENTATION_VERSION,"plugin_revision":null})
        }
    };
    let timeout = request
        .get("timeout_seconds")
        .map_or(Some(120), Value::as_i64)
        .map(|seconds| seconds.clamp(1, 600));
    let Some(timeout) = timeout else {
        return json!({"status":"failed","output":"timeout_seconds must be a whole number"});
    };
    let ports = {
        let claims = roots.port_claims.clone();
        let base = base.clone();
        tokio::task::spawn_blocking(move || network_ports(&claims, &base)).await
    };
    let ports = match ports {
        Ok(Ok(ports)) => ports,
        Ok(Err(error)) => return json!({"status":"failed","output":error.to_string()}),
        Err(_) => return json!({"status":"failed","output":"Port claim failed"}),
    };
    match run(runner, &argv, &base, &ports, timeout as u64).await {
        Ok(result) => result,
        Err(error) => json!({"status":"failed","output":error.to_string()}),
    }
}

/// The runner's command route: the argv a call runs, or why it is refused.
fn plan(name: &str, args: &Value) -> Result<Vec<String>, String> {
    let Value::Object(args) = args else {
        return Err("Unsupported Codebase Admin contract".into());
    };
    if args.get("task_id").is_some_and(|task| !task.is_null()) {
        return Err("Task scope unsupported".into());
    }
    let (key, prefix): (&str, &[&str]) = match name {
        "shell" => ("command", &["/bin/sh", "-c"]),
        _ => ("code", &["/usr/bin/python3", "-I", "-c"]),
    };
    let text = args
        .get(key)
        .ok_or_else(|| format!("'{key}'"))?
        .as_str()
        .ok_or_else(|| format!("{key} must be text"))?;
    Ok(prefix
        .iter()
        .map(|part| (*part).to_owned())
        .chain([text.to_owned()])
        .collect())
}

/// The workspace's four preview ports, claimed once in the shared claim directory (the
/// Python worker's terminals read the same files) under an exclusive lock.
fn network_ports(directory: &Path, base: &Path) -> io::Result<Vec<u16>> {
    let key = std::fs::canonicalize(base)?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    let lock = OpenOptions::new()
        .append(true)
        .create(true)
        .open(directory.join("lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)?;
    let digest = hex::encode(Sha256::digest(key.to_string_lossy().as_bytes()));
    let path = directory.join(format!("{digest}.json"));
    let invalid = || io::Error::new(io::ErrorKind::InvalidData, "invalid workspace port claims");
    if path.exists() {
        let ports: Vec<u16> =
            serde_json::from_slice(&std::fs::read(&path)?).map_err(|_| invalid())?;
        if ports.len() != 4 || ports.iter().any(|port| !PREVIEW_PORTS.contains(port)) {
            return Err(invalid());
        }
        return Ok(ports);
    }
    let mut claimed = BTreeSet::new();
    for entry in std::fs::read_dir(directory)?.flatten() {
        if entry
            .path()
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let ports: Vec<u16> =
                serde_json::from_slice(&std::fs::read(entry.path())?).map_err(|_| invalid())?;
            claimed.extend(ports);
        }
    }
    let ports: Vec<u16> = PREVIEW_PORTS
        .filter(|port| !claimed.contains(port) && bindable(*port))
        .take(4)
        .collect();
    if ports.len() != 4 {
        return Err(io::Error::other("workspace preview ports exhausted"));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW)
        .mode(0o600)
        .open(&path)?;
    file.write_all(serde_json::to_string(&ports)?.as_bytes())?;
    file.sync_all()?;
    Ok(ports)
}

/// Whether nothing listens on `127.0.0.1:port` now (no address reuse, as Python's bind).
fn bindable(port: u16) -> bool {
    // SAFETY: a plain socket/bind/close on a descriptor this function owns.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return false;
        }
        let address = libc::sockaddr_in {
            sin_family: libc::AF_INET as libc::sa_family_t,
            sin_port: port.to_be(),
            sin_addr: libc::in_addr {
                s_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(),
            },
            sin_zero: [0; 8],
        };
        let bound = libc::bind(
            fd,
            (&address as *const libc::sockaddr_in).cast(),
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        ) == 0;
        libc::close(fd);
        bound
    }
}

/// Stops a started supervisor's session if the call ends before it does.
struct Session {
    child: Option<tokio::process::Child>,
    group: i32,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(child) = self.child.take() {
            stop_in_background(child, self.group);
        }
    }
}

/// Stops a supervisor's session in the background: SIGTERM now, then SIGKILL once the
/// supervisor exits or after five seconds.
pub(crate) fn stop_in_background(mut child: tokio::process::Child, group: i32) {
    {
        // SAFETY: killpg takes plain integers; the unreaped child keeps the group's id.
        unsafe {
            libc::killpg(group, libc::SIGTERM);
        }
        // Reaped here, so the group id cannot be reused before the final SIGKILL.
        std::thread::spawn(move || {
            for _ in 0..500 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            // SAFETY: as above; ESRCH once everything has gone is expected.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
            let _ = child.start_kill();
            for _ in 0..500 {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        });
    }
}

async fn run(
    runner: &[String],
    argv: &[String],
    base: &Path,
    ports: &[u16],
    timeout: u64,
) -> io::Result<Value> {
    let (reader, writer) = std::io::pipe()?;
    let ports: Vec<String> = ports.iter().map(u16::to_string).collect();
    let (program, leading) = runner
        .split_first()
        .ok_or_else(|| io::Error::other("command supervisor not configured"))?;
    let mut command = tokio::process::Command::new(program);
    command
        .args(leading)
        .arg(serde_json::to_string(argv)?)
        .current_dir(base)
        .env_clear()
        .env("PATH", "/opt/rust/bin:/usr/local/bin:/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("HOME", base)
        .env("TMPDIR", base)
        .env("CARGO_HOME", base.join(".cargo"))
        .env("CARGO_BUILD_JOBS", "1")
        .env("CARGO_INCREMENTAL", "0")
        .env("RUSTFLAGS", "-C debuginfo=0")
        .env("V3_ALLOWED_LISTEN_PORTS", ports.join(","))
        .stdin(Stdio::null())
        .stdout(Stdio::from(writer.try_clone()?))
        .stderr(Stdio::from(writer));
    // SAFETY: setsid is async-signal-safe and touches no memory.
    unsafe {
        command.as_std_mut().pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The command's own copies of the write end are the only ones left.
    drop(command);
    let group = child
        .id()
        .and_then(|pid| i32::try_from(pid).ok())
        .ok_or_else(|| io::Error::other("command supervisor exited at once"))?;
    let mut session = Session {
        child: Some(child),
        group,
    };
    let mut output = tokio::net::unix::pipe::Receiver::from_owned_fd(OwnedFd::from(reader))?;
    let mut data = Vec::new();
    let mut truncated = false;
    let drain = async {
        let mut chunk = vec![0_u8; 65536];
        loop {
            let read = output.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            let room = LIMIT.saturating_sub(data.len());
            data.extend_from_slice(&chunk[..read.min(room)]);
            truncated |= read > room;
        }
        match session.child.as_mut() {
            Some(child) => child.wait().await,
            None => Err(io::Error::other("command supervisor lost")),
        }
    };
    let (status, exit_code) = match tokio::time::timeout(Duration::from_secs(timeout), drain).await
    {
        Ok(waited) => {
            let waited = waited?;
            session.child = None;
            let code = exit_code(waited);
            (if code == Some(0) { "success" } else { "failed" }, code)
        }
        Err(_) => {
            let code = terminate(&mut session).await;
            ("timeout", code)
        }
    };
    // Kill descendants even if the immediate supervisor exited.
    // SAFETY: killpg takes plain integers; ESRCH once everything has gone is expected.
    unsafe {
        libc::killpg(group, libc::SIGKILL);
    }
    Ok(
        json!({"status":status,"output":String::from_utf8_lossy(&data),
        "exit_code":exit_code,"truncated":truncated}),
    )
}

/// The exit code as asyncio reports it: negative for a signal.
fn exit_code(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
}

/// SIGTERM to the session, five seconds to stop, then SIGKILL.
async fn terminate(session: &mut Session) -> Option<i32> {
    let mut child = session.child.take()?;
    // SAFETY: killpg takes plain integers.
    unsafe {
        libc::killpg(session.group, libc::SIGTERM);
    }
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(status) => status.ok(),
        Err(_) => {
            // SAFETY: as above.
            unsafe {
                libc::killpg(session.group, libc::SIGKILL);
            }
            child.wait().await.ok()
        }
    };
    status.and_then(exit_code)
}
