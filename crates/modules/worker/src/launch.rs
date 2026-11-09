//! The web container's launcher (`webagent-v3 --launch`), ported from `launch.py`, which
//! now only hands over to it. A short privileged setup creates the protected directories,
//! control files and the promotion socket; the promotion broker, the tool worker and the
//! API then run under restricted identities, and the launcher itself
//! drops to the application identity. The separate loop and gate containers have their
//! own entry points.
use std::ffi::CString;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const APP_UID: u32 = 1100;
const APP_GID: u32 = 1100;
/// The checkout owner; the promotion broker keeps this identity.
const OWNER_UID: u32 = 1000;
/// The tool worker's own identity (`webagent-worker`), distinct from any host login.
const WORKER_UID: u32 = 1103;
const APP_BINARY: &str = "/usr/local/bin/webagent-v3";
const PROMOTE_SOCKET: &str = "/runtime/promote/promote.sock";
/// The worker runs as `webagent-worker` (1103) in the app group, with only the
/// `webagent-source` group (1110) added so checkout_edit can write the checkout, and no
/// capabilities: application-only files such as the encryption key stay unreadable to tools
/// and no host login's files are reachable.
const WORKER_IDENTITY: [&str; 8] = [
    "--reuid=1103",
    "--regid=1100",
    "--groups=1110",
    "--inh-caps=-all",
    "--ambient-caps=-all",
    "--no-new-privs",
    "--",
    "",
];

static STOPPING: AtomicBool = AtomicBool::new(false);

extern "C" fn stop(_signal: libc::c_int) {
    STOPPING.store(true, Ordering::SeqCst);
}

fn failed(message: &str) -> io::Error {
    io::Error::other(message.to_owned())
}

fn path(text: &str) -> io::Result<CString> {
    CString::new(text).map_err(|_| failed("path contains a NUL byte"))
}

