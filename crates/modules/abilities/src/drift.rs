//! Compares the live checkout's ability folders with the published release the app runs.
//! Definition-only edits (a tool's description or role defaults, changed guidance text)
//! are adopted live. Everything else becomes a notice for administrators: removed tools
//! and tools whose new definition the running code cannot honour are turned off at once,
//! added abilities and tools stay inactive (the running catalog never lists them), and
//! changed code keeps the last build running until a rebuild.
use crate::tool_definitions::ToolDefinition;
use anyhow::{bail, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_FILES: usize = 4096;
const MAX_FILE_BYTES: u64 = 16 * 1024 * 1024;

/// What changed in the checkout relative to the running release.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    AddedAbility,
    RemovedAbility,
    AddedTool,
    RemovedTool,
    InvalidTool,
    ChangedSchema,
    ChangedManifest,
    ChangedGuidanceFiles,
    ChangedCode,
}

/// One change that needs a rebuild, shown to administrators.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Notice {
    pub change: Change,
    /// `Group/ability_id`.
    pub ability: String,
    pub tool: Option<String>,
    pub detail: String,
}

/// The checkout's drift from the running release.
#[derive(Debug, Default)]
pub struct Drift {
    /// Sorted; their digest identifies the change set.
    pub notices: Vec<Notice>,
    /// Tools turned off, with the reason.
    pub disabled: BTreeMap<String, String>,
    /// Live definitions for tools whose description or role defaults changed compatibly.
    pub definitions: BTreeMap<String, ToolDefinition>,
    /// Checkout files that replace changed guidance, by (ability id, kind, name).
    pub guidance: BTreeMap<(String, String, String), PathBuf>,
}

impl Drift {
    /// Identity of the change set the notices describe; empty when nothing needs a rebuild.
    pub fn digest(&self) -> String {
        if self.notices.is_empty() {
            return String::new();
        }
        let bytes = serde_json::to_vec(&self.notices).unwrap_or_default();
        hex::encode(Sha256::digest(bytes))
    }

    pub fn disabled(&self, tool: &str) -> bool {
        self.disabled.contains_key(tool)
    }

    pub fn definition(&self, tool: &str) -> Option<&ToolDefinition> {
        self.definitions.get(tool)
    }

    /// `kind` is `ability`, `tool` or `selectable`, as in the catalog's guidance reads.
    pub fn guidance(&self, ability: &str, kind: &str, name: &str) -> Option<&Path> {
        self.guidance
            .get(&(ability.to_owned(), kind.to_owned(), name.to_owned()))
            .map(PathBuf::as_path)
    }
}

/// Compare the published catalog root with the checkout's (`plugins/abilities` in both).
pub fn compare(published: &Path, checkout: &Path) -> Result<Drift> {
    let old = abilities(published)?;
    let new = abilities(checkout)?;
    let mut drift = Drift::default();
    for key in new.keys().filter(|key| !old.contains_key(*key)) {
        notice(
            &mut drift,
            Change::AddedAbility,
            key,
            None,
            "New ability; inactive until rebuilt.".into(),
        );
    }
    for (key, path) in &old {
        let Some(current) = new.get(key) else {
            notice(
                &mut drift,
                Change::RemovedAbility,
                key,
                None,
                "Removed from the checkout; its tools are off. Rebuild to drop its code.".into(),
            );
            for tool in declared_tools(key, &files(path)?) {
                drift
                    .disabled
                    .insert(tool, "Its ability was removed from the checkout".into());
            }
            continue;
        };
        ability(&mut drift, key, &files(path)?, &files(current)?, current);
    }
    drift.notices.sort();
    Ok(drift)
}

