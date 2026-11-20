//! Per-tool folders: `<ability>/tools/<tool>/<tool>.tool.json` plus an optional `<tool>.skill.md`.
//! A definition holds the model-facing schema, the tool's effect and its default access
//! per built-in role. The executable handler stays compiled Rust and is matched by name;
//! a definition never selects code. An invalid folder disables only that tool.
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
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

/// How a repeat of an identical call in one run is treated; writes default to `High`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum RetryRisk {
    None,
    Low,
    High,
}

/// Repeat-call safety for one tool: how risky a repeat is, what the agent should verify
/// before repeating, and what the user is told when asked to approve a repeat.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RepeatGuard {
    /// Absent means high for a write. Reads are never repeat-guarded.
    #[serde(default)]
    pub risk: Option<RetryRisk>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub warning: Option<String>,
}

/// How long two calls with the same key count as one action.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Window {
    /// Only when no other recorded action of the agent came in between.
    Consecutive,
    /// For the rest of the session.
    Session,
    /// Until someone resolves or clears it.
    Forever,
    /// Within this many seconds of the earlier call.
    Seconds(u64),
}

/// The action ledger's repeat protection: the argument fields that make two calls the same
/// action, and how long they stay the same.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DuplicateProtection {
    pub key: Vec<String>,
    pub window: Window,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    /// Kept only when a schema opts out of strict argument validation.
    #[serde(default)]
    pub strict: Option<bool>,
    pub parameters: Value,
    /// Whether the tool changes anything. Drives recovery (an interrupted write is
    /// uncertain, never rerun), batches, background work and the repeat guard; never access.
    pub effect: Effect,
    /// Access when an agent's capability level saves no explicit tool permission. A role
    /// left out gets the next role down; with none named, a read allows and a write
    /// requires approval.
    #[serde(default)]
    pub default_policy: BTreeMap<String, Permission>,
    /// Tiers whose owners' agents may have this tool; absent inherits its ability's
    /// `owner_tier`, and a list may only narrow it.
    #[serde(default)]
    pub owner_tier: Option<Vec<String>>,
    /// Built-in roles that may ever have this tool; absent inherits its ability's
    /// `role_access` (every role when that has none). No level setting widens it.
    #[serde(default)]
    pub role_access: Option<Vec<String>>,
    /// Offered in every session without loading, and exempt from ability and tier gating;
    /// the tool's own policy still applies. Absent or null means false.
    #[serde(default)]
    pub core: Option<bool>,
    #[serde(default)]
    pub repeat_guard: RepeatGuard,
    /// One line added to argument errors to guide the agent's next call.
    #[serde(default)]
    pub usage_hint: Option<String>,
    /// Example requests and exact keywords that list_skills ranking uses for this tool.
    #[serde(default, rename = "match")]
    pub match_data: Option<ToolMatch>,
    /// What stored calls keep once the conversation has moved past them; absent keeps all.
    #[serde(default)]
    pub retention: crate::tool_retention::Retention,
    /// Ledger repeat protection, on unless the agent owner switches it off.
    #[serde(default)]
    pub duplicate_protection: Option<DuplicateProtection>,
    /// The ledger records every call and reports uncertain outcomes, whatever the
    /// repeat protection setting says.
    #[serde(default)]
    pub critical: bool,
}

/// Discovery hints: short example requests (embedded) and exact words (keyword evidence).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ToolMatch {
    #[serde(default)]
    pub examples: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

