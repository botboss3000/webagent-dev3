//! The per-call supervisors, ported from `worker.py --runner` and
//! `ability_plugin_worker.py --supervise`: `webagent-v3 --tool-runner ARGV_JSON` for a
//! workspace command and `webagent-v3 --plugin-runner SPEC_JSON` for an ability plugin. A
//! supervisor stays outside the sandbox so it can always reap: it becomes a child
//! subreaper, so double-forked descendants reparent to it, runs the child confined by
//! [`crate::sandbox`], and on exit or SIGTERM ends every descendant.
use crate::limits::CommandLimits;
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
    /// A Sandbox ability command: the strict sandbox, resource limits and a folder cap.
    strict: Option<crate::sandbox_tools::Confinement>,
    /// What the whole process tree may use; plugins are not metered.
    limits: Option<CommandLimits>,
}

fn invalid(error: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, error.to_string())
}

/// Runs the command `argv_json` names in the current directory and returns the exit code:
/// the command's own, 128 + signal when a signal ended it, 143 when the supervisor was told
/// to stop, and 1 when the command could not start confined.
#[must_use]
pub fn supervise(argv_json: &str) -> i32 {
    let strict = std::env::var("V3_SANDBOX_CONFINEMENT")
        .ok()
        .map(|text| crate::sandbox_tools::Confinement::parse(&text))
        .transpose();
    let spec = strict.and_then(|strict| {
        let argv = serde_json::from_str(argv_json).map_err(invalid)?;
        Ok(Spec {
            argv,
            // A Sandbox command reads only the strict sandbox's own system files and the
            // read-only copy of the agent's saved elements beside its folder.
            read_paths: if strict.is_some() {
                std::env::current_dir()
                    .map(|base| base.with_file_name("saved"))
                    .into_iter()
                    .filter(|saved| saved.is_dir())
                    .collect()
            } else {
                command_read_paths(std::env::var("V3_CHECKOUT_READ").is_ok_and(|v| v == "1"))
            },
            command: true,
            limits: Some(match strict {
                Some(strict) => CommandLimits {
                    memory_bytes: strict.memory_bytes,
                    processes: strict.processes,
                    cpu_seconds: strict.cpu_seconds,
                },
                None => CommandLimits::from_env(std::env::var("V3_COMMAND_LIMITS").ok().as_deref()),
            }),
            strict,
        })
    });
    finish(spec.and_then(run), true)
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
                strict: None,
                limits: None,
            })
        });
    finish(spec.and_then(run), false)
}

/// Commands read the container's system folders and, with `checkout`, the checkout's source
/// files, and write only this workspace. Runtime state, other sessions' workspaces, control
/// sockets and the checkout's runtime folders stay out. Checkout writes go through
/// checkout_edit or broker tasks.
pub(crate) fn command_read_paths(checkout: bool) -> Vec<PathBuf> {
    read_paths(
        Path::new("/"),
        &webagent_configuration::paths::checkout(),
        checkout,
    )
}

