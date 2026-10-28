//! Reusable bounded private-pipe wrapper for registered pure engine implementations,
//! and the seccomp isolation that engine and module service processes apply.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, Read, Write};

/// Syscalls a pure computation needs to read and write its private pipes.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PIPE_SYSCALLS: [libc::c_long; 28] = [
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_close,
    libc::SYS_fstat,
    libc::SYS_lseek,
    libc::SYS_mmap,
    libc::SYS_mprotect,
    libc::SYS_munmap,
    libc::SYS_brk,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    libc::SYS_rt_sigreturn,
    libc::SYS_ioctl,
    libc::SYS_sched_yield,
    libc::SYS_madvise,
    libc::SYS_getpid,
    libc::SYS_exit,
    libc::SYS_fcntl,
    libc::SYS_sigaltstack,
    libc::SYS_gettid,
    libc::SYS_futex,
    libc::SYS_set_tid_address,
    libc::SYS_clock_gettime,
    libc::SYS_exit_group,
    libc::SYS_set_robust_list,
    libc::SYS_prlimit64,
    libc::SYS_getrandom,
    libc::SYS_rseq,
];

const LOAD: u16 = 0x20; // BPF_LD | BPF_W | BPF_ABS
const JUMP_EQUAL: u16 = 0x15; // BPF_JMP | BPF_JEQ | BPF_K
const RETURN: u16 = 0x06; // BPF_RET | BPF_K
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
const KILL_PROCESS: u32 = 0x8000_0000;
const ALLOW: u32 = 0x7fff_0000;
const DENY_EPERM: u32 = 0x0005_0001;

/// The seccomp program: kill a foreign architecture, allow the pipe syscalls plus
/// `extra`, deny everything else with EPERM.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn filter(extra: &[libc::c_long]) -> Result<Vec<libc::sock_filter>> {
    let rule = |code, jt, jf, k| libc::sock_filter { code, jt, jf, k };
    // seccomp_data: the architecture is at offset 4, the syscall number at 0.
    let mut filter = vec![
        rule(LOAD, 0, 0, 4),
        rule(JUMP_EQUAL, 1, 0, AUDIT_ARCH_X86_64),
        rule(RETURN, 0, 0, KILL_PROCESS),
        rule(LOAD, 0, 0, 0),
    ];
    for number in PIPE_SYSCALLS.iter().chain(extra) {
        filter.push(rule(JUMP_EQUAL, 0, 1, u32::try_from(*number)?));
        filter.push(rule(RETURN, 0, 0, ALLOW));
    }
    filter.push(rule(RETURN, 0, 0, DENY_EPERM));
    Ok(filter)
}

/// Close inherited descriptors above stdio, bound memory and descriptors, and
/// prohibit ambient files, network and execution: only the pipe syscalls plus
/// `extra` are allowed. Fails closed on unsupported architectures.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn isolate(extra: &[libc::c_long]) -> Result<()> {
    let mut filter = filter(extra)?;
    let program = libc::sock_fprog {
        len: u16::try_from(filter.len())?,
        filter: filter.as_mut_ptr(),
    };
    for fd in 3..1024 {
        // SAFETY: runs first in the service process; nothing in it owns these inherited
        // descriptors, and closing an unused number is harmless.
        unsafe { libc::close(fd) };
    }
    // The host prevents limit mutation after exec. Standalone checks set them here;
    // a denied mutation is acceptable only when the host already installed tighter bounds.
    for (resource, bound) in [
        (libc::RLIMIT_AS, 512 * 1024 * 1024),
        (libc::RLIMIT_NOFILE, 32),
    ] {
        let wanted = libc::rlimit {
            rlim_cur: bound,
            rlim_max: bound,
        };
        // SAFETY: passes a valid rlimit for a fixed resource.
        if unsafe { libc::setrlimit(resource, &wanted) } != 0 {
            let mut current = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: writes into the local rlimit only.
            let read = unsafe { libc::getrlimit(resource, &mut current) };
            if read != 0 || current.rlim_cur > bound || current.rlim_max > bound {
                bail!("service resource bounds unavailable");
            }
        }
    }
    // SAFETY: prctl with constant options and a filter program that outlives the call
    // (the kernel copies it).
    let isolated = unsafe {
        libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) == 0
            && libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                &program as *const libc::sock_fprog,
            ) == 0
    };
    if !isolated {
        bail!("service isolation unavailable");
    }
    Ok(())
}

#[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
pub fn isolate(_extra: &[i64]) -> Result<()> {
    bail!("service isolation unsupported");
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    version: u32,
    id: String,
    run_key: String,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    input: Option<Value>,
}

#[derive(Serialize)]
struct Reply {
    version: u32,
    id: String,
    result: Value,
}

/// Accept only fixed computation/release operations; no paths, sockets or ambient credentials.
pub fn serve(limit: usize, compute: impl Fn(Value) -> Result<Value>) -> Result<()> {
    isolate(&[])?;
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    loop {
        let mut bytes = Vec::new();
        let length = (&mut reader)
            .take((limit + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        if length == 0 {
            return Ok(());
        }
        if bytes.len() > limit || bytes.last() != Some(&b'\n') {
            bail!("Engine frame limit");
        }
        let call: Call = serde_json::from_slice(&bytes)?;
        if call.version != 1
            || call.id.is_empty()
            || call.id.len() > 128
            || call.run_key.is_empty()
            || call.run_key.len() > 256
        {
            bail!("Engine invocation rejected");
        }
        let result = if call.operation.as_deref() == Some("release") {
            if call.input.is_some() {
                bail!("Engine release arguments");
            }
            json!({"status": "released", "removed": false})
        } else {
            if call.operation.as_deref().is_some_and(|op| op != "prepare") {
                bail!("Engine operation rejected");
            }
            compute(
                call.input
                    .ok_or_else(|| anyhow::anyhow!("Engine request missing"))?,
            )?
        };
        let response = serde_json::to_vec(&Reply {
            version: 1,
            id: call.id,
            result,
        })?;
        if response.len() + 1 > limit {
            bail!("Engine response limit");
        }
        writer.write_all(&response)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;

    #[test]
    fn filter_checks_architecture_then_allows_listed_syscalls_and_denies_the_rest() {
        let program = filter(&[libc::SYS_epoll_wait]).expect("filter");
        let allowed = PIPE_SYSCALLS.len() + 1;
        assert_eq!(program.len(), 4 + 2 * allowed + 1);
        assert_eq!(
            (program[1].code, program[1].k),
            (JUMP_EQUAL, AUDIT_ARCH_X86_64)
        );
        assert_eq!((program[2].code, program[2].k), (RETURN, KILL_PROCESS));
        let numbers: Vec<u32> = program[4..program.len() - 1]
            .chunks(2)
            .map(|pair| {
                assert_eq!((pair[0].code, pair[0].jt, pair[0].jf), (JUMP_EQUAL, 0, 1));
                assert_eq!((pair[1].code, pair[1].k), (RETURN, ALLOW));
                pair[0].k
            })
            .collect();
        assert!(numbers.contains(&(libc::SYS_epoll_wait as u32)));
        assert!(!numbers.contains(&(libc::SYS_openat as u32)));
        assert!(!numbers.contains(&(libc::SYS_socket as u32)));
        assert_eq!(program.last().map(|rule| rule.k), Some(DENY_EPERM));
    }

    #[test]
    fn filter_rejects_a_negative_syscall_number() {
        assert!(filter(&[-1]).is_err());
    }
}