impl ToolDefinition {
    /// Parse and validate one `<tool>.tool.json`; `folder` is the tool folder's name.
    pub fn parse(bytes: &[u8], folder: &str) -> Result<Self> {
        let definition: Self =
            serde_json::from_slice(bytes).with_context(|| format!("Invalid {folder}.tool.json"))?;
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
        if self
            .default_policy
            .keys()
            .any(|role| !ROLES.contains(&role.as_str()))
        {
            bail!("default_policy names an unknown role: {folder}");
        }
        if self
            .owner_tier
            .iter()
            .flatten()
            .any(|tier| !webagent_access::valid_id(tier))
        {
            bail!("owner_tier names an invalid tier: {folder}");
        }
        if self.role_access.as_ref().is_some_and(|roles| {
            roles.is_empty() || roles.iter().any(|role| !ROLES.contains(&role.as_str()))
        }) {
            bail!("role_access must list known roles: {folder}");
        }
        if self.effect == Effect::Read
            && self
                .repeat_guard
                .risk
                .is_some_and(|risk| risk != RetryRisk::None)
        {
            bail!("A read tool cannot set a repeat risk: {folder}");
        }
        if [
            &self.repeat_guard.hint,
            &self.repeat_guard.warning,
            &self.usage_hint,
        ]
        .into_iter()
        .flatten()
        .any(|text| text.trim().is_empty() || text.len() > 1000)
        {
            bail!("Invalid repeat_guard hint/warning or usage_hint: {folder}");
        }
        self.retention.validate(&self.parameters, folder)?;
        if (self.duplicate_protection.is_some() || self.critical) && self.is_read() {
            bail!("A read tool cannot use the action ledger: {folder}");
        }
        if let Some(protection) = &self.duplicate_protection {
            let properties = self.parameters["properties"].as_object();
            if protection.key.is_empty()
                || protection.key.len() > 16
                || protection
                    .key
                    .iter()
                    .any(|field| !properties.is_some_and(|known| known.contains_key(field)))
                || matches!(protection.window, Window::Seconds(0))
            {
                bail!("duplicate_protection key must list tool parameters and seconds be positive: {folder}");
            }
        }
        if let Some(hints) = &self.match_data {
            if hints.examples.len() > 8
                || hints.keywords.len() > 12
                || hints
                    .examples
                    .iter()
                    .any(|e| e.trim().is_empty() || e.len() > 200)
                || hints
                    .keywords
                    .iter()
                    .any(|k| k.trim().is_empty() || k.len() > 40)
            {
                bail!("Invalid match examples or keywords: {folder}");
            }
        }
        Ok(())
    }

    pub fn is_read(&self) -> bool {
        self.effect == Effect::Read
    }

    /// The function schema the model sees.
    pub fn schema(&self) -> Value {
        let mut schema = json!({"type":"function","name":self.name,
            "description":self.description,"parameters":self.parameters});
        if let Some(strict) = self.strict {
            schema["strict"] = json!(strict);
        }
        schema
    }

    /// Default policy for the first of `rungs` (a level, then the levels below it) this
    /// definition names; with none named, a read allows and a write requires approval.
    pub fn default_for(&self, rungs: &[String]) -> Permission {
        rungs
            .iter()
            .find_map(|rung| self.default_policy.get(rung))
            .copied()
            .unwrap_or(match self.effect {
                Effect::Read => Permission::Auto,
                Effect::Write => Permission::Ask,
            })
    }
}

/// `tools/shared/` holds Rust code the ability's tools share; it is never a tool folder.
pub const SHARED_FOLDER: &str = "shared";

/// A tool folder's definition file name: `<tool>.tool.json`.
pub fn definition_file(name: &str) -> String {
    format!("{name}.tool.json")
}

/// A tool folder's skill file name: `<tool>.skill.md`.
pub fn skill_file(name: &str) -> String {
    format!("{name}.skill.md")
}

/// Where a tool's skill text lives: its folder's `<tool>.skill.md`, or the legacy flat file.
pub fn skill_path(ability: &Path, name: &str) -> PathBuf {
    let folder = ability.join("tools").join(name);
    if folder.is_dir() {
        folder.join(skill_file(name))
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
            if entry.file_name() != SHARED_FOLDER {
                folders.push(entry.path());
            }
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
        bail!(
            "Ability mixes tool folders and flat tool files: {}",
            ability.display()
        );
    }
    if folders.len() > MAX_TOOLS_PER_ABILITY {
        bail!("Too many tools: {}", ability.display());
    }
    folders.sort();
    folders
        .into_iter()
        .map(|folder| read_tool(&folder))
        .collect::<Result<_>>()
        .map(Some)
}

/// Sorted tool folder names, or `None` for the legacy layout; reads only the directory.
pub fn folder_names(ability: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(ability.join("tools"))
        .ok()?
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter(|entry| entry.file_name() != SHARED_FOLDER)
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
    let definition =
        crate::catalog::read_bytes(&folder.join(definition_file(&name)), MAX_DEFINITION_BYTES)
            .and_then(|bytes| {
                hasher.update(&bytes);
                ToolDefinition::parse(&bytes, &name)
            });
    let skill = crate::catalog::optional_skill(&folder.join(skill_file(&name)))?;
    if let Some(text) = &skill {
        hasher.update(text.as_bytes());
    }
    Ok(FolderTool {
        name,
        definition,
        skill,
        digest: hex::encode(hasher.finalize()),
    })
}