fn chown(target: &str, uid: u32, gid: u32) -> io::Result<()> {
    let target = path(target)?;
    // SAFETY: a valid NUL-terminated path and plain integers.
    if unsafe { libc::chown(target.as_ptr(), uid, gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn chmod(target: &str, mode: u32) -> io::Result<()> {
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(mode))
}

/// Creates `target` if needed, then sets its owner (group 1100) and mode.
fn directory(target: &str, uid: u32, mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    chown(target, uid, APP_GID)?;
    chmod(target, mode)
}

/// Drops to `uid`/`gid` with no supplementary groups and no privilege gain on exec. Only
/// raw system calls, so it is safe between fork and exec.
fn drop_to(uid: u32, gid: u32) -> io::Result<()> {
    // SAFETY: plain system calls with valid arguments.
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0
            || libc::setgid(gid) != 0
            || libc::setuid(uid) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

fn exited(child: &mut Child) -> bool {
    !matches!(child.try_wait(), Ok(None))
}

/// Waits up to five seconds for `socket` to appear while `process` and the broker run.
/// Checks `socket` with the worker's effective identity: the worker folders are
/// private to that owner, and the launcher keeps no capability to read past file modes.
fn exists_as_owner(socket: &str) -> io::Result<bool> {
    // SAFETY: seteuid takes a plain integer; the launcher is root here and returns to it.
    if unsafe { libc::seteuid(WORKER_UID) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let found = Path::new(socket).exists();
    // SAFETY: as above; the saved set-user-ID is still root.
    if unsafe { libc::seteuid(0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(found)
}

fn wait_for(socket: &str, process: &mut Child, broker: &mut Child, name: &str) -> io::Result<()> {
    for _ in 0..100 {
        if exists_as_owner(socket)? {
            return Ok(());
        }
        if exited(process) {
            return Err(failed(&format!("{name} failed")));
        }
        if exited(broker) {
            return Err(failed("promotion broker failed"));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: kill takes plain integers.
    unsafe {
        libc::kill(process.id() as i32, libc::SIGTERM);
    }
    Err(failed(&format!("{name} startup timed out")))
}

fn worker(program: &[&str], env: &[(&str, &str)]) -> io::Result<Child> {
    let mut identity = WORKER_IDENTITY;
    identity[7] = program[0];
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args(identity)
        .args(&program[1..])
        .env_clear()
        .envs(env.iter().copied());
    command.spawn()
}

/// Runs the launcher and returns the container's exit code.
#[must_use]
pub fn run() -> i32 {
    match launch() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("launcher: {error}");
            1
        }
    }
}

fn launch() -> io::Result<i32> {
    // Preserve worker access to live checkout data across startup. Other tenant profiles
    // keep private modes beneath the listable state root.
    directory("/state", APP_UID, 0o770)?;
    directory("/workspaces", 1101, 0o770)?;
    for folder in [
        "/runtime",
        "/runtime/control",
        "/runtime/worker",
        "/runtime/promote",
    ] {
        directory(folder, 0, 0o700)?;
    }
    for (name, content) in [("dispatch.lock", ""), ("shutoff", "1")] {
        let file = format!("/runtime/control/{name}");
        if Path::new(&file).exists() {
            chown(&file, 0, 0)?;
        }
        std::fs::write(&file, content)?;
        chmod(&file, 0o640)?;
    }
    let owner: u32 = std::env::var("V3_SOURCE_OWNER_UID")
        .unwrap_or_else(|_| OWNER_UID.to_string())
        .parse()
        .map_err(|_| failed("invalid V3_SOURCE_OWNER_UID"))?;
    let source = std::fs::symlink_metadata("/live-v3");
    if !source.is_ok_and(|meta| meta.is_dir() && meta.uid() == owner) {
        return Err(failed("live v3 source mount unavailable"));
    }
    match std::fs::remove_file(PROMOTE_SOCKET) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    let listener = UnixListener::bind(PROMOTE_SOCKET)?;
    chown(PROMOTE_SOCKET, APP_UID, APP_GID)?;
    chmod(PROMOTE_SOCKET, 0o600)?;
    // SAFETY: re-listening on an owned socket only changes its backlog to launch.py's 8.
    if unsafe { libc::listen(listener.as_raw_fd(), 8) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let lock = File::open("/runtime/control/dispatch.lock")?;
    let shutoff = File::open("/runtime/control/shutoff")?;
    for name in ["dispatch.lock", "shutoff"] {
        chown(&format!("/runtime/control/{name}"), APP_UID, APP_GID)?;
    }
    directory("/runtime/control", APP_UID, 0o750)?;
    directory("/runtime/worker", WORKER_UID, 0o750)?;
    directory("/runtime/promote", APP_UID, 0o700)?;
    directory("/runtime", APP_UID, 0o755)?;

    // The broker keeps only the checkout owner's identity and these inherited handles.
    let handed: [RawFd; 3] = [listener.as_raw_fd(), lock.as_raw_fd(), shutoff.as_raw_fd()];
    let mut broker = Command::new("/usr/bin/python3");
    broker
        .args(["-I", "/opt/v3/promote.py"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("V3_LIVE_SOURCE_ROOT", "/live-v3")
        .env("V3_PROMOTE_LISTENER_FD", handed[0].to_string())
        .env("V3_PROMOTE_LOCK_FD", handed[1].to_string())
        .env("V3_PROMOTE_SHUTOFF_FD", handed[2].to_string());
    // SAFETY: fcntl, prctl and the identity calls are raw system calls on integers.
    unsafe {
        broker.pre_exec(move || {
            for fd in handed {
                if libc::fcntl(fd, libc::F_SETFD, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0);
            drop_to(OWNER_UID, OWNER_UID)
        });
    }
    let mut broker = broker.spawn()?;
    drop((listener, lock, shutoff));
    if exited(&mut broker) {
        return Err(failed("promotion broker failed"));
    }
    // SAFETY: plain system calls; the launcher keeps root only to start the worker.
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(APP_GID) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let mut tools = worker(
        &[APP_BINARY, "--tool-worker"],
        &[
            ("PATH", "/usr/bin:/bin"),
            ("LANG", "C.UTF-8"),
            ("V3_TOOL_SOCKET", "/runtime/worker/tools.sock"),
            ("V3_TOOL_CLIENT_UID", "1100"),
        ],
    )?;
    wait_for(
        "/runtime/worker/tools.sock",
        &mut tools,
        &mut broker,
        "tool worker",
    )?;
    // The API keeps the container environment, as before.
    let mut app = Command::new(APP_BINARY);
    // SAFETY: drop_to only makes raw system calls.
    unsafe {
        app.pre_exec(|| drop_to(APP_UID, APP_GID));
    }
    let mut app = app.spawn()?;
    // The launcher itself drops to the application identity; it keeps no root privileges.
    drop_to(APP_UID, APP_GID)?;
    // SAFETY: the handler only stores to an atomic, which is async-signal-safe.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = stop as extern "C" fn(libc::c_int) as usize;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
    let app_pid = app.id() as i32;
    let terminate_app = || {
        // SAFETY: kill takes plain integers.
        unsafe {
            libc::kill(app_pid, libc::SIGTERM);
        }
    };
    loop {
        // Container init delivers SIGTERM here; the API has its own identity and drains.
        if STOPPING.swap(false, Ordering::SeqCst) {
            terminate_app();
        }
        if exited(&mut app) || exited(&mut tools) || exited(&mut broker) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !exited(&mut app) {
        terminate_app();
    }
    // As launch.py: the first process that ended with a non-zero code decides the exit.
    let code = [&mut app, &mut tools, &mut broker]
        .into_iter()
        .filter_map(|child| child.try_wait().ok().flatten())
        .map(|status| {
            status
                .code()
                .unwrap_or_else(|| -status.signal().unwrap_or(0))
        })
        .find(|code| *code != 0)
        .unwrap_or(1);
    Ok(code & 0xff)
}
