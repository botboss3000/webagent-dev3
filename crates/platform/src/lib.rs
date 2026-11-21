//! Unix file and path extension traits under the same names on every platform.
//!
//! On Unix these are the standard library's traits. Elsewhere (Windows) they are
//! stand-ins for a single-user install: the current user owns every file (owner and
//! group 0), modes follow the read-only flag, and device and inode numbers are 0, so
//! code that compares them treats every file alike. Code that needs the real Unix
//! security model (workspace sandbox, UID checks between processes) stays Unix-only.

pub mod fs {
    #[cfg(unix)]
    pub use std::os::unix::fs::{
        symlink, DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt,
    };

    #[cfg(not(unix))]
    pub use self::other::*;

    #[cfg(not(unix))]
    mod other {
        use std::time::UNIX_EPOCH;

        fn since_epoch(time: std::io::Result<std::time::SystemTime>) -> (i64, i64) {
            time.ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or((0, 0), |since| {
                    (since.as_secs() as i64, i64::from(since.subsec_nanos()))
                })
        }

        fn mode_of(directory: bool, readonly: bool) -> u32 {
            let kind = if directory { 0o040_000 } else { 0o100_000 };
            let access = match (directory, readonly) {
                (true, _) => 0o700,
                (false, true) => 0o400,
                (false, false) => 0o600,
            };
            kind | access
        }

        pub trait MetadataExt {
            fn dev(&self) -> u64;
            fn ino(&self) -> u64;
            fn mode(&self) -> u32;
            fn nlink(&self) -> u64;
            fn uid(&self) -> u32;
            fn gid(&self) -> u32;
            fn mtime(&self) -> i64;
            fn mtime_nsec(&self) -> i64;
            fn ctime(&self) -> i64;
            fn ctime_nsec(&self) -> i64;
        }

        impl MetadataExt for std::fs::Metadata {
            fn dev(&self) -> u64 {
                0
            }
            fn ino(&self) -> u64 {
                0
            }
            fn mode(&self) -> u32 {
                mode_of(self.is_dir(), self.permissions().readonly())
            }
            fn nlink(&self) -> u64 {
                1
            }
            fn uid(&self) -> u32 {
                0
            }
            fn gid(&self) -> u32 {
                0
            }
            fn mtime(&self) -> i64 {
                since_epoch(self.modified()).0
            }
            fn mtime_nsec(&self) -> i64 {
                since_epoch(self.modified()).1
            }
            fn ctime(&self) -> i64 {
                self.mtime()
            }
            fn ctime_nsec(&self) -> i64 {
                self.mtime_nsec()
            }
        }

        pub trait PermissionsExt {
            fn mode(&self) -> u32;
            fn set_mode(&mut self, mode: u32);
            fn from_mode(mode: u32) -> Self;
        }

        impl PermissionsExt for std::fs::Permissions {
            fn mode(&self) -> u32 {
                mode_of(false, self.readonly())
            }
            fn set_mode(&mut self, mode: u32) {
                self.set_readonly(mode & 0o200 == 0);
            }
            fn from_mode(mode: u32) -> Self {
                // `Permissions` has no constructor off Unix; a temporary file's carries
                // the platform default, then the read-only flag follows `mode`.
                let mut permissions = std::env::temp_dir()
                    .metadata()
                    .map(|info| info.permissions())
                    .expect("temporary folder metadata");
                permissions.set_readonly(mode & 0o200 == 0);
                permissions
            }
        }

        pub trait OpenOptionsExt {
            fn mode(&mut self, mode: u32) -> &mut Self;
            fn custom_flags(&mut self, flags: i32) -> &mut Self;
        }

        impl OpenOptionsExt for std::fs::OpenOptions {
            fn mode(&mut self, _mode: u32) -> &mut Self {
                self
            }
            fn custom_flags(&mut self, _flags: i32) -> &mut Self {
                self
            }
        }

        pub trait DirBuilderExt {
            fn mode(&mut self, mode: u32) -> &mut Self;
        }

        impl DirBuilderExt for std::fs::DirBuilder {
            fn mode(&mut self, _mode: u32) -> &mut Self {
                self
            }
        }

        pub trait FileTypeExt {
            fn is_socket(&self) -> bool;
        }

        impl FileTypeExt for std::fs::FileType {
            fn is_socket(&self) -> bool {
                false
            }
        }
    }
}

