//! Root compatibility names for the Engine-owned confinement boundary.
pub use webagent_engine::sandbox::*;
#[cfg(test)] use std::{fs::File,io,path::Path};

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
