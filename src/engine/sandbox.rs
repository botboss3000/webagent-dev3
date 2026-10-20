//! Host-enforced boundary installed before replaceable engine code executes.
//! Linux x86-64 only; unsupported kernels fail closed. No child allocation/locks.
use std::{fs::{File, OpenOptions}, io, os::fd::{AsRawFd, FromRawFd}, path::Path};
use std::os::unix::fs::OpenOptionsExt;

unsafe extern "C" {
    fn syscall(number: i64, ...) -> i64;
    fn prctl(option: i32, ...) -> i32;
}
#[repr(C)] struct Ruleset { handled: u64 }
#[repr(C, packed)] struct PathRule { allowed: u64, parent: i32 }
#[repr(C)] #[derive(Clone, Copy)] struct Filter { code: u16, jt: u8, jf: u8, k: u32 }
#[repr(C)] struct Limit { current: u64, maximum: u64 }
#[repr(C)] struct Program { len: u16, filter: *const Filter }

pub struct PreparedSandbox { ruleset: File, filter: Vec<Filter> }
/// Preserve syscall failure as a host error so incomplete confinement never launches.
fn checked(value: i64) -> io::Result<i64> {
    if value < 0 { Err(io::Error::last_os_error()) } else { Ok(value) }
}
/// Grant only the prepared loader or artifact path rights through a held descriptor.
fn rule(ruleset: &File, path: &Path, allowed: u64) -> io::Result<()> {
    let file = OpenOptions::new().read(true).custom_flags(0x200000 | 0x80000).open(path)?;
    let attr = PathRule { allowed, parent: file.as_raw_fd() };
    checked(unsafe { syscall(445, ruleset.as_raw_fd(), 1i32, &attr as *const PathRule, 0u32) })?;
    Ok(())
}
/// Parent-only preparation. The caller must validate the immutable published
/// executable's identity before this call and preserve it through spawn.
pub fn prepare(program: &Path) -> io::Result<PreparedSandbox> {
    if !cfg!(all(target_os="linux", target_arch="x86_64")) {
        return Err(io::Error::new(io::ErrorKind::Unsupported, "engine sandbox requires Linux x86-64"));
    }
    let abi = checked(unsafe { syscall(444, std::ptr::null::<Ruleset>(), 0usize, 1u32) })?;
    if abi < 3 { return Err(io::Error::new(io::ErrorKind::Unsupported, "engine sandbox requires Landlock ABI 3")); }
    let attr = Ruleset { handled: (1 << 15) - 1 };
    let fd = checked(unsafe { syscall(444, &attr as *const Ruleset, std::mem::size_of::<Ruleset>(), 0u32) })?;
    let ruleset = unsafe { File::from_raw_fd(fd as i32) };
    rule(&ruleset, program, 1 | 4)?; // only approved program may execute
    for library_dir in ["/usr/lib", "/lib", "/lib64"] {
        let path = Path::new(library_dir);
        if path.exists() { rule(&ruleset, path, 4 | 8)?; }
    }
    // Kernel resolves the ELF interpreter separately; no directory EXEC grants.
    rule(&ruleset, Path::new("/lib64/ld-linux-x86-64.so.2"), 1 | 4)?;
    if Path::new("/etc/ld.so.cache").exists() { rule(&ruleset, Path::new("/etc/ld.so.cache"), 4)?; }
    let mut filter = vec![
        Filter { code: 0x20, jt: 0, jf: 0, k: 4 }, // audit architecture
        Filter { code: 0x15, jt: 1, jf: 0, k: 0xc000003e },
        Filter { code: 0x06, jt: 0, jf: 0, k: 0x80000000 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 0 }, // syscall number
    ];
    // Only tightening prctl operations are exposed to the post-exec service.
    filter.extend([
        Filter { code: 0x15, jt: 0, jf: 5, k: 157 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 16 },
        Filter { code: 0x15, jt: 2, jf: 0, k: 38 },
        Filter { code: 0x15, jt: 1, jf: 0, k: 22 },
        Filter { code: 0x06, jt: 0, jf: 0, k: 0x00050001 },
        Filter { code: 0x06, jt: 0, jf: 0, k: 0x7fff0000 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 0 },
    ]);
    // prlimit64 is needed by Rust/glibc startup. Permit read-only self queries;
    // inspecting or changing another process (including the host) is forbidden.
    filter.extend([
        Filter { code: 0x15, jt: 0, jf: 10, k: 302 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 16 },
        Filter { code: 0x15, jt: 0, jf: 7, k: 0 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 20 },
        Filter { code: 0x15, jt: 0, jf: 5, k: 0 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 32 },
        Filter { code: 0x15, jt: 0, jf: 3, k: 0 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 36 },
        Filter { code: 0x15, jt: 0, jf: 1, k: 0 },
        Filter { code: 0x06, jt: 0, jf: 0, k: 0x7fff0000 },
        Filter { code: 0x06, jt: 0, jf: 0, k: 0x00050001 },
        Filter { code: 0x20, jt: 0, jf: 0, k: 0 },
    ]);
    // Positive syscall allowlist. x32 numbers, socket/socketpair, fork/clone,
    // ptrace, process_vm_*, io_uring, mount and seccomp changes are absent.
    // execve is constrained by Landlock to the exact approved executable.
    for nr in [0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,17,19,20,21,24,
        25,28,35,39,59,60,63,72,96,97,102,104,107,108,110,131,158,
        186,202,218,228,230,231,257,262,273,318,334] {
        filter.push(Filter { code: 0x15, jt: 0, jf: 1, k: nr });
        filter.push(Filter { code: 0x06, jt: 0, jf: 0, k: 0x7fff0000 });
    }
    filter.push(Filter { code: 0x06, jt: 0, jf: 0, k: 0x00050001 }); // EPERM
    Ok(PreparedSandbox { ruleset, filter })
}
impl PreparedSandbox {
    /// Call only inside Command::pre_exec. Every operation here is a raw syscall;
    /// all memory, descriptors and BPF instructions were prepared in the parent.
    pub unsafe fn apply(&self) -> io::Result<()> {
        // CLOEXEC preserves Rust's private exec-error pipe until successful exec,
        // while ensuring no ambient host descriptors reach replacement code.
        checked(syscall(436, 3u32, u32::MAX, 4u32))?;
        let memory = Limit { current: 512 * 1024 * 1024, maximum: 512 * 1024 * 1024 };
        let descriptors = Limit { current: 32, maximum: 32 };
        checked(syscall(160, 9i32, &memory as *const Limit))?;
        checked(syscall(160, 7i32, &descriptors as *const Limit))?;
        checked(prctl(38, 1i64, 0i64, 0i64, 0i64) as i64)?;
        checked(syscall(446, self.ruleset.as_raw_fd(), 0u32))?;
        let program = Program { len: self.filter.len() as u16, filter: self.filter.as_ptr() };
        checked(prctl(22, 2i64, &program as *const Program, 0i64, 0i64) as i64)?;
        Ok(())
    }
}

