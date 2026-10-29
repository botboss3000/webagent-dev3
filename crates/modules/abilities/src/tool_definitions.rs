//! Per-tool folders: `<ability>/tools/<tool>/tool.json` plus an optional `tool.skill.md`.
//! A definition holds the model-facing schema, the tool's effect and its default access
//! per built-in role. The executable handler stays compiled Rust and is matched by name;
//! a definition never selects code. An invalid folder disables only that tool.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};
use webagent_access::Permission;

/// Built-in roles a definition may give default access for, highest first.
pub const ROLES: [&str; 3] = ["operator", "member", "visitor"];
const MAX_DEFINITION_BYTES: u64 = 64 * 1024;
const MAX_TOOLS_PER_ABILITY: usize = 64;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    Read,
    Write,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
    pub effect: Effect,
    /// Access when an agent's capability level saves no explicit tool permission.
    /// A role left out gets the next role down; none at all means Deny.
    pub default_access: BTreeMap<String, Permission>,
}

impl ToolDefinition {
    /// Parse and validate one `tool.json`; `folder` is the tool folder's name.
    pub fn parse(bytes: &[u8], folder: &str) -> Result<Self> {
        let definition: Self =
            serde_json::from_slice(bytes).with_context(|| format!("Invalid tool.json: {folder}"))?;
        definition.validate(folder)?;
        Ok(definition)
    }

    fn validate(&self, folder: &str) -> Result<()> {
        if self.name != folder || !crate::catalog::valid_id(&self.name) {
            bail!("Tool name must match its folder: {folder}");
        }
        if self.description.trim().is_empty() || self.description.len() > 8192 {
            bail!("Invalid tool description: {folder}");
        }
        let parameters = &self.parameters;
        let Some(properties) = parameters["properties"].as_object() else {
            bail!("Tool parameters need an object of properties: {folder}");
        };
        let Some(required) = parameters["required"].as_array() else {
            bail!("Tool parameters need a required list: {folder}");
        };
        if parameters["type"] != "object"
            || !parameters["additionalProperties"].is_boolean()
            || properties.len() > 256
            || required.len() > 256
            || required
                .iter()
                .any(|key| key.as_str().is_none_or(|key| !properties.contains_key(key)))
        {
            bail!("Invalid tool parameters: {folder}");
        }
        if self.default_access.keys().any(|role| !ROLES.contains(&role.as_str())) {
            bail!("default_access names an unknown role: {folder}");
        }
        Ok(())
    }

    pub fn is_read(&self) -> bool {
        self.effect == Effect::Read
    }

    /// The function schema the model sees.
    pub fn schema(&self) -> Value {
        json!({"type":"function","name":self.name,"description":self.description,"parameters":self.parameters})
    }

    /// Default access for the first of `rungs` (a level, then the levels below it) this
    /// definition names; Deny when it names none.
    pub fn default_for(&self, rungs: &[String]) -> Permission {
        rungs
            .iter()
            .find_map(|rung| self.default_access.get(rung))
            .copied()
            .unwrap_or(Permission::Deny)
    }
}

/// Where a tool's skill text lives: its folder's `tool.skill.md`, or the legacy flat file.
pub fn skill_path(ability: &Path, name: &str) -> PathBuf {
    let folder = ability.join("tools").join(name);
    if folder.is_dir() {
        folder.join("tool.skill.md")
    } else {
        ability.join("tools").join(format!("{name}.skill.md"))
    }
}

/// One `tools/<tool>/` folder as read by the catalog.
pub struct FolderTool {
    pub name: String,
    pub definition: Result<ToolDefinition>,
    pub skill: Option<String>,
    /// Digest of the folder's bytes, folded into the ability's manifest digest.
    pub digest: String,
}

/// Read an ability's per-tool folders. `None` means the ability still lists its tools in
/// its manifest (the legacy layout, with flat `tools/<tool>.skill.md` files).
pub fn read_folder(ability: &Path) -> Result<Option<Vec<FolderTool>>> {
    let tools = ability.join("tools");
    if !tools.is_dir() {
        return Ok(None);
    }
    let mut folders = Vec::new();
    let mut flat = false;
    for entry in fs::read_dir(&tools)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            folders.push(entry.path());
        } else if kind.is_file() {
            flat = true;
        } else {
            bail!("Invalid tool asset: {}", entry.path().display());
        }
    }
    if folders.is_empty() {
        return Ok(None);
    }
    if flat {
        bail!("Ability mixes tool folders and flat tool files: {}", ability.display());
    }
    if folders.len() > MAX_TOOLS_PER_ABILITY {
        bail!("Too many tools: {}", ability.display());
    }
    folders.sort();
    folders.into_iter().map(|folder| read_tool(&folder)).collect::<Result<_>>().map(Some)
}

/// Sorted tool folder names, or `None` for the legacy layout; reads only the directory.
pub fn folder_names(ability: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(ability.join("tools"))
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    (!names.is_empty()).then_some(names)
}

fn read_tool(folder: &Path) -> Result<FolderTool> {
    let name = folder
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| crate::catalog::valid_id(name))
        .ok_or_else(|| anyhow!("Invalid tool folder: {}", folder.display()))?
        .to_owned();
    let mut hasher = Sha256::new();
    let definition = crate::catalog::read_bytes(&folder.join("tool.json"), MAX_DEFINITION_BYTES)
        .and_then(|bytes| {
            hasher.update(&bytes);
            ToolDefinition::parse(&bytes, &name)
        });
    let skill = crate::catalog::optional_skill(&folder.join("tool.skill.md"))?;
    if let Some(text) = &skill {
        hasher.update(text.as_bytes());
    }
    Ok(FolderTool { name, definition, skill, digest: hex::encode(hasher.finalize()) })
}