/// The manifest digest with every tool folder folded in, so an edited `<tool>.tool.json` or
/// `<tool>.skill.md` changes the ability's identity like an edited manifest does.
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
    for ability in
        groups.flat_map(|group| fs::read_dir(group.path()).into_iter().flatten().flatten())
    {
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

/// Tools served by isolated plugin packages whose code differs between two abilities roots:
/// anything outside a package's `tools/` and `skills/` folders changed, or the package is on
/// one side only. Such a release ships the code with its schemas, so they may change together.
pub fn package_tools_with_code_changes(baseline: &Path, candidate: &Path) -> BTreeSet<String> {
    let (old, new) = (packages(baseline), packages(candidate));
    let mut tools = BTreeSet::new();
    for folder in old.keys().chain(new.keys()) {
        let (before, after) = (old.get(folder), new.get(folder));
        if before.map(|package| &package.1) != after.map(|package| &package.1) {
            for package in [before, after].into_iter().flatten() {
                tools.extend(package.0.iter().cloned());
            }
        }
    }
    tools
}

/// Isolated packages by `<group>/<ability>`: their declared tools and a digest of their code.
fn packages(abilities: &Path) -> BTreeMap<String, (Vec<String>, String)> {
    let mut packages = BTreeMap::new();
    let groups = fs::read_dir(abilities).into_iter().flatten().flatten();
    for ability in
        groups.flat_map(|group| fs::read_dir(group.path()).into_iter().flatten().flatten())
    {
        let path = ability.path();
        let Ok(manifest) = fs::read(path.join("plugin.json")) else {
            continue;
        };
        let manifest: Value = serde_json::from_slice(&manifest).unwrap_or(Value::Null);
        if manifest["dispatch"] != "isolated" {
            continue;
        }
        let tools = manifest["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        let mut digest = Sha256::new();
        code_digest(&path, &path, &mut digest);
        let folder = path.strip_prefix(abilities).unwrap_or(&path);
        packages.insert(
            folder.to_string_lossy().into_owned(),
            (tools, hex::encode(digest.finalize())),
        );
    }
    packages
}

/// Hashes every file under `dir` but the package's own `tools/` and `skills/` folders, in
/// name order, with its relative path; symlinks are skipped.
fn code_digest(package: &Path, dir: &Path, digest: &mut Sha256) {
    let mut entries: Vec<_> = fs::read_dir(dir).into_iter().flatten().flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if dir == package && matches!(entry.file_name().to_str(), Some("tools" | "skills")) {
                continue;
            }
            code_digest(package, &path, digest);
        } else if kind.is_file() {
            let relative = path.strip_prefix(package).unwrap_or(&path);
            digest.update(relative.to_string_lossy().as_bytes());
            digest.update([0]);
            digest.update(fs::read(&path).unwrap_or_default());
            digest.update([0]);
        }
    }
}

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Read definitions from `abilities` (a candidate being validated); call before first use.
pub fn use_root(abilities: PathBuf) {
    let _ = ROOT.set(abilities);
}

/// The abilities folder definitions are read from, matching the catalog's: a root chosen
/// with `use_root`, else the live snapshot the catalog chose (so checkout edits of tool.json
/// reach the argument check, effects and guards), else the selected publication (so an
/// ability publish takes effect without a restart), else the app's `V3_ABILITIES_ROOT`, else
/// the first live root that has one (the checkout, the published release, then this source
/// tree for tests).
pub fn root() -> Option<PathBuf> {
    if let Some(root) = ROOT.get() {
        return Some(root.clone());
    }
    // This crate's own tests point the live snapshot at temporary folders.
    #[cfg(not(test))]
    if let Some(live) = crate::live::selected()
        .map(|root| root.join("abilities"))
        .filter(|path| path.is_dir())
    {
        return Some(live);
    }
    let published = webagent_configuration::prompts::default_source(None)
        .ok()
        .flatten()
        .map(|source| source.join("abilities"))
        .filter(|path| path.is_dir());
    published.or_else(|| {
        std::env::var_os("V3_ABILITIES_ROOT")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .or_else(|| {
                webagent_configuration::live::roots()
                    .into_iter()
                    .map(|root| root.join("abilities"))
                    .find(|path| path.is_dir())
            })
    })
}

type Loaded = (BTreeMap<String, ToolDefinition>, Vec<String>);

