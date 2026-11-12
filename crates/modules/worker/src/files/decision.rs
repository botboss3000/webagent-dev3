//! `decision_candidates`: one page of directory entries for the native directory-decision
//! tool, optionally filtered by a basename glob, walked recursively and given bounded
//! excerpts. Ported from `worker.py` `file_operation`; symlinks are never followed while
//! collecting candidates for an external model.
use super::{
    confine, glob_matches, open_at, rejected, resolve, Outcome, Roots, CHECKOUT_HIDDEN, DIRECTORY,
};
use rustix::fs::{fstat, openat, statat, AtFlags, Dir, FileType, Mode, OFlags, CWD};
use serde_json::{json, Map, Value};
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::os::fd::OwnedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

const PAGE: usize = 32;
const EXAMINED_LIMIT: usize = 10_000;
const EXCERPT: u64 = 1024;

struct Walk<'a> {
    file_glob: Option<&'a str>,
    recursive: bool,
    names: Vec<(String, &'static str)>,
    examined: usize,
    limited: bool,
    /// The walk starts at the checkout root, whose runtime folders it skips.
    checkout_top: bool,
}

impl Walk<'_> {
    fn walk(&mut self, folder: &OwnedFd, prefix: &str, depth: usize) -> Outcome<()> {
        for entry in Dir::read_from(folder)? {
            let entry = entry?;
            let raw = entry.file_name().to_bytes();
            if raw == b"." || raw == b".." {
                continue;
            }
            if self.examined >= EXAMINED_LIMIT {
                self.limited = true;
                return Ok(());
            }
            self.examined += 1;
            let Ok(name) = std::str::from_utf8(raw) else {
                self.limited = true;
                continue;
            };
            if depth == 0 && self.checkout_top && CHECKOUT_HIDDEN.contains(&name) {
                continue;
            }
            let relative = format!("{prefix}{name}");
            if relative.len() > 512 {
                self.limited = true;
                continue;
            }
            let kind = match statat(folder, OsStr::from_bytes(raw), AtFlags::SYMLINK_NOFOLLOW)
                .map(|stat| FileType::from_raw_mode(stat.st_mode))
            {
                Ok(FileType::RegularFile) => "file",
                Ok(FileType::Directory) => "directory",
                _ => "other",
            };
            let wanted = match self.file_glob {
                None => true,
                Some(glob) => kind == "file" && glob_matches(name, glob),
            };
            if wanted {
                self.names.push((relative.clone(), kind));
            }
            if self.recursive && kind == "directory" {
                if depth >= 8 {
                    self.limited = true;
                    continue;
                }
                let child = open_at(folder, OsStr::from_bytes(raw), DIRECTORY)?;
                self.walk(&child, &format!("{relative}/"), depth + 1)?;
            }
        }
        Ok(())
    }
}

/// The first kilobyte of `relative` under `folder`, and whether the file is longer.
fn excerpt(folder: &OwnedFd, relative: &str) -> Outcome<(String, bool)> {
    let parts: Vec<&str> = relative.split('/').collect();
    let (leaf, folders) = parts
        .split_last()
        .ok_or_else(|| rejected("empty candidate"))?;
    let mut current = folder.try_clone()?;
    for part in folders {
        current = open_at(&current, OsStr::new(part), DIRECTORY)?;
    }
    let mut file = File::from(open_at(
        &current,
        OsStr::new(leaf),
        OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW,
    )?);
    let stat = fstat(&file)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(rejected("Candidate changed during inspection"));
    }
    let mut bytes = Vec::new();
    (&mut file).take(EXCERPT).read_to_end(&mut bytes)?;
    let longer = stat.st_size > bytes.len() as i64;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), longer))
}

/// Runs one `decision_candidates` call in `base`.
pub(super) fn candidates(
    roots: &Roots,
    base: &Path,
    args: &Map<String, Value>,
    granted: bool,
) -> Outcome<Value> {
    let raw = match args.get("path") {
        None => ".",
        Some(Value::String(raw)) => raw.as_str(),
        Some(_) => return Err(rejected("path must be text")),
    };
    let live = granted && roots.live_checkout.is_dir();
    let default_root = if live { &roots.live_checkout } else { base };
    let target = resolve(&default_root.join(raw));
    let checkout_top = confine(roots, base, &target, live, granted)?;
    let parts: Vec<&OsStr> = target
        .components()
        .filter_map(|part| match part {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    let mut directory = openat(CWD, "/", DIRECTORY, Mode::empty())?;
    let (leaf, folders) = match parts.split_last() {
        Some((leaf, folders)) => (*leaf, folders),
        None => (OsStr::new("."), &[][..]),
    };
    for part in folders {
        directory = open_at(&directory, part, DIRECTORY)?;
    }
    let offset = match args.get("offset") {
        None => Some(0),
        Some(value) => value.as_u64().filter(|offset| *offset <= 10_000),
    }
    .ok_or_else(|| rejected("offset must be between 0 and 10000"))?;
    let file_glob = match args.get("file_glob") {
        None | Some(Value::Null) => None,
        Some(Value::String(glob))
            if !glob.is_empty()
                && glob.chars().count() <= 128
                && !glob.contains(['/', '\\', '\0']) =>
        {
            Some(glob.as_str())
        }
        Some(_) => {
            return Err(rejected(
                "file_glob must be a basename pattern of at most 128 characters",
            ))
        }
    };
    let flag = |key: &str| match args.get(key) {
        None => Some(false),
        Some(value) => value.as_bool(),
    };
    let (Some(recursive), Some(include_content)) = (flag("recursive"), flag("include_content"))
    else {
        return Err(rejected("recursive and include_content must be booleans"));
    };
    if (recursive || include_content) && file_glob.is_none() {
        return Err(rejected("recursive and include_content require file_glob"));
    }
    let folder = open_at(&directory, leaf, DIRECTORY)?;
    let mut walk = Walk {
        file_glob,
        recursive,
        names: Vec::new(),
        examined: 0,
        limited: false,
        checkout_top,
    };
    walk.walk(&folder, "", 0)?;
    walk.names.sort();
    let start = usize::try_from(offset)
        .unwrap_or(usize::MAX)
        .min(walk.names.len());
    let end = (start + PAGE).min(walk.names.len());
    let mut page = Vec::with_capacity(end - start);
    for (name, kind) in &walk.names[start..end] {
        let mut item = json!({"name":name,"kind":kind});
        if include_content {
            let (snippet, longer) = excerpt(&folder, name)?;
            item["snippet"] = json!(snippet);
            item["content_truncated"] = json!(longer);
        }
        page.push(item);
    }
    let next = (end < walk.names.len()).then_some(end);
    Ok(
        json!({"status":"success","output":page,"offset":offset,"next_offset":next,
        "total_scanned":walk.names.len(),"entries_examined":walk.examined,
        "truncated":walk.limited}),
    )
}
