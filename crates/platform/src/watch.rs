//! Change notification for a folder tree, read without blocking.
//!
//! On Linux a [`Tree`] is an inotify instance with one watch per folder added through
//! [`Tree::add`]. The kernel queues an event as part of the file operation itself, so a
//! [`Tree::changed`] that starts after an edit returned always sees it: no thread, no timer.
//! Elsewhere [`Tree::new`] returns `None` and callers fall back to comparing file metadata.
//!
//! [`Marks`] builds on it: each file's length and modification time, read from the file
//! system only when its folder reported a change since the last read.

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
            self.watch(folder).map(|_| ())
        }

        /// As [`Tree::add`], returning the watch's number, which [`Tree::drain`] reports.
        pub fn watch(&self, folder: &Path) -> io::Result<i32> {
            let path = CString::new(folder.as_os_str().as_encoded_bytes())
                .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
            // SAFETY: `path` is a valid NUL-terminated string that outlives the call.
            let watch =
                unsafe { libc::inotify_add_watch(self.fd.as_raw_fd(), path.as_ptr(), EVENTS) };
            if watch < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(watch)
        }

        /// Empties the queue, calling `event(watch, name, gone)` for each event: `name` is
        /// the file's name in the folder (empty for the folder itself), `gone` that the watch
        /// ended (the folder was deleted or moved). Returns `false` when events were lost (a
        /// queue overflow or a read error but "nothing queued"): then anything may have
        /// changed.
        pub fn drain(&self, mut event: impl FnMut(i32, &[u8], bool)) -> bool {
            let mut buffer = [0u8; 8192];
            let header = std::mem::size_of::<libc::inotify_event>();
            let field = |bytes: &[u8], at: usize| {
                u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            loop {
                // SAFETY: the buffer is valid for `buffer.len()` writable bytes.
                let read = unsafe {
                    libc::read(
                        self.fd.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                    )
                };
                if read < 0 {
                    let error = io::Error::last_os_error().kind();
                    if error == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return error == io::ErrorKind::WouldBlock;
                }
                if read == 0 {
                    return false;
                }
                // The kernel writes whole events: `wd`, `mask`, `cookie`, `len`, then `len`
                // bytes of NUL-padded name.
                let bytes = &buffer[..read as usize];
                let mut at = 0;
                while at + header <= bytes.len() {
                    let wd = field(bytes, at) as i32;
                    let mask = field(bytes, at + 4);
                    let len = field(bytes, at + 12) as usize;
                    let Some(name) = bytes.get(at + header..at + header + len) else {
                        return false;
                    };
                    if mask & libc::IN_Q_OVERFLOW != 0 {
                        return false;
                    }
                    let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(len)];
                    let gone =
                        mask & (libc::IN_IGNORED | libc::IN_DELETE_SELF | libc::IN_MOVE_SELF) != 0;
                    event(wd, name, gone);
                    at += header + len;
                }
            }
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
        pub fn watch(&self, _folder: &Path) -> io::Result<i32> {
            Err(io::Error::from(io::ErrorKind::Unsupported))
        }
        pub fn changed(&self) -> bool {
            true
        }
        pub fn drain(&self, _event: impl FnMut(i32, &[u8], bool)) -> bool {
            false
        }
    }
}

/// A file's length and modification time; `None` when it is absent or empty.
pub type Mark = Option<(u64, std::time::SystemTime)>;

/// After a refused watch (the per-user watch limit), how long before a folder is tried again.
const REFUSED_RETRY: std::time::Duration = std::time::Duration::from_secs(60);

/// File marks ([`Mark`]) of files in watched folders, read from the file system only after
/// a change. Each folder asked about gets one watch, added **before** its files are read, so
/// a change after the read is queued; every [`Marks::mark`] first empties the queue and forgets
/// what changed (the named file, or the whole folder for an event on the folder itself), so
/// a change completed before the call is always seen. Lost events forget everything. A folder
/// that cannot be watched (no change notification on this platform, the watch limit, a
/// missing folder) is read on every call, as without this cache.
pub struct Marks {
    state: std::sync::Mutex<State>,
    reads: std::sync::atomic::AtomicU64,
}

struct State {
    tree: Option<Tree>,
    by_watch: std::collections::HashMap<i32, std::path::PathBuf>,
    folders: std::collections::HashMap<std::path::PathBuf, Folder>,
    refused_at: Option<std::time::Instant>,
    refusals: u64,
}

struct Folder {
    files: std::collections::HashMap<Vec<u8>, Mark>,
}

impl Default for Marks {
    fn default() -> Self {
        Self::new()
    }
}

impl Marks {
    /// A cache with change notification where the platform has it.
    pub fn new() -> Self {
        Self::with(Tree::new())
    }

    /// A cache that reads every file on every call (as when watching is refused).
    pub fn unwatched() -> Self {
        Self::with(None)
    }

    fn with(tree: Option<Tree>) -> Self {
        Self {
            state: std::sync::Mutex::new(State {
                tree,
                by_watch: Default::default(),
                folders: Default::default(),
                refused_at: None,
                refusals: 0,
            }),
            reads: Default::default(),
        }
    }

