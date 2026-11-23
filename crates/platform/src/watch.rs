//! Change notification for a folder tree, read without blocking.
//!
//! On Linux a [`Tree`] is an inotify instance with one watch per folder added through
//! [`Tree::add`]. The kernel queues an event as part of the file operation itself, so a
//! [`Tree::changed`] that starts after an edit returned always sees it: no thread, no timer.
//! Elsewhere [`Tree::new`] returns `None` and callers fall back to comparing file metadata.

#[cfg(target_os = "linux")]
pub use linux::Tree;

#[cfg(not(target_os = "linux"))]
pub use other::Tree;

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        ffi::CString,
        io,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
        path::Path,
    };

    /// What changes a folder's listing or a file's bytes or times: writes, metadata
    /// changes, creation, deletion and renames in the folder, and the folder itself going
    /// away or moving. Opens and reads are left out, so reading the tree is silent.
    const EVENTS: u32 = libc::IN_MODIFY
        | libc::IN_ATTRIB
        | libc::IN_CLOSE_WRITE
        | libc::IN_CREATE
        | libc::IN_DELETE
        | libc::IN_MOVED_FROM
        | libc::IN_MOVED_TO
        | libc::IN_DELETE_SELF
        | libc::IN_MOVE_SELF
        | libc::IN_ONLYDIR;

    /// One inotify instance; watches go away with it.
    pub struct Tree {
        fd: OwnedFd,
    }

    impl Tree {
        /// A new instance with no watches, or `None` when the system refuses one (for
        /// example the per-user instance limit).
        pub fn new() -> Option<Self> {
            // SAFETY: inotify_init1 takes only flags and returns a new descriptor or -1.
            let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
            if fd < 0 {
                return None;
            }
            // SAFETY: `fd` is a descriptor this call just opened and nothing else owns.
            Some(Self {
                fd: unsafe { OwnedFd::from_raw_fd(fd) },
            })
        }

        /// Watches `folder` (not its subfolders; add each). Adding a folder again keeps one
        /// watch. Fails when the folder is gone or the per-user watch limit is reached.
        pub fn add(&self, folder: &Path) -> io::Result<()> {
            let path = CString::new(folder.as_os_str().as_encoded_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
            let watch =
                unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), EVENTS) };
            if watch < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }

        /// Whether anything happened in a watched folder since the last call, emptying the
        /// queue. A queue overflow is a change too. Any read error but "nothing queued"
        /// also answers `true`, so a broken watch costs a recheck, never a missed edit.
        pub fn changed(&self) -> bool {
            let mut buffer = [0u8; 4096];
            let mut changed = false;
            loop {
                // SAFETY: the buffer is valid for `buffer.len()` writable bytes.
                let read = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                    )
                };
                if read > 0 {
                    changed = true;
                    continue;
                }
                if read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                let empty =
                    read < 0 && io::Error::last_os_error().kind() == io::ErrorKind::WouldBlock;
                return changed || !empty;
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod other {
    use std::{io, path::Path};

    /// No change notification on this platform.
    pub struct Tree;

    impl Tree {
        pub fn new() -> Option<Self> {
            None
        }
        pub fn add(&self, _folder: &Path) -> io::Result<()> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn changed(&self) -> bool {
            true
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::Tree;

    #[test]
    fn edits_creations_deletions_and_renames_are_seen_and_reads_are_not() {
        let dir = std::env::temp_dir().join(format!("platform-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::write(dir.join("a/f.json"), "1").unwrap();
        let tree = Tree::new().unwrap();
        tree.add(&dir).unwrap();
        tree.add(&dir.join("a")).unwrap();
        assert!(!tree.changed(), "nothing happened yet");
        let _ = std::fs::read(dir.join("a/f.json")).unwrap();
        let _ = std::fs::read_dir(dir.join("a")).unwrap().count();
        assert!(!tree.changed(), "reading is not a change");
        std::fs::write(dir.join("a/f.json"), "2").unwrap();
        assert!(tree.changed(), "an edit");
        assert!(!tree.changed(), "the queue was emptied");
        std::fs::write(dir.join("a/g.md"), "new").unwrap();
        assert!(tree.changed(), "a new file");
        std::fs::remove_file(dir.join("a/g.md")).unwrap();
        assert!(tree.changed(), "a deleted file");
        std::fs::rename(dir.join("a"), dir.join("b")).unwrap();
        assert!(tree.changed(), "a renamed folder");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
