//! The web container's launcher (`webagent-v3 --launch`), ported from `launch.py`, which
//! now only hands over to it. A short privileged setup creates the protected directories
//! and control files; the tool worker and the API then run under restricted identities,
//! and the launcher itself
//! drops to the application identity. The separate loop and gate containers have their
//! own entry points.
use std::ffi::CString;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const APP_UID: u32 = 1100;
const APP_GID: u32 = 1100;
/// `webagent-work`: the tool worker's own group, shared with the app only for workspaces,
/// the dispatch control files and the worker's and browser's sockets (stream D1, DATA-5).
/// Nothing under `/state` belongs to it.
const WORK_GID: u32 = 1105;
/// The tool worker's own identity (`webagent-worker`), distinct from any host login.
const WORKER_UID: u32 = 1103;
const APP_BINARY: &str = "/usr/local/bin/webagent-v3";
/// The worker runs as `webagent-worker` (1103) in its own group `webagent-work` (1105) only,
/// with no capabilities: the app's state folder, the encryption key and account databases
/// stay out of reach of tools, it reads the checkout as any account may and writes neither
/// the checkout nor `.git` (the broker makes those changes, streams D2 and D3), and no host
/// login's files are reachable.
const WORKER_IDENTITY: [&str; 8] = [
    "--reuid=1103",
    "--regid=1105",
    "--clear-groups",
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

/// Creates `target` if needed, then sets its owner and mode.
fn directory(target: &str, uid: u32, gid: u32, mode: u32) -> io::Result<()> {
    std::fs::create_dir_all(target)?;
    chown(target, uid, gid)?;
    chmod(target, mode)
}

/// Gives the browser container's socket folder and its socket, when present, the worker's
/// group, keeping their owner (the browser identity) and mode. Never follows a symbolic link.
fn browser_socket_group(folder: &str, socket: &str) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    for (target, folder_expected) in [(folder, true), (socket, false)] {
        let kind = match std::fs::symlink_metadata(target) {
            Ok(meta) => meta.file_type(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if (folder_expected && !kind.is_dir()) || (!folder_expected && !kind.is_socket()) {
            return Err(failed("unexpected browser socket file type"));
        }
        let name = path(target)?;
        // SAFETY: a valid NUL-terminated path; uid -1 keeps the owner.
        if unsafe { libc::lchown(name.as_ptr(), libc::uid_t::MAX, WORK_GID) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Drops to `uid`/`gid` with only the supplementary `groups` and no privilege gain on exec.
/// Only raw system calls, so it is safe between fork and exec.
fn drop_to(uid: u32, gid: u32, groups: &[libc::gid_t]) -> io::Result<()> {
    // SAFETY: plain system calls with valid arguments; `groups` outlives the call.
    unsafe {
        if libc::setgroups(groups.len(), groups.as_ptr()) != 0
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

/// Waits up to five seconds for `socket` to appear while `process` runs.
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

fn wait_for(socket: &str, process: &mut Child, name: &str) -> io::Result<()> {
    for _ in 0..100 {
        if exists_as_owner(socket)? {
            return Ok(());
        }
        if exited(process) {
            return Err(failed(&format!("{name} failed")));
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
    let text = |path: &Path| path.to_string_lossy().into_owned();
    let runtime = webagent_configuration::paths::runtime();
    let (control, worker_dir, external_dir) = (
        text(&runtime.join("control")),
        text(&runtime.join("worker")),
        text(&runtime.join("worker-c")),
    );
    // Stream D4: with `V3_EXTERNAL_WORKER=1` the tool worker runs in its own container (the
    // broker's follower `<api>-worker`), serving `runtime/worker-c/tools.sock` (the API and
    // loop get `V3_TOOL_SOCKET` for it). This launcher then only prepares that folder and
    // starts the API. A release without this mode starts its own worker on
    // `runtime/worker/tools.sock`, so a rollback never puts two workers on one socket.
    let external = std::env::var_os("V3_EXTERNAL_WORKER").is_some_and(|value| value == "1");
    let runtime = text(&runtime);
    directory(
        &text(&webagent_configuration::paths::state()),
        APP_UID,
        APP_GID,
        0o770,
    )?;
    // Workspaces are shared through the worker's group: the worker creates each session's
    // folder, the app (which has the group too) reads previews and files from it.
    directory(
        &text(&webagent_configuration::paths::workspaces()),
        1101,
        WORK_GID,
        0o770,
    )?;
    for folder in [&runtime, &control, &worker_dir] {
        directory(folder, 0, 0, 0o700)?;
    }
    for (name, content) in [("dispatch.lock", ""), ("shutoff", "1")] {
        let file = format!("{control}/{name}");
        if Path::new(&file).exists() {
            chown(&file, 0, 0)?;
        }
        std::fs::write(&file, content)?;
        chmod(&file, 0o640)?;
    }
    // The worker reads these two through its group; it may pass through the control folder
    // but not list it, so anything else the app keeps there stays private.
    for name in ["dispatch.lock", "shutoff"] {
        chown(&format!("{control}/{name}"), APP_UID, WORK_GID)?;
    }
    directory(&control, APP_UID, WORK_GID, 0o710)?;
    directory(&worker_dir, WORKER_UID, WORK_GID, 0o750)?;
    if external {
        directory(&external_dir, WORKER_UID, WORK_GID, 0o750)?;
    }
    directory(&runtime, APP_UID, APP_GID, 0o755)?;
    let browser_socket = text(&crate::browser::host::default_socket());
    // The socket sits in a folder only its owner and group may enter, and the launcher keeps no
    // capability to read past file modes: it takes the worker's group for this step (the groups
    // are cleared below before anything starts).
    // SAFETY: a plain system call with a valid one-element group list.
    if unsafe { libc::setgroups(1, &WORK_GID) } != 0 {
        return Err(io::Error::last_os_error());
    }
    browser_socket_group(&format!("{runtime}/browser"), &browser_socket)?;

    // SAFETY: plain system calls; the launcher keeps root only to start the worker.
    unsafe {
        if libc::setgroups(0, std::ptr::null()) != 0 || libc::setgid(APP_GID) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let tools_socket = format!("{worker_dir}/tools.sock");
    let mut env = vec![
        ("PATH", "/usr/bin:/bin"),
        ("LANG", "C.UTF-8"),
        ("V3_TOOL_SOCKET", tools_socket.as_str()),
        ("V3_TOOL_CLIENT_UID", "1100"),
    ];
    // Browsers run in the browser container once its socket folder exists (the operator
    // creates it with that container); the worker then never starts a browser itself.
    let host = std::env::var("V3_BROWSER_HOST").ok().or_else(|| {
        Path::new(&format!("{runtime}/browser"))
            .is_dir()
            .then(|| browser_socket.clone())
    });
    if let Some(host) = &host {
        env.push(("V3_BROWSER_HOST", host));
    }
    let mut tools = if external {
        None
    } else {
        let mut tools = worker(&[APP_BINARY, "--tool-worker"], &env)?;
        wait_for(&tools_socket, &mut tools, "tool worker")?;
        Some(tools)
    };
    // The API keeps the container environment, as before.
    let mut app = Command::new(APP_BINARY);
    // SAFETY: drop_to only makes raw system calls.
    unsafe {
        // The API reads workspaces and reaches the worker through the worker's group.
        app.pre_exec(|| drop_to(APP_UID, APP_GID, &[WORK_GID]));
    }
    let mut app = app.spawn()?;
    // The launcher itself drops to the application identity; it keeps no root privileges.
    drop_to(APP_UID, APP_GID, &[])?;
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
        if exited(&mut app) || tools.as_mut().is_some_and(exited) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    if !exited(&mut app) {
        terminate_app();
    }
    // As launch.py: the first process that ended with a non-zero code decides the exit.
    let code = std::iter::once(&mut app)
        .chain(tools.as_mut())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_worker_runs_in_its_own_group_without_the_app_group() {
        assert!(WORKER_IDENTITY.contains(&"--regid=1105"));
        assert!(!WORKER_IDENTITY.iter().any(|arg| arg.contains("1100")));
        assert!(!WORKER_IDENTITY.iter().any(|arg| arg.contains("1110")));
        assert!(WORKER_IDENTITY.contains(&"--clear-groups"));
        assert_eq!(WORK_GID, 1105);
        assert_ne!(WORK_GID, APP_GID);
    }

    #[test]
    fn browser_socket_group_skips_missing_and_refuses_unexpected_files() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let folder = dir.path().join("browser");
        let socket = folder.join("host.sock");
        let text = |path: &Path| path.to_string_lossy().into_owned();
        // No browser container on this server: nothing to change.
        assert!(browser_socket_group(&text(&folder), &text(&socket)).is_ok());
        // A plain file where the folder belongs is refused, never changed.
        std::fs::write(&folder, b"").expect("write");
        assert!(browser_socket_group(&text(&folder), &text(&socket)).is_err());
        std::fs::remove_file(&folder).expect("remove");
        // A link in place of the socket is refused instead of being followed.
        std::fs::create_dir(&folder).expect("folder");
        std::os::unix::fs::symlink(dir.path(), &socket).expect("link");
        let changed = browser_socket_group(&text(&dir.path().join("absent")), &text(&socket));
        assert!(changed.is_err());
    }
}