fn ability(
    drift: &mut Drift,
    key: &str,
    old: &BTreeMap<String, Vec<u8>>,
    new: &BTreeMap<String, Vec<u8>>,
    root: &Path,
) {
    let id = key.rsplit('/').next().unwrap_or(key);
    let (old_tools, new_tools) = (tool_folders(old), tool_folders(new));
    for tool in old_tools.difference(&new_tools) {
        notice(
            drift,
            Change::RemovedTool,
            key,
            Some(tool.as_str()),
            "Removed from the checkout; turned off. Rebuild to drop its code.".into(),
        );
        drift
            .disabled
            .insert(tool.clone(), "Removed from the checkout".into());
    }
    for tool in new_tools.difference(&old_tools) {
        notice(
            drift,
            Change::AddedTool,
            key,
            Some(tool.as_str()),
            "New tool; inactive until rebuilt.".into(),
        );
    }
    for tool in old_tools.intersection(&new_tools) {
        definition(drift, key, tool, old, new);
    }
    let mut code = Vec::new();
    let paths: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
    for path in paths {
        if old.get(path) == new.get(path)
            || in_tool_folder(path).is_some_and(|tool| {
                old_tools.contains(tool) && new_tools.contains(tool) && path.ends_with("/tool.json")
            })
        {
            continue;
        }
        if in_tool_folder(path)
            .is_some_and(|tool| !old_tools.contains(tool) || !new_tools.contains(tool))
        {
            continue;
        }
        if *path == format!("{id}.json") {
            notice(
                drift,
                Change::ChangedManifest,
                key,
                None,
                "Ability manifest changed; the last build keeps running until rebuilt.".into(),
            );
        } else if let Some((kind, name)) = guidance_file(id, path) {
            if old.contains_key(path) && new.contains_key(path) {
                drift
                    .guidance
                    .insert((id.to_owned(), kind.into(), name), root.join(path));
            } else {
                notice(
                    drift,
                    Change::ChangedGuidanceFiles,
                    key,
                    None,
                    format!(
                        "Guidance file {path} was added or removed; takes effect after a rebuild."
                    ),
                );
            }
        } else {
            code.push(path.as_str());
        }
    }
    if !code.is_empty() {
        let shown = code.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        let more = if code.len() > 3 {
            format!(" and {} more", code.len() - 3)
        } else {
            String::new()
        };
        notice(
            drift,
            Change::ChangedCode,
            key,
            None,
            format!("Code changed ({shown}{more}); the last build keeps running until rebuilt."),
        );
    }
}

/// A changed `tool.json`: adopt it when only the description or role defaults changed,
/// otherwise turn the tool off.
fn definition(
    drift: &mut Drift,
    key: &str,
    tool: &str,
    old: &BTreeMap<String, Vec<u8>>,
    new: &BTreeMap<String, Vec<u8>>,
) {
    let path = format!("tools/{tool}/tool.json");
    if old.get(&path) == new.get(&path) {
        return;
    }
    let parsed = new
        .get(&path)
        .ok_or_else(|| anyhow::anyhow!("tool.json is missing"))
        .and_then(|bytes| ToolDefinition::parse(bytes, tool));
    let current = match parsed {
        Ok(current) => current,
        Err(error) => {
            notice(
                drift,
                Change::InvalidTool,
                key,
                Some(tool),
                format!("Invalid tool.json, turned off: {error:#}"),
            );
            drift
                .disabled
                .insert(tool.to_owned(), format!("Invalid tool.json: {error:#}"));
            return;
        }
    };
    let Some(running) = old
        .get(&path)
        .and_then(|bytes| ToolDefinition::parse(bytes, tool).ok())
    else {
        return;
    };
    if running.name == current.name
        && running.parameters == current.parameters
        && running.effect == current.effect
        && running.strict == current.strict
    {
        drift.definitions.insert(tool.to_owned(), current);
    } else {
        notice(
            drift,
            Change::ChangedSchema,
            key,
            Some(tool),
            "Parameters or effect changed; turned off until rebuilt.".into(),
        );
        drift.disabled.insert(
            tool.to_owned(),
            "Its definition changed; rebuild needed".into(),
        );
    }
}

fn notice(drift: &mut Drift, change: Change, ability: &str, tool: Option<&str>, detail: String) {
    drift.notices.push(Notice {
        change,
        ability: ability.to_owned(),
        tool: tool.map(str::to_owned),
        detail,
    });
}

/// `Group/ability_id` folders that hold at least one file.
fn abilities(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut found = BTreeMap::new();
    for group in directories(root)? {
        for ability in directories(&group)? {
            if has_file(&ability)? {
                let name = |path: &Path| {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                };
                found.insert(format!("{}/{}", name(&group), name(&ability)), ability);
            }
        }
    }
    Ok(found)
}