/// The manifest digest with every tool folder folded in, so an edited `tool.json` or
/// `tool.skill.md` changes the ability's identity like an edited manifest does.
pub fn folded_digest(manifest_digest: &str, tools: &[FolderTool]) -> String {
    let mut digest = Sha256::new();
    digest.update(manifest_digest.as_bytes());
    for tool in tools {
        digest.update(tool.name.as_bytes());
        digest.update(tool.digest.as_bytes());
    }
    hex::encode(digest.finalize())
}

/// Every valid definition under an abilities root, by tool name, and one message per
/// folder that could not be used.
pub fn load_all(abilities: &Path) -> (BTreeMap<String, ToolDefinition>, Vec<String>) {
    let mut definitions = BTreeMap::new();
    let mut problems = Vec::new();
    let groups = fs::read_dir(abilities).into_iter().flatten().flatten();
    for ability in groups.flat_map(|group| fs::read_dir(group.path()).into_iter().flatten().flatten()) {
        match read_folder(&ability.path()) {
            Ok(tools) => {
                for tool in tools.into_iter().flatten() {
                    match tool.definition {
                        Ok(definition) => {
                            if definitions.insert(tool.name.clone(), definition).is_some() {
                                problems.push(format!("Duplicate tool definition: {}", tool.name));
                            }
                        }
                        Err(error) => problems.push(format!("{error:#}")),
                    }
                }
            }
            Err(error) => problems.push(format!("{error:#}")),
        }
    }
    (definitions, problems)
}

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Read definitions from `abilities` (a candidate being validated); call before first use.
pub fn use_root(abilities: PathBuf) {
    let _ = ROOT.set(abilities);
}

/// The abilities folder definitions are read from, matching the catalog's: a root chosen
/// with `use_root`, else the app's `V3_ABILITIES_ROOT`, else the first live root that has
/// one (the checkout, the published release, then this source tree for tests).
pub fn root() -> Option<PathBuf> {
    if let Some(root) = ROOT.get() {
        return Some(root.clone());
    }
    std::env::var_os("V3_ABILITIES_ROOT")
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            webagent_configuration::live::roots()
                .into_iter()
                .map(|root| root.join("plugins/abilities"))
                .find(|path| path.is_dir())
        })
}

static DEFINITIONS: OnceLock<(BTreeMap<String, ToolDefinition>, Vec<String>)> = OnceLock::new();

/// Process-wide definitions, read once on first use.
pub fn definitions() -> &'static BTreeMap<String, ToolDefinition> {
    &loaded().0
}

/// Folders that were skipped when the process-wide definitions were read.
pub fn problems() -> &'static [String] {
    &loaded().1
}

fn loaded() -> &'static (BTreeMap<String, ToolDefinition>, Vec<String>) {
    DEFINITIONS.get_or_init(|| match root() {
        Some(root) => load_all(&root),
        None => (BTreeMap::new(), vec!["No plugins/abilities folder found".into()]),
    })
}

pub fn get(name: &str) -> Option<&'static ToolDefinition> {
    definitions().get(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(folder: &Path, name: &str, body: &Value) {
        let path = folder.join("tools").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("tool.json"), serde_json::to_vec(body).unwrap()).unwrap();
    }
    fn body(name: &str) -> Value {
        json!({"name":name,"description":"Look something up","effect":"read",
            "parameters":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"],"additionalProperties":false},
            "default_access":{"operator":"auto","member":"ask"}})
    }

    #[test]
    fn folders_define_tools_and_bad_folders_fail_alone() {
        let root = tempfile::tempdir().unwrap();
        let ability = root.path().join("Web/lookup");
        write(&ability, "lookup", &body("lookup"));
        write(&ability, "renamed", &body("other_name"));
        let mut extra = body("extra");
        extra["effect"] = json!("delete");
        write(&ability, "extra", &extra);
        fs::write(ability.join("tools/lookup/tool.skill.md"), "Use it well.").unwrap();
        let tools = read_folder(&ability).unwrap().unwrap();
        assert_eq!(tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["extra", "lookup", "renamed"]);
        assert!(tools[0].definition.is_err() && tools[2].definition.is_err());
        let lookup = tools[1].definition.as_ref().unwrap();
        assert_eq!(tools[1].skill.as_deref(), Some("Use it well."));
        assert!(lookup.is_read());
        assert_eq!(lookup.schema()["parameters"]["required"], json!(["q"]));
        let (all, problems) = load_all(root.path());
        assert_eq!(all.keys().collect::<Vec<_>>(), ["lookup"]);
        assert_eq!(problems.len(), 2);
    }

    #[test]
    fn default_access_falls_to_the_next_role_down_then_deny() {
        let definition = ToolDefinition::parse(&serde_json::to_vec(&body("lookup")).unwrap(), "lookup").unwrap();
        let rungs = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(definition.default_for(&rungs(&["operator", "member", "visitor"])), Permission::Auto);
        assert_eq!(definition.default_for(&rungs(&["trusted", "member", "visitor"])), Permission::Ask);
        assert_eq!(definition.default_for(&rungs(&["visitor"])), Permission::Deny);
    }

    #[test]
    fn legacy_and_mixed_layouts() {
        let root = tempfile::tempdir().unwrap();
        let legacy = root.path().join("Web/legacy");
        fs::create_dir_all(legacy.join("tools")).unwrap();
        fs::write(legacy.join("tools/lookup.skill.md"), "Flat skill").unwrap();
        assert!(read_folder(&legacy).unwrap().is_none());
        write(&legacy, "lookup", &body("lookup"));
        assert!(read_folder(&legacy).is_err());
        let mut unknown = body("lookup");
        unknown["default_access"]["admin"] = json!("auto");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&unknown).unwrap(), "lookup").is_err());
    }
}