fn read_paths(root: &Path, code: &Path, checkout: bool) -> Vec<PathBuf> {
    // `live-checkout` is the image's compatibility link to `/code`: granting it would grant all
    // of `/code`, hidden folders included.
    const HIDDEN: [&str; 6] = [
        "state",
        "workspaces",
        "code",
        "live-checkout",
        "runtime",
        "run",
    ];
    let entries = |folder: &Path, skip: &[&str]| -> Vec<PathBuf> {
        std::fs::read_dir(folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| !skip.iter().any(|name| entry.file_name() == *name))
            .map(|entry| entry.path())
            .collect()
    };
    let mut paths = entries(root, &HIDDEN);
    if checkout {
        paths.extend(entries(code, &crate::files::CHECKOUT_HIDDEN));
    }
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
    let base = std::env::current_dir()?;
    let sandbox = match spec.strict {
        Some(_) => crate::sandbox::prepare_strict(&base, &read_paths)?,
        None => crate::sandbox::prepare(&base, &read_paths, &ports, spec.command)?,
    };
    let rlimits = match spec.limits {
        Some(limits) => resource_limits(spec.strict, limits.processes)?,
        None => Vec::new(),
    };
    let metered = spec.limits.is_some();
    let mut command = Command::new(program);
    command.args(args);
    if spec.command {
        command.stdin(Stdio::null());
    }
    // SAFETY: setrlimit, `first_to_kill` and `apply` only make raw system calls on memory
    // prepared before the fork.
    unsafe {
        command.pre_exec(move || {
            set_limits(&rlimits)?;
            if metered {
                first_to_kill();
            }
            sandbox.apply()
        });
    }
    let mut child = command.spawn()?;
    let mut pause = Duration::from_millis(1);
    let mut measured = Instant::now();
    let who = if spec.strict.is_some() {
        "Sandbox"
    } else {
        "Command"
    };
    let status = loop {
        if TERMINATED.load(Ordering::SeqCst) {
            clean_descendants(graceful);
            return Ok(143);
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        // Measured on every pass (1 ms apart at first, then up to 50 ms), so a burst of
        // short-lived forks is seen even where the kernel's process limit cannot apply.
        if let Some(limits) = spec.limits {
            if let Some((code, reason)) = exceeded(&limits, &usage()) {
                eprintln!("{who}: {reason}, so it was stopped");
                clean_descendants(false);
                return Ok(code);
            }
        }
        if let Some(strict) = spec.strict {
            if measured.elapsed() >= FOLDER_CHECK {
                measured = Instant::now();
                if crate::sandbox_tools::folder_bytes(&base) > strict.folder_cap_bytes {
                    eprintln!(
                        "Sandbox: the folder passed its size limit, so the command was stopped"
                    );
                    clean_descendants(graceful);
                    return Ok(125);
                }
            }
        }
        std::thread::sleep(pause);
        pause = (pause * 2).min(Duration::from_millis(50));
    };
    clean_descendants(graceful);
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// How often a Sandbox command's folder is measured against its cap.
const FOLDER_CHECK: Duration = Duration::from_millis(500);
/// How often a terminal's process tree is measured against its limits.
pub(crate) const METER: Duration = Duration::from_millis(100);

/// What one command's process tree uses at a moment.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct Usage {
    /// Processes and threads now running.
    tasks: u64,
    /// Private (anonymous) resident memory of all its processes together.
    memory_bytes: u64,
    /// CPU time of all its processes, ended ones included.
    cpu_seconds: u64,
}

/// Measures this supervisor's descendants from `/proc`: their threads, private memory and
/// CPU time, plus the CPU time of ended descendants already counted into a live parent or
/// reaped by this supervisor.
pub(crate) fn usage() -> Usage {
    // SAFETY: sysconf and getrusage write only into the struct passed by pointer.
    let (page, ticks, reaped) = unsafe {
        let mut own: libc::rusage = std::mem::zeroed();
        libc::getrusage(libc::RUSAGE_CHILDREN, &mut own);
        (
            u64::try_from(libc::sysconf(libc::_SC_PAGESIZE)).unwrap_or(4096),
            u64::try_from(libc::sysconf(libc::_SC_CLK_TCK))
                .unwrap_or(100)
                .max(1),
            u64::try_from(own.ru_utime.tv_sec + own.ru_stime.tv_sec).unwrap_or(0),
        )
    };
    let mut usage = Usage {
        cpu_seconds: reaped,
        ..Usage::default()
    };
    let mut cpu_ticks = 0u64;
    for pid in descendants() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // Fields after the command name: state is 0, utime 11, stime 12, cutime 13,
        // cstime 14 and num_threads 17.
        let fields: Vec<u64> = stat
            .rfind(')')
            .map(|at| &stat[at + 2..])
            .unwrap_or_default()
            .split_whitespace()
            .map(|field| field.parse().unwrap_or(0))
            .collect();
        let field = |index: usize| fields.get(index).copied().unwrap_or(0);
        cpu_ticks += field(11) + field(12) + field(13) + field(14);
        usage.tasks += field(17).max(1);
        // statm: total, resident and shared (file and shared memory) pages.
        if let Ok(statm) = std::fs::read_to_string(format!("/proc/{pid}/statm")) {
            let pages: Vec<u64> = statm
                .split_whitespace()
                .map(|value| value.parse().unwrap_or(0))
                .collect();
            if let (Some(resident), Some(shared)) = (pages.get(1), pages.get(2)) {
                usage.memory_bytes += resident.saturating_sub(*shared) * page;
            }
        }
    }
    usage.cpu_seconds += cpu_ticks / ticks;
    usage
}

/// The limit a command's tree has passed: the exit code the command reports and the reason
/// its output ends with.
pub(crate) fn exceeded(limits: &CommandLimits, usage: &Usage) -> Option<(i32, String)> {
    const MIB: u64 = 1024 * 1024;
    if usage.tasks > limits.processes {
        return Some((
            126,
            format!(
                "the command started more than {} processes",
                limits.processes
            ),
        ));
    }
    if usage.memory_bytes > limits.memory_bytes {
        return Some((
            137,
            format!(
                "the command used more than {} MB of memory",
                limits.memory_bytes / MIB
            ),
        ));
    }
    if usage.cpu_seconds > limits.cpu_seconds {
        return Some((
            152,
            format!(
                "the command used more than {} seconds of CPU time",
                limits.cpu_seconds
            ),
        ));
    }
    None
}