#[cfg(test)] mod tests {
    use super::*;
    use std::{os::unix::process::CommandExt, process::{Command, Stdio}};
    /// Launch probes through the same host pre-exec boundary as the replaceable service.
    fn run(program: &Path, args: &[&str]) -> io::Result<std::process::Output> {
        let sandbox = prepare(program)?;
        let mut command = Command::new(program);
        command.args(args).env_clear().current_dir("/").stdin(Stdio::null());
        unsafe { command.pre_exec(move || sandbox.apply()); }
        command.output()
    }
    #[test] fn approved_program_starts_but_cannot_read_application_state() {
        assert!(run(Path::new("/bin/cat"), &["/etc/passwd"]).unwrap().status.success() == false);
        assert!(run(Path::new("/bin/true"), &[]).unwrap().status.success());
    }
    #[test] fn host_boundary_rejects_malicious_native_code() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("probe.c");
        let executable = temp.path().join("probe");
        std::fs::write(&source, r#"
#include <unistd.h>
#include <sys/socket.h>
#include <sys/syscall.h>
#include <fcntl.h>
#include <errno.h>
#include <sys/resource.h>
#include <sys/prctl.h>
int main(void) {
 for(int fd=3;fd<200;fd++) { if(fcntl(fd,F_GETFD)!=-1 || errno!=EBADF) return 14; }
 if(open("/etc/passwd", O_RDONLY)>=0) return 1;
 if(open("/tmp/engine-sandbox-escape", O_WRONLY|O_CREAT,0600)>=0) return 2;
 if(socket(AF_INET, SOCK_STREAM,0)>=0) return 3;
 if(socket(AF_UNIX, SOCK_STREAM,0)>=0) return 4;
 if(fork()>=0) return 5;
 if(syscall(0x40000000|SYS_getpid)!=-1 || errno!=EPERM) return 6;
 char *args[]={"/bin/true",0}; char *env[]={0};
 if(execve(args[0],args,env)!=-1) return 7;
 struct rlimit limit = {1,1};
 if(prlimit(getppid(), RLIMIT_NOFILE, &limit,0)!=-1 || errno!=EPERM) return 8;
 if(prlimit(0, RLIMIT_NOFILE, &limit,0)!=-1 || errno!=EPERM) return 9;
 if(prctl(PR_SET_DUMPABLE, 1)!=-1 || errno!=EPERM) return 10;
 if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)!=0) return 11;
 if(getrlimit(RLIMIT_AS,&limit)!=0 || limit.rlim_max!=512*1024*1024) return 12;
 if(getrlimit(RLIMIT_NOFILE,&limit)!=0 || limit.rlim_max!=32) return 13;
 return 0;
}"#).unwrap();
        assert!(Command::new("cc").arg(&source).arg("-o").arg(&executable).status().unwrap().success());
        let ambient = File::open("/etc/passwd").unwrap();
        let result = run(&executable, &[]).unwrap();
        drop(ambient);
        assert!(result.status.success(), "probe failed: {:?} {:?}", result.status, result.stderr);
    }
}
