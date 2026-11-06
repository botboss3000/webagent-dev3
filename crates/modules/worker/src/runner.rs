//! The per-call supervisors, ported from `worker.py --runner` and
//! `ability_plugin_worker.py --supervise`: `webagent-v3 --tool-runner ARGV_JSON` for a
//! workspace command and `webagent-v3 --plugin-runner SPEC_JSON` for an ability plugin. A
//! supervisor stays outside the sandbox so it can always reap: it becomes a child
//! subreaper, so double-forked descendants reparent to it, runs the child confined by
//! [`crate::sandbox`], and on exit or SIGTERM ends every descendant.
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static TERMINATED: AtomicBool = AtomicBool::new(false);

extern "C" fn terminated(_signal: libc::c_int) {
    TERMINATED.store(true, Ordering::SeqCst);
}

/// What one supervisor runs and how it is confined.
struct Spec {
    argv: Vec<String>,
    read_paths: Vec<PathBuf>,
    /// Commands: Unix socket fence, no stdin, descendants get SIGTERM before SIGKILL.
    /// Plugins: no fence, the protocol frame on stdin, descendants get SIGKILL at once.
    command: bool,
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

/// Runs the command `argv_json` names in the current directory and returns the exit code:
/// the command's own, 128 + signal when a signal ended it, 143 when the supervisor was told
/// to stop, and 1 when the command could not start confined.
#[must_use]
pub fn supervise(argv_json: &str) -> i32 {
    let spec = serde_json::from_str(argv_json).map(|argv| Spec {
        argv,
        read_paths: command_read_paths(),
        command: true,
    });
    finish(spec.map_err(invalid).and_then(run), true)
}

/// Runs an ability plugin: `spec_json` is `{"argv": [...], "read": [package root,
/// executable]}`, both already verified by the worker. The plugin reads its protocol frame
/// from this process's stdin. Exit codes as [`supervise`].
#[must_use]
pub fn supervise_plugin(spec_json: &str) -> i32 {
    let spec = serde_json::from_str::<serde_json::Value>(spec_json)
        .map_err(invalid)
        .and_then(|spec| {
            let strings = |key: &str| -> io::Result<Vec<String>> {
                serde_json::from_value(spec[key].clone()).map_err(invalid)
            };
            Ok(Spec {
                argv: strings("argv")?,
                read_paths: strings("read")?.into_iter().map(PathBuf::from).collect(),
                command: false,
            })
        });
    finish(spec.and_then(run), false)
}

/// Commands read the container's system folders and the checkout's source files, and
/// write only this workspace. Runtime state, other sessions' workspaces, control sockets
/// and the checkout's runtime folders stay out. Checkout writes go through checkout_edit
/// or broker tasks.
fn command_read_paths() -> Vec<PathBuf> {
    const HIDDEN: [&str; 4] = ["state", "workspaces", "live-checkout", "run"];
    let entries = |folder: &str, skip: &[&str]| -> Vec<PathBuf> {
        std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| !skip.iter().any(|name| entry.file_name() == *name))
            .map(|entry| entry.path())
            .collect()
    };
    let mut paths = entries("/", &HIDDEN);
    paths.extend(entries("/live-checkout", &crate::files::CHECKOUT_HIDDEN));
    paths
}

fn finish(result: io::Result<i32>, graceful: bool) -> i32 {
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("Supervisor: {error}");
            clean_descendants(graceful);
            1
        }
    }
}

fn run(spec: Spec) -> io::Result<i32> {
    let graceful = spec.command;
    let (program, args) = spec
        .argv
        .split_first()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
    // SAFETY: prctl and sigaction calls with valid arguments; the handler only stores to an
    // atomic, which is async-signal-safe.
    unsafe {
        // Orphaned and double-forked children reparent here, not to container init.
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = terminated as extern "C" fn(libc::c_int) as usize;
        if libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    let expected_parent = std::env::var("V3_SUPERVISOR_PARENT")
        .ok()
        .and_then(|parent| parent.parse().ok())
        // SAFETY: getppid has no preconditions.
        .unwrap_or_else(|| unsafe { libc::getppid() });
    // SAFETY: as above.
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM, 0, 0, 0) != 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::getppid() != expected_parent {
            clean_descendants(graceful);
            return Ok(143);
        }
    }
    let ports: Vec<u16> = std::env::var("V3_ALLOWED_LISTEN_PORTS")
        .unwrap_or_default()
        .split(',')
        .filter(|port| !port.is_empty())
        .map(|port| {
            port.parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid trusted workspace listen ports",
                )
            })
        })
        .collect::<io::Result<_>>()?;
    let read_paths: Vec<&Path> = spec.read_paths.iter().map(PathBuf::as_path).collect();
    let sandbox =
        crate::sandbox::prepare(&std::env::current_dir()?, &read_paths, &ports, spec.command)?;
    let mut command = Command::new(program);
    command.args(args);
    if spec.command {
        command.stdin(Stdio::null());
    }
    // SAFETY: `apply` only makes raw system calls on memory prepared before the fork.
    unsafe {
        command.pre_exec(move || sandbox.apply());
    }
    let mut child = command.spawn()?;
    let mut pause = Duration::from_millis(1);
    let status = loop {
        if TERMINATED.load(Ordering::SeqCst) {
            clean_descendants(graceful);
            return Ok(143);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    };
    clean_descendants(graceful);
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// This process's descendants, from the public parent links in `/proc`.
fn descendants() -> Vec<i32> {
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
            // Read only public process metadata, never environ or session data.
            let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
                continue;
            };
            let parent = stat
                .rfind(')')
                .and_then(|at| stat[at + 2..].split_whitespace().nth(1))
                .and_then(|parent| parent.parse::<i32>().ok());
            if let Some(parent) = parent {
                parents.push((pid, parent));
            }
        }
    }
    // SAFETY: getpid has no preconditions.
    let mut found = vec![unsafe { libc::getpid() }];
    let mut changed = true;
    while changed {
        changed = false;
        for (pid, parent) in &parents {
            if !found.contains(pid) && found.contains(parent) {
                found.push(*pid);
                changed = true;
            }
        }
    }
    found.remove(0);
    found
}

/// Ends every descendant, reaping as they exit: with `graceful`, SIGTERM for three seconds
/// and then SIGKILL; otherwise SIGKILL at once.
fn clean_descendants(graceful: bool) {
    let deadline = Instant::now() + Duration::from_secs(if graceful { 3 } else { 0 });
    loop {
        let children = descendants();
        if children.is_empty() {
            break;
        }
        let signal = if Instant::now() < deadline {
            libc::SIGTERM
        } else {
            libc::SIGKILL
        };
        for pid in children {
            // SAFETY: kill and waitpid take plain integers; a vanished pid is ignored.
            unsafe {
                libc::kill(pid, signal);
            }
        }
        // SAFETY: as above.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        std::thread::sleep(Duration::from_millis(50));
        if Instant::now() > deadline + Duration::from_secs(1) {
            break;
        }
    }
}