/// Applies `limits` to the calling process. Only raw system calls, so it is safe between
/// fork and exec.
pub(crate) fn set_limits(limits: &[(Resource, libc::rlim_t)]) -> io::Result<()> {
    for (resource, value) in limits {
        let limit = libc::rlimit {
            rlim_cur: *value,
            rlim_max: *value,
        };
        // SAFETY: setrlimit reads the struct passed by pointer.
        if unsafe { libc::setrlimit(*resource, &limit) } != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Makes the calling process, and everything it starts, the kernel's first choice when the
/// container runs out of memory, so a runaway command dies before the app does. Only raw
/// system calls, so it is safe between fork and exec; a failure leaves the score unchanged.
pub(crate) fn first_to_kill() {
    const PATH: &[u8] = b"/proc/self/oom_score_adj\0";
    // SAFETY: open, write and close on a NUL-terminated constant path and a constant buffer.
    unsafe {
        let fd = libc::open(PATH.as_ptr().cast(), libc::O_WRONLY | libc::O_CLOEXEC);
        if fd >= 0 {
            libc::write(fd, b"1000".as_ptr().cast(), 4);
            libc::close(fd);
        }
    }
}

#[cfg(target_env = "gnu")]
pub(crate) type Resource = libc::__rlimit_resource_t;
#[cfg(not(target_env = "gnu"))]
pub(crate) type Resource = libc::c_int;

/// The kernel limits of a command: a Sandbox command's per-process limits, and for every
/// command the process limit, so a fork bomb is refused at once rather than at the next
/// measurement. The kernel's process limit counts every task of this user on the host,
/// which a container's `/proc` may not show, so it is applied (the user's visible count
/// plus the command's allowance) only when a probe child can still fork under it. The
/// supervisor also counts the command's processes either way.
pub(crate) fn resource_limits(
    strict: Option<crate::sandbox_tools::Confinement>,
    processes: u64,
) -> io::Result<Vec<(Resource, libc::rlim_t)>> {
    let mut limits = match strict {
        Some(strict) => vec![
            (libc::RLIMIT_CPU, strict.cpu_seconds),
            (libc::RLIMIT_AS, strict.memory_bytes),
            (libc::RLIMIT_FSIZE, strict.file_bytes),
            (libc::RLIMIT_NOFILE, strict.open_files),
            (libc::RLIMIT_CORE, 0),
        ],
        None => Vec::new(),
    };
    let processes = user_tasks()?.saturating_add(processes);
    if forks_under(processes) {
        limits.push((libc::RLIMIT_NPROC, processes));
    }
    Ok(limits)
}

/// Whether a child that sets `RLIMIT_NPROC` to `limit` can still start one process: false
/// when this user runs tasks this `/proc` does not show. The supervisor is single-threaded,
/// so forking here is sound.
fn forks_under(limit: libc::rlim_t) -> bool {
    // SAFETY: fork in a single-threaded process; the child only makes raw system calls
    // and leaves with _exit.
    unsafe {
        let probe = libc::fork();
        if probe < 0 {
            return false;
        }
        if probe == 0 {
            let rlimit = libc::rlimit {
                rlim_cur: limit,
                rlim_max: limit,
            };
            if libc::setrlimit(libc::RLIMIT_NPROC, &rlimit) != 0 {
                libc::_exit(1);
            }
            let child = libc::fork();
            if child < 0 {
                libc::_exit(1);
            }
            if child == 0 {
                libc::_exit(0);
            }
            libc::waitpid(child, std::ptr::null_mut(), 0);
            libc::_exit(0);
        }
        let mut status = 0;
        libc::waitpid(probe, &mut status, 0) == probe
            && libc::WIFEXITED(status)
            && libc::WEXITSTATUS(status) == 0
    }
}

/// The tasks (threads included) this process's user runs, from `/proc`.
fn user_tasks() -> io::Result<u64> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut tasks = 0u64;
    for entry in std::fs::read_dir("/proc")?.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        if entry.metadata().is_ok_and(|meta| meta.uid() == uid) {
            tasks += std::fs::read_dir(entry.path().join("task")).map_or(1, |t| t.count() as u64);
        }
    }
    Ok(tasks)
}

/// This process's descendants, from the public parent links in `/proc`.
pub(crate) fn descendants() -> Vec<i32> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkout_source_is_readable_only_with_the_grant() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let code = dir.path().join("code");
        for folder in ["src", "data", "build-jobs"] {
            std::fs::create_dir_all(code.join(folder)).expect("checkout");
        }
        std::fs::create_dir_all(dir.path().join("usr")).expect("system folder");
        let granted = read_paths(dir.path(), &code, true);
        assert!(granted.contains(&code.join("src")));
        assert!(granted.contains(&dir.path().join("usr")));
        assert!(!granted
            .iter()
            .any(|path| path == &code || path.ends_with("data") || path.ends_with("build-jobs")));
        let ungranted = read_paths(dir.path(), &code, false);
        assert_eq!(ungranted, [dir.path().join("usr")]);
    }
}