/// Make a folder's entries (a new or renamed file) durable. Unix syncs the folder;
/// Windows cannot open a folder as a file and NTFS journals its entries, so it does nothing.
pub fn sync_dir(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::File::open(path)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Give `original` a second name `link`, failing like `hard_link` (for example
/// `AlreadyExists`). Where links are forbidden (the Android app sandbox) the file is
/// copied instead; callers only use this for files that are not changed afterwards.
pub fn link_or_copy(original: &std::path::Path, link: &std::path::Path) -> std::io::Result<()> {
    match std::fs::hard_link(original, link) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::Unsupported
            ) =>
        {
            // Copy beside the target first, so a crash never leaves a partial file there.
            if link.exists() {
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            let mut partial = link.as_os_str().to_owned();
            partial.push(".partial");
            let partial = std::path::PathBuf::from(partial);
            std::fs::copy(original, &partial)?;
            if link.exists() {
                let _ = std::fs::remove_file(&partial);
                return Err(std::io::ErrorKind::AlreadyExists.into());
            }
            std::fs::rename(&partial, link)
        }
        result => result,
    }
}

pub mod process {
    /// Raise this process's soft open-file limit to its hard limit and return the new soft
    /// limit. Each user's database holds files for every pooled connection (database, WAL and
    /// shared memory), so a server with hundreds of accounts needs far more than the 1024
    /// that containers start with; past it, opens fail with "unable to open database file".
    /// Returns `None` where the limit cannot be read or changed (and on non-Unix systems).
    #[cfg(unix)]
    pub fn raise_open_file_limit() -> Option<u64> {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit and setrlimit only read and write the struct passed to them.
        unsafe {
            if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
                return None;
            }
            if limit.rlim_cur < limit.rlim_max {
                let raised = libc::rlimit {
                    rlim_cur: limit.rlim_max,
                    rlim_max: limit.rlim_max,
                };
                if libc::setrlimit(libc::RLIMIT_NOFILE, &raised) == 0 {
                    limit.rlim_cur = limit.rlim_max;
                }
            }
        }
        Some(limit.rlim_cur as u64)
    }

    #[cfg(not(unix))]
    pub fn raise_open_file_limit() -> Option<u64> {
        None
    }
}

pub mod ffi {
    #[cfg(unix)]
    pub use std::os::unix::ffi::OsStrExt;

    /// The platform's encoded bytes (WTF-8 on Windows), equal to the Unix bytes for
    /// valid UTF-8 names.
    #[cfg(not(unix))]
    pub trait OsStrExt {
        fn as_bytes(&self) -> &[u8];
    }

    #[cfg(not(unix))]
    impl OsStrExt for std::ffi::OsStr {
        fn as_bytes(&self) -> &[u8] {
            self.as_encoded_bytes()
        }
    }
}

pub mod ipc {
    //! Local connections to a sibling process: Unix sockets on Unix, named pipes on
    //! Windows, where a socket path maps to the pipe [`pipe_name`] gives it.
    use std::{io, path::Path};

    #[cfg(unix)]
    pub type LocalStream = tokio::net::UnixStream;
    #[cfg(windows)]
    pub type LocalStream = tokio::net::windows::named_pipe::NamedPipeClient;

    #[cfg(unix)]
    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        tokio::net::UnixStream::connect(path).await
    }

    #[cfg(windows)]
    pub async fn connect(path: &Path) -> io::Result<LocalStream> {
        tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name(path))
    }

    /// The pipe standing for a socket path: `\\.\pipe\webagent-` and the path with every
    /// character outside `[A-Za-z0-9._-]` turned into `-`, cut to the 256-character limit.
    #[must_use]
    pub fn pipe_name(path: &Path) -> String {
        let flat: String = path
            .to_string_lossy()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        let mut name = format!(r"\\.\pipe\webagent-{flat}");
        name.truncate(256);
        name
    }
}

pub mod signal {
    //! The service manager's stop request: SIGTERM on Unix. Windows has no such signal
    //! for a background app; its window or Ctrl-C (handled by callers) stops it.

    #[cfg(unix)]
    pub struct Terminate(tokio::signal::unix::Signal);
    #[cfg(not(unix))]
    pub struct Terminate;

    /// Starts listening for the stop request.
    pub fn terminate() -> std::io::Result<Terminate> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            Ok(Terminate(signal(SignalKind::terminate())?))
        }
        #[cfg(not(unix))]
        {
            Ok(Terminate)
        }
    }

    impl Terminate {
        /// Waits for the next stop request.
        pub async fn recv(&mut self) {
            #[cfg(unix)]
            {
                self.0.recv().await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn open_file_limit_is_raised_to_the_hard_limit() {
        let raised = super::process::raise_open_file_limit().expect("limit readable");
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: getrlimit only writes the struct passed to it.
        assert_eq!(
            unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) },
            0
        );
        assert_eq!(raised, limit.rlim_cur as u64);
        assert_eq!(limit.rlim_cur, limit.rlim_max);
    }

    #[test]
    fn link_or_copy_names_the_file_once() {
        let dir = std::env::temp_dir().join(format!("platform-link-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (original, link) = (dir.join("original"), dir.join("link"));
        std::fs::write(&original, b"bytes").unwrap();
        let _ = std::fs::remove_file(&link);
        super::link_or_copy(&original, &link).unwrap();
        assert_eq!(std::fs::read(&link).unwrap(), b"bytes");
        let again = super::link_or_copy(&original, &link).unwrap_err();
        assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