/// The definitions read from the current root. Publications are immutable, so each root is
/// read once; a switch to a new publication reads it and keeps the old one alive, since
/// callers hold `'static` references. Publishes are rare, so this grows by one small map each.
static DEFINITIONS: Mutex<Option<(Option<PathBuf>, &'static Loaded)>> = Mutex::new(None);

/// Process-wide definitions from the current root.
pub fn definitions() -> &'static BTreeMap<String, ToolDefinition> {
    &loaded().0
}

/// Folders that were skipped when the process-wide definitions were read.
pub fn problems() -> &'static [String] {
    &loaded().1
}

fn loaded() -> &'static Loaded {
    let root = root();
    let mut held = DEFINITIONS.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((read_from, loaded)) = held.as_ref() {
        if *read_from == root {
            return loaded;
        }
    }
    let loaded: &'static Loaded = Box::leak(Box::new(match &root {
        Some(root) => load_all(root),
        None => (BTreeMap::new(), vec!["No abilities folder found".into()]),
    }));
    *held = Some((root, loaded));
    loaded
}

pub fn get(name: &str) -> Option<&'static ToolDefinition> {
    definitions().get(name)
}

/// Whether `name`'s `<tool>.tool.json` sets `core: true`.
pub fn is_core(name: &str) -> bool {
    get(name).is_some_and(|definition| definition.core == Some(true))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(folder: &Path, name: &str, body: &Value) {
        let path = folder.join("tools").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join(definition_file(name)),
            serde_json::to_vec(body).unwrap(),
        )
        .unwrap();
    }
    fn body(name: &str) -> Value {
        json!({"name":name,"description":"Look something up","effect":"read",
            "parameters":{"type":"object","properties":{"q":{"type":"string"}},"required":["q"],"additionalProperties":false},
            "default_policy":{"operator":"allow","member":"require_approval"}})
    }

    #[test]
    fn duplicate_protection_keys_name_parameters_of_a_write() {
        let parse = |extra: Value| {
            let mut tool = body("push");
            tool["effect"] = json!("write");
            tool.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            ToolDefinition::parse(&serde_json::to_vec(&tool).unwrap(), "push")
        };
        let ok = parse(json!({"critical":true,
            "duplicate_protection":{"key":["q"],"window":{"seconds":60}}}))
        .unwrap();
        assert!(ok.critical);
        assert_eq!(ok.duplicate_protection.unwrap().window, Window::Seconds(60));
        for window in ["consecutive", "session", "forever"] {
            assert!(parse(json!({"duplicate_protection":{"key":["q"],"window":window}})).is_ok());
        }
        for bad in [
            json!({"duplicate_protection":{"key":["missing"],"window":"forever"}}),
            json!({"duplicate_protection":{"key":[],"window":"forever"}}),
            json!({"duplicate_protection":{"key":["q"],"window":{"seconds":0}}}),
            json!({"duplicate_protection":{"key":["q"],"window":"daily"}}),
        ] {
            assert!(parse(bad).is_err());
        }
        let read = ToolDefinition::parse(
            &serde_json::to_vec(
                &json!({"critical":true})
                    .as_object()
                    .unwrap()
                    .clone()
                    .into_iter()
                    .chain(body("push").as_object().unwrap().clone())
                    .collect::<serde_json::Map<_, _>>(),
            )
            .unwrap(),
            "push",
        );
        assert!(read.is_err());
    }

    #[test]
    fn package_code_changes_exempt_only_that_packages_tools() {
        let write = |root: &Path, file: &str, text: &str| {
            let path = root.join("G/pkg").join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        let (baseline, candidate) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        for root in [baseline.path(), candidate.path()] {
            write(
                root,
                "plugin.json",
                r#"{"dispatch":"isolated","tools":["x"]}"#,
            );
            write(root, "src/main.rs", "fn main() {}");
            write(root, "tools/x/x.tool.json", "{}");
        }
        write(
            candidate.path(),
            "tools/x/x.tool.json",
            r#"{"edited":true}"#,
        );
        write(candidate.path(), "skills/pkg.skill.md", "new");
        assert!(package_tools_with_code_changes(baseline.path(), candidate.path()).is_empty());
        write(candidate.path(), "src/main.rs", "fn main() { run() }");
        assert_eq!(
            package_tools_with_code_changes(baseline.path(), candidate.path()),
            BTreeSet::from(["x".to_owned()])
        );
    }

    #[test]
    fn shared_code_and_rust_files_are_not_tools() {
        let root = tempfile::tempdir().unwrap();
        let ability = root.path().join("Web/lookup");
        write(&ability, "lookup", &body("lookup"));
        fs::write(ability.join("tools/lookup/lookup.rs"), "pub fn run() {}").unwrap();
        fs::create_dir_all(ability.join("tools/shared")).unwrap();
        fs::write(ability.join("tools/shared/store.rs"), "pub fn open() {}").unwrap();
        let tools = read_folder(&ability).unwrap().unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["lookup"]
        );
        assert!(tools[0].definition.is_ok());
        assert_eq!(folder_names(&ability).unwrap(), ["lookup"]);
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
        fs::write(ability.join("tools/lookup/lookup.skill.md"), "Use it well.").unwrap();
        let tools = read_folder(&ability).unwrap().unwrap();
        assert_eq!(
            tools.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(),
            ["extra", "lookup", "renamed"]
        );
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
    fn default_policy_falls_to_the_next_role_down_then_the_effect() {
        let definition =
            ToolDefinition::parse(&serde_json::to_vec(&body("lookup")).unwrap(), "lookup").unwrap();
        let rungs = |ids: &[&str]| ids.iter().map(|id| id.to_string()).collect::<Vec<_>>();
        assert_eq!(
            definition.default_for(&rungs(&["operator", "member", "visitor"])),
            Permission::Auto
        );
        assert_eq!(
            definition.default_for(&rungs(&["trusted", "member", "visitor"])),
            Permission::Ask
        );
        assert_eq!(
            definition.default_for(&rungs(&["visitor"])),
            Permission::Auto
        );
        let mut write = body("lookup");
        write["effect"] = json!("write");
        write.as_object_mut().unwrap().remove("default_policy");
        let write = ToolDefinition::parse(&serde_json::to_vec(&write).unwrap(), "lookup").unwrap();
        assert_eq!(write.default_for(&rungs(&["operator"])), Permission::Ask);
    }

    #[test]
    fn core_defaults_to_false_when_absent_or_null() {
        let parse = |value: Option<Value>| {
            let mut tool = body("lookup");
            if let Some(value) = value {
                tool["core"] = value;
            }
            ToolDefinition::parse(&serde_json::to_vec(&tool).unwrap(), "lookup")
                .unwrap()
                .core
                == Some(true)
        };
        assert!(!parse(None));
        assert!(!parse(Some(Value::Null)));
        assert!(!parse(Some(json!(false))));
        assert!(parse(Some(json!(true))));
        // The harness basics ship as core.
        for name in [
            "load_skill",
            "list_skills",
            "background_calls_cancel",
            "background_calls_read",
            "present_chat_component",
            "search_this_session",
            "read_attachment",
            "recall_compacted",
        ] {
            assert!(is_core(name), "{name}");
        }
        assert!(!is_core("file_write"));
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
        unknown["default_policy"]["admin"] = json!("allow");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&unknown).unwrap(), "lookup").is_err());
        let mut retired = body("lookup");
        retired["policy_ceiling"] = json!("auto");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&retired).unwrap(), "lookup").is_err());
        let mut roles = body("lookup");
        roles["role_access"] = json!(["operator", "admin"]);
        assert!(ToolDefinition::parse(&serde_json::to_vec(&roles).unwrap(), "lookup").is_err());
    }

    #[test]
    fn repeat_guard_is_validated_and_exempt_is_not_configurable() {
        let mut low = body("lookup");
        low["effect"] = json!("write");
        low["repeat_guard"] = json!({"risk":"low","hint":"read it again first"});
        let parsed = ToolDefinition::parse(&serde_json::to_vec(&low).unwrap(), "lookup").unwrap();
        assert_eq!(parsed.repeat_guard.risk, Some(RetryRisk::Low));
        let mut exempt = low.clone();
        exempt["repeat_guard"]["risk"] = json!("exempt");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&exempt).unwrap(), "lookup").is_err());
        let mut read = body("lookup");
        read["effect"] = json!("read");
        read["repeat_guard"] = json!({"risk":"high"});
        assert!(ToolDefinition::parse(&serde_json::to_vec(&read).unwrap(), "lookup").is_err());
        low["repeat_guard"]["warning"] = json!(" ");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&low).unwrap(), "lookup").is_err());
        let mut hint = body("lookup");
        hint["usage_hint"] = json!(" ");
        assert!(ToolDefinition::parse(&serde_json::to_vec(&hint).unwrap(), "lookup").is_err());
    }
}