    /// File-system reads made so far (for "nothing is read when nothing changed").
    pub fn reads(&self) -> u64 {
        self.reads.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Folders that could not be watched because the system refused (the watch limit).
    pub fn refusals(&self) -> u64 {
        self.state.lock().map_or(0, |state| state.refusals)
    }

    fn read(&self, file: &std::path::Path) -> Mark {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::fs::metadata(file)
            .ok()
            .filter(|m| m.len() > 0)
            .and_then(|m| Some((m.len(), m.modified().ok()?)))
    }

    /// The mark of `name` in `folder`, as the file system has it now.
    pub fn mark(&self, folder: &std::path::Path, name: &str) -> Mark {
        let Ok(mut state) = self.state.lock() else {
            return self.read(&folder.join(name));
        };
        state.drain();
        if let Some(mark) = state
            .folders
            .get(folder)
            .and_then(|f| f.files.get(name.as_bytes()))
        {
            return *mark;
        }
        let watched = state.watch(folder);
        let mark = self.read(&folder.join(name));
        if watched {
            if let Some(f) = state.folders.get_mut(folder) {
                f.files.insert(name.as_bytes().to_vec(), mark);
            }
        }
        mark
    }
}

impl State {
    /// Applies the queued events: a named file is forgotten, an event on the folder itself
    /// forgets the folder, an ended watch drops it, lost events forget everything.
    fn drain(&mut self) {
        let Some(tree) = &self.tree else {
            return;
        };
        let mut events = Vec::new();
        let complete = tree.drain(|watch, name, gone| events.push((watch, name.to_vec(), gone)));
        for (watch, name, gone) in events {
            let Some(path) = self.by_watch.get(&watch) else {
                continue;
            };
            if gone {
                let path = path.clone();
                self.by_watch.remove(&watch);
                self.folders.remove(&path);
            } else if let Some(folder) = self.folders.get_mut(path) {
                if name.is_empty() {
                    folder.files.clear();
                } else {
                    folder.files.remove(&name);
                }
            }
        }
        if !complete {
            for folder in self.folders.values_mut() {
                folder.files.clear();
            }
        }
    }

    /// Whether `folder` is watched, adding the watch if it is not.
    fn watch(&mut self, folder: &std::path::Path) -> bool {
        if self.folders.contains_key(folder) {
            return true;
        }
        let Some(tree) = &self.tree else {
            return false;
        };
        if self
            .refused_at
            .is_some_and(|at| at.elapsed() < REFUSED_RETRY)
        {
            return false;
        }
        match tree.watch(folder) {
            Ok(watch) => {
                // A folder reached by another path shares the watch: keep one name for it.
                if let Some(old) = self.by_watch.insert(watch, folder.to_path_buf()) {
                    self.folders.remove(&old);
                }
                self.folders.insert(
                    folder.to_path_buf(),
                    Folder {
                        files: Default::default(),
                    },
                );
                true
            }
            Err(error) => {
                // A missing folder is simply read; a refusal (the watch limit) waits a while.
                if error.kind() == std::io::ErrorKind::StorageFull {
                    self.refused_at = Some(std::time::Instant::now());
                    self.refusals += 1;
                }
                false
            }
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

    #[test]
    fn marks_are_read_again_only_after_a_change_and_every_change_is_seen() {
        use super::Marks;
        let dir = std::env::temp_dir().join(format!("platform-marks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let user = dir.join("u1");
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("u1.db"), "db").unwrap();
        let marks = Marks::new();
        let db = marks.mark(&user, "u1.db");
        let wal = marks.mark(&user, "u1.db-wal");
        assert!(db.is_some() && wal.is_none());
        let reads = marks.reads();
        for _ in 0..5 {
            assert_eq!(marks.mark(&user, "u1.db"), db);
            assert_eq!(marks.mark(&user, "u1.db-wal"), None);
        }
        assert_eq!(marks.reads(), reads, "nothing is read when nothing changed");
        // A write to the log, then to the database, each seen by the next call.
        std::fs::write(user.join("u1.db-wal"), "commit").unwrap();
        assert_eq!(marks.mark(&user, "u1.db-wal").map(|m| m.0), Some(6));
        assert_eq!(
            marks.mark(&user, "u1.db"),
            db,
            "only the written file is read"
        );
        std::fs::write(user.join("u1.db"), "database").unwrap();
        assert_eq!(marks.mark(&user, "u1.db").map(|m| m.0), Some(8));
        // A file the folder does not care about forgets nothing else.
        let reads = marks.reads();
        std::fs::write(user.join("u1.db-shm"), "x").unwrap();
        marks.mark(&user, "u1.db");
        marks.mark(&user, "u1.db-wal");
        assert_eq!(marks.reads(), reads);
        // A checkpoint empties the log; a deleted log is absent.
        std::fs::write(user.join("u1.db-wal"), "").unwrap();
        assert_eq!(marks.mark(&user, "u1.db-wal"), None);
        std::fs::write(user.join("u1.db-wal"), "again").unwrap();
        assert!(marks.mark(&user, "u1.db-wal").is_some());
        std::fs::remove_file(user.join("u1.db-wal")).unwrap();
        assert_eq!(marks.mark(&user, "u1.db-wal"), None);
        // A replaced folder is watched again.
        std::fs::remove_dir_all(&user).unwrap();
        assert_eq!(marks.mark(&user, "u1.db"), None);
        std::fs::create_dir_all(&user).unwrap();
        std::fs::write(user.join("u1.db"), "restored").unwrap();
        assert_eq!(marks.mark(&user, "u1.db").map(|m| m.0), Some(8));
        std::fs::write(user.join("u1.db"), "restored again").unwrap();
        assert_eq!(marks.mark(&user, "u1.db").map(|m| m.0), Some(14));
        // Unwatched (a refused watch): read on every call, still correct.
        let plain = Marks::unwatched();
        let before = plain.reads();
        assert_eq!(plain.mark(&user, "u1.db").map(|m| m.0), Some(14));
        assert_eq!(plain.mark(&user, "u1.db").map(|m| m.0), Some(14));
        assert_eq!(plain.reads(), before + 2);
        std::fs::write(user.join("u1.db"), "x").unwrap();
        assert_eq!(plain.mark(&user, "u1.db").map(|m| m.0), Some(1));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
