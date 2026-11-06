//! The Landlock and seccomp boundary for workspace commands, ported from `sandbox.py`.
//! The command may read the given paths, read and write its workspace, bind only its
//! claimed preview ports, never signal or reach processes outside its domain, and never
//! create Unix sockets. The supervisor builds the ruleset and filter; the child only applies
//! them between fork and exec, with raw system calls and no allocation.
use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

/// Execute, read file, read directory.
const READ: u64 = (1 << 0) | (1 << 2) | (1 << 3);
/// Write, remove and create rights, without device-node creation or device ioctl.
const WRITE: u64 = (1 << 1)
    | (1 << 4)
    | (1 << 5)
    | (1 << 7)
    | (1 << 8)
    | (1 << 9)
    | (1 << 10)
    | (1 << 12)
    | (1 << 13)
    | (1 << 14);
/// The rights Landlock accepts on a file rather than a directory.
const FILE_RIGHTS: u64 = (1 << 0) | (1 << 1) | (1 << 2) | (1 << 14);
const CREATE_RULESET: libc::c_long = 444;
const ADD_RULE: libc::c_long = 445;
const RESTRICT_SELF: libc::c_long = 446;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C, packed)]
struct PathBeneath {
    allowed_access: u64,
    parent_fd: i32,
}

#[repr(C)]
struct NetPort {
    allowed_access: u64,
    port: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Filter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct Program {
    len: u16,
    filter: *const Filter,
}

/// A ruleset, and for commands the Unix socket filter, ready to apply in a child.
pub struct Prepared {
    ruleset: OwnedFd,
    filter: Option<[Filter; 11]>,
}

fn checked(value: libc::c_long) -> io::Result<libc::c_long> {
    if value < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(value)
    }
}

fn unavailable(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

/// The Landlock ABI version, or an error when Landlock is missing.
///
/// # Errors
/// When the kernel has no Landlock.
pub fn abi() -> io::Result<i64> {
    // SAFETY: a version query passes no memory and creates nothing.
    checked(unsafe {
        libc::syscall(
            CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0_usize,
            1_u32,
        )
    })
}

/// Builds the boundary for a child in `base` that may also read `read_paths` and bind
/// `ports` (at most four, 20000–29999). `deny_unix` adds the Unix socket fence commands
/// get; ability plugins run without it, as before.
///
/// # Errors
/// When Landlock ABI 6 is unavailable, a rule cannot be added, the ports are invalid or the
/// ephemeral port range overlaps the preview range. A command never starts unconfined.
pub fn prepare(
    base: &Path,
    read_paths: &[&Path],
    ports: &[u16],
    deny_unix: bool,
) -> io::Result<Prepared> {
    let base = std::fs::canonicalize(base)?;
    if abi()? < 6 {
        return Err(unavailable(
            "workspace sandbox requires Landlock ABI 6 or newer",
        ));
    }
    // Handle every filesystem right supported through ABI 8, deny unlisted paths, and
    // isolate signals and abstract Unix sockets from other domains.
    let attr = RulesetAttr {
        handled_access_fs: (1 << 16) - 1,
        handled_access_net: 1,
        scoped: 3,
    };
    // SAFETY: `attr` outlives the call and its size is passed with it.
    let fd = checked(unsafe {
        libc::syscall(
            CREATE_RULESET,
            &attr as *const RulesetAttr,
            std::mem::size_of::<RulesetAttr>(),
            0_u32,
        )
    })?;
    let fd = i32::try_from(fd).map_err(|_| unavailable("invalid ruleset descriptor"))?;
    // SAFETY: the kernel just returned this descriptor and nothing else owns it.
    let ruleset = unsafe { OwnedFd::from_raw_fd(fd) };
    let allow = |path: &Path, rights: u64| -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
            .open(path)?;
        let rights = if path.is_dir() {
            rights
        } else {
            rights & FILE_RIGHTS
        };
        let rule = PathBeneath {
            allowed_access: rights,
            parent_fd: file.as_raw_fd(),
        };
        // SAFETY: `rule` and the descriptor it names outlive the call.
        checked(unsafe {
            libc::syscall(
                ADD_RULE,
                ruleset.as_raw_fd(),
                1_u32,
                &rule as *const PathBeneath,
                0_u32,
            )
        })?;
        Ok(())
    };
    for path in ["/usr", "/bin", "/lib", "/lib64", "/opt/rust"] {
        allow(Path::new(path), READ)?;
    }
    for path in [
        "/etc/ld.so.cache",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/ssl/certs",
        "/etc/passwd",
        "/etc/group",
        "/dev/random",
        "/dev/urandom",
    ] {
        allow(Path::new(path), READ)?;
    }
    allow(Path::new("/dev/null"), (1 << 1) | (1 << 2))?;
    allow(&base, READ | WRITE)?;
    for path in read_paths {
        allow(path, READ)?;
    }
    // The worker assigns unique workspace preview ports; a command cannot bind another
    // workspace's port, even while a listener is being replaced.
    if ports.len() > 4 || ports.iter().any(|port| !(20000..=29999).contains(port)) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid trusted workspace listen ports",
        ));
    }
    // Ephemeral listeners used by test fixtures stay outside the reserved preview range.
    let range = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")?;
    let lower: u32 = range
        .split_whitespace()
        .next()
        .and_then(|lower| lower.parse().ok())
        .ok_or_else(|| unavailable("ephemeral port range unreadable"))?;
    if lower <= 29999 {
        return Err(unavailable("ephemeral ports overlap workspace previews"));
    }
    for port in std::iter::once(0).chain(ports.iter().copied()) {
        let rule = NetPort {
            allowed_access: 1,
            port: u64::from(port),
        };
        // SAFETY: `rule` outlives the call.
        checked(unsafe {
            libc::syscall(
                ADD_RULE,
                ruleset.as_raw_fd(),
                2_u32,
                &rule as *const NetPort,
                0_u32,
            )
        })?;
    }
    Ok(Prepared {
        ruleset,
        filter: if deny_unix {
            Some(unix_socket_filter()?)
        } else {
            None
        },
    })
}