fn directories(path: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !ignored(&entry.file_name().to_string_lossy()) {
            found.push(entry.path());
        }
    }
    Ok(found)
}

fn has_file(path: &Path) -> Result<bool> {
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if ignored(&entry.file_name().to_string_lossy()) {
            continue;
        }
        if kind.is_file() || (kind.is_dir() && has_file(&entry.path())?) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Every regular file under `root` by relative path. Symlinks are never followed, so a
/// symlinked definition reads as missing and its tool turns off.
fn files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(dir) = pending.pop() {
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if ignored(&entry.file_name().to_string_lossy()) {
                continue;
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file() {
                if entry.metadata()?.len() > MAX_FILE_BYTES || found.len() >= MAX_FILES {
                    bail!("Ability folder too large: {}", root.display());
                }
                let relative = entry
                    .path()
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .into_owned();
                found.insert(relative, fs::read(entry.path())?);
            }
        }
    }
    Ok(found)
}

/// Build and editor leftovers that are never part of a release.
fn ignored(name: &str) -> bool {
    name == "__pycache__" || name.ends_with(".pyc") || name.starts_with('.')
}

/// The tool a `tools/<tool>/...` path belongs to.
fn in_tool_folder(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("tools/")?;
    let (tool, _) = rest.split_once('/')?;
    Some(tool)
}

fn tool_folders(files: &BTreeMap<String, Vec<u8>>) -> BTreeSet<String> {
    files
        .keys()
        .filter_map(|path| in_tool_folder(path))
        .map(str::to_owned)
        .collect()
}