/// Denies `socket(AF_UNIX, ...)`: file confinement alone does not mediate `connect`. The
/// architecture is checked and x32 calls refused before the socket domain is inspected.
fn unix_socket_filter() -> io::Result<[Filter; 11]> {
    let (architecture, socket) = if cfg!(target_arch = "x86_64") {
        (0xc000_003e, 41)
    } else if cfg!(target_arch = "aarch64") {
        (0xc000_00b7, 198)
    } else {
        return Err(unavailable(
            "Command socket fence requires a supported native architecture",
        ));
    };
    let f = |code, jt, jf, k| Filter { code, jt, jf, k };
    Ok([
        f(0x20, 0, 0, 4),
        f(0x15, 1, 0, architecture),
        f(0x06, 0, 0, 0x8000_0000),
        f(0x20, 0, 0, 0),
        f(0x35, 0, 1, 0x4000_0000),
        f(0x06, 0, 0, 0x8000_0000),
        f(0x15, 0, 3, socket),
        f(0x20, 0, 0, 16),
        f(0x15, 0, 1, 1),
        f(0x06, 0, 0, 0x0005_0001),
        f(0x06, 0, 0, 0x7fff_0000),
    ])
}

impl Prepared {
    /// Confines the calling process. Call only between fork and exec (`pre_exec`).
    ///
    /// # Safety
    /// Every step is a raw system call on memory prepared before the fork, so it is safe
    /// in a forked child of a multi-threaded process.
    ///
    /// # Errors
    /// When any step fails; the command then never executes.
    pub unsafe fn apply(&self) -> io::Result<()> {
        // SAFETY: plain prctl/syscall calls on prepared, live memory (see above).
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            checked(libc::syscall(
                RESTRICT_SELF,
                self.ruleset.as_raw_fd(),
                0_u32,
            ))?;
            if let Some(filter) = &self.filter {
                let program = Program {
                    len: 11,
                    filter: filter.as_ptr(),
                };
                if libc::prctl(libc::PR_SET_SECCOMP, 2, &program as *const Program, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        Ok(())
    }
}