/// Tools an ability declares: its tool folders, or its manifest's `tools` list.
fn declared_tools(key: &str, files: &BTreeMap<String, Vec<u8>>) -> BTreeSet<String> {
    let id = key.rsplit('/').next().unwrap_or(key);
    let mut tools = tool_folders(files);
    if let Some(manifest) = files
        .get(&format!("{id}.json"))
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
    {
        tools.extend(
            manifest["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool.as_str().map(str::to_owned)),
        );
    }
    tools
}

/// The guidance a path holds, as (kind, name).
fn guidance_file(id: &str, path: &str) -> Option<(&'static str, String)> {
    if path == format!("{id}.skill.md") {
        return Some(("ability", id.to_owned()));
    }
    if let Some(tool) = in_tool_folder(path) {
        return (path == format!("tools/{tool}/tool.skill.md")).then(|| ("tool", tool.to_owned()));
    }
    let name = |prefix: &str| {
        path.strip_prefix(prefix)
            .and_then(|rest| rest.strip_suffix(".skill.md"))
            .filter(|name| !name.contains('/'))
            .map(str::to_owned)
    };
    name("skills/")
        .map(|name| ("selectable", name))
        .or_else(|| name("tools/").map(|name| ("tool", name)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    fn tool(name: &str, description: &str, effect: &str, operator: &str) -> String {
        json!({"name":name,"description":description,"effect":effect,
            "default_access":{"operator":operator},
            "parameters":{"type":"object","properties":{"q":{"type":"string"}},"required":[],"additionalProperties":false}})
            .to_string()
    }

    /// Two identical trees: Core/files with tools read_file and write_file, and Core/notes.
    fn trees() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let (old, new) = (temp.path().join("old"), temp.path().join("new"));
        for root in [&old, &new] {
            write(root, "Core/files/files.json", r#"{"name":"Files"}"#);
            write(root, "Core/files/files.skill.md", "Files guidance");
            write(root, "Core/files/adapter.rs", "fn main() {}");
            write(
                root,
                "Core/files/tools/read_file/tool.json",
                &tool("read_file", "Read", "read", "auto"),
            );
            write(
                root,
                "Core/files/tools/read_file/tool.skill.md",
                "Read guidance",
            );
            write(
                root,
                "Core/files/tools/write_file/tool.json",
                &tool("write_file", "Write", "write", "auto"),
            );
            write(
                root,
                "Core/notes/notes.json",
                r#"{"name":"Notes","tools":["note_add"]}"#,
            );
        }
        (temp, old, new)
    }

    #[test]
    fn identical_trees_have_no_drift() {
        let (_temp, old, new) = trees();
        write(&new, "Core/files/__pycache__/x.pyc", "cache");
        fs::create_dir_all(new.join("Core/empty/tools")).unwrap();
        let drift = compare(&old, &new).unwrap();
        assert!(
            drift.notices.is_empty() && drift.disabled.is_empty() && drift.definitions.is_empty()
        );
        assert_eq!(drift.digest(), "");
    }

    #[test]
    fn definition_only_edits_are_live_without_a_notice() {
        let (_temp, old, new) = trees();
        write(
            &new,
            "Core/files/tools/read_file/tool.json",
            &tool("read_file", "Read a file", "read", "ask"),
        );
        write(
            &new,
            "Core/files/tools/read_file/tool.skill.md",
            "New read guidance",
        );
        write(&new, "Core/files/files.skill.md", "New files guidance");
        let drift = compare(&old, &new).unwrap();
        assert!(drift.notices.is_empty(), "{:?}", drift.notices);
        assert_eq!(
            drift.definition("read_file").unwrap().description,
            "Read a file"
        );
        assert_eq!(
            drift.guidance("files", "tool", "read_file").unwrap(),
            new.join("Core/files/tools/read_file/tool.skill.md")
        );
        assert_eq!(
            drift.guidance("files", "ability", "files").unwrap(),
            new.join("Core/files/files.skill.md")
        );
        assert!(!drift.disabled("read_file"));
    }

    #[test]
    fn incompatible_invalid_and_removed_tools_turn_off_with_notices() {
        let (_temp, old, new) = trees();
        write(
            &new,
            "Core/files/tools/read_file/tool.json",
            &tool("read_file", "Read", "write", "auto"),
        );
        write(&new, "Core/files/tools/write_file/tool.json", "{");
        fs::remove_dir_all(new.join("Core/notes")).unwrap();
        let drift = compare(&old, &new).unwrap();
        assert!(
            drift.disabled("read_file")
                && drift.disabled("write_file")
                && drift.disabled("note_add")
        );
        let changes: Vec<_> = drift
            .notices
            .iter()
            .map(|n| (n.change, n.tool.clone()))
            .collect();
        assert_eq!(
            changes,
            vec![
                (Change::RemovedAbility, None),
                (Change::InvalidTool, Some("write_file".into())),
                (Change::ChangedSchema, Some("read_file".into())),
            ]
        );
        assert!(drift.definitions.is_empty());
        assert_ne!(drift.digest(), "");
    }

    #[test]
    fn additions_and_code_changes_only_notify() {
        let (_temp, old, new) = trees();
        write(
            &new,
            "Core/files/tools/move_file/tool.json",
            &tool("move_file", "Move", "write", "auto"),
        );
        write(&new, "Core/files/adapter.rs", "fn main() { changed() }");
        write(&new, "Core/files/files.json", r#"{"name":"Files v2"}"#);
        write(&new, "Core/files/skills/extra.skill.md", "Extra");
        write(&new, "Web/browse/browse.json", r#"{"name":"Browse"}"#);
        let drift = compare(&old, &new).unwrap();
        assert!(drift.disabled.is_empty());
        let changes: Vec<_> = drift.notices.iter().map(|n| n.change).collect();
        assert_eq!(
            changes,
            vec![
                Change::AddedAbility,
                Change::AddedTool,
                Change::ChangedManifest,
                Change::ChangedGuidanceFiles,
                Change::ChangedCode
            ]
        );
        let before = drift.digest();
        write(
            &new,
            "Core/files/adapter.rs",
            "fn main() { changed_again() }",
        );
        assert_eq!(
            compare(&old, &new).unwrap().digest(),
            before,
            "same change set, same digest"
        );
        write(&new, "Core/files/other.rs", "");
        assert_ne!(
            compare(&old, &new).unwrap().digest(),
            before,
            "a new change raises a new popup"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_definition_reads_as_missing() {
        let (_temp, old, new) = trees();
        let path = new.join("Core/files/tools/read_file/tool.json");
        fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(old.join("Core/files/tools/read_file/tool.json"), &path)
            .unwrap();
        let drift = compare(&old, &new).unwrap();
        assert!(drift.disabled("read_file"));
        assert_eq!(drift.notices[0].change, Change::InvalidTool);
    }
}
