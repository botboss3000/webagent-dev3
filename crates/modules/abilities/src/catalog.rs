use crate::ports::Host;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ABILITIES: usize = 128;

pub use webagent_context_contracts::abilities::{AbilityContext, AbilityInclusion, SkillInclusion};
pub fn public_context(value: &Value) -> Option<AbilityContext> {
    let report: AbilityContext = serde_json::from_value(value.clone()).ok()?;
    if report.abilities.len() > MAX_ABILITIES
        || report.abilities.iter().any(|a| {
            !valid_id(&a.id)
                || !matches!(a.visibility.as_str(), "visible" | "discoverable")
                || !matches!(
                    a.availability.as_str(),
                    "visible" | "activated" | "discoverable"
                )
                || a.schemas.len() > 512
                || a.schemas.iter().any(|id| !valid_id(id))
                || a.skills.len() > 512
                || a.skills.iter().any(|s| {
                    !valid_id(&s.id)
                        || !matches!(s.kind.as_str(), "ability" | "tool" | "selectable" | "own")
                        || !matches!(s.mode.as_str(), "visible" | "discoverable" | "off")
                        || !matches!(
                            s.reason.as_str(),
                            "included" | "off" | "discoverable" | "unavailable"
                        )
                })
        })
    {
        return None;
    }
    Some(report)
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EntryKind {
    Ability,
    VirtualCore,
    Oauth,
    Channel,
    Service,
    Placeholder,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Readiness {
    Ready,
    Partial,
    Pending,
    Placeholder,
}

#[derive(Clone, Debug, Serialize)]
struct Ability {
    id: String,
    #[serde(skip_serializing)]
    path: PathBuf,
    manifest_digest: String,
    /// Freshness inputs: the manifest's own bytes and the tool folder names. Folder
    /// contents count toward `manifest_digest` but are not re-read on every check.
    #[serde(skip_serializing)]
    manifest_bytes_digest: String,
    #[serde(skip_serializing)]
    tool_folders: Option<Vec<String>>,
    plugin: Option<crate::package::Package>,
    #[serde(skip_serializing)]
    frozen: bool,
    group: String,
    group_name: String,
    group_order: i32,
    name: String,
    description: String,
    skill_summary: Option<String>,
    migration_note: Option<String>,
    required_module: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    order: i32,
    kind: EntryKind,
    readiness: Readiness,
    config: Value,
    tools: Vec<String>,
    skill: Option<String>,
    tool_skills: BTreeMap<String, String>,
    selectable_skills: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct Manifest {
    display_name: String,
    kind: EntryKind,
    description: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    readiness: Option<Readiness>,
    #[serde(default)]
    config: Value,
    #[serde(default)]
    skill_summary: Option<String>,
    #[serde(default)]
    migration_note: Option<String>,
    #[serde(default)]
    required_module: Option<String>,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    color: Option<String>,
    #[serde(default)]
    order: i32,
    #[serde(default)]
    placeholder: bool,
}

#[derive(Default, Deserialize)]
struct GroupManifest {
    name: Option<String>,
    #[serde(default)]
    order: i32,
}

#[derive(Clone)]
pub struct Catalog<H: Host> {
    host: std::marker::PhantomData<H>,
    optimization_generation: Option<String>,
    root: PathBuf,
    published: bool,
    native_schemas: Vec<Value>,
    abilities: Vec<Ability>,
    version: String,
}

fn optional_setting_value_valid(declaration: &Value, value: &Value) -> bool {
    match declaration["type"].as_str() {
        Some("boolean") => value.is_boolean(),
        Some("select") => declaration["options"].as_array().is_some_and(|options| {
            options
                .iter()
                .any(|o| o["value"] == *value && value.is_string())
        }),
        // Free text such as a site list; `max_length` (characters, default 2000) bounds it.
        Some("text") => value.as_str().is_some_and(|text| {
            let limit = declaration["max_length"].as_u64().unwrap_or(2000);
            u64::try_from(text.chars().count()).is_ok_and(|count| count <= limit)
                && !text.chars().any(|c| c.is_control() && c != '\n')
        }),
        Some("number" | "integer") => {
            let Some(number) = value.as_i64() else {
                return false;
            };
            let bound = |key: &str| match declaration.get(key) {
                None => Some(None),
                Some(value) => value.as_i64().map(Some),
            };
            let (Some(min), Some(max)) = (bound("min"), bound("max")) else {
                return false;
            };
            if min.zip(max).is_some_and(|(min, max)| min > max) {
                return false;
            }
            min.is_none_or(|min| number >= min) && max.is_none_or(|max| number <= max)
        }
        _ => false,
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub fn search_words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_owned)
        .collect()
}

pub fn near_word(query: &str, word: &str) -> bool {
    if query == word || query.len() >= 4 && word.starts_with(query) {
        return true;
    }
    let aliases: &[&str] = match query {
        "code" | "coding" => &["source", "repository"],
        "edit" | "editing" | "change" | "remove" | "delete" | "fix" => &["write", "patch"],
        "inspect" | "browse" => &["read", "review", "search"],
        "compile" | "build" => &["cargo", "worker"],
        "skill" | "skills" => &["guidance"],
        "theme" => &["theming", "appearance", "palette"],
        _ => &[],
    };
    if aliases.contains(&word) {
        return true;
    }
    if !query.is_ascii() || !word.is_ascii() {
        return false;
    }
    let (left, right) = (query.as_bytes(), word.as_bytes());
    if left.len() >= 4 && left.len() == right.len() {
        for index in 0..left.len() - 1 {
            if left[index] == right[index + 1]
                && left[index + 1] == right[index]
                && left[..index] == right[..index]
                && left[index + 2..] == right[index + 2..]
            {
                return true;
            }
        }
    }
    if query.len() < 5 || query.len().abs_diff(word.len()) > 1 {
        return false;
    }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            i += 1;
            j += 1;
            continue;
        }
        edits += 1;
        if edits > 1 {
            return false;
        }
        if left.len() >= right.len() {
            i += 1;
        }
        if right.len() >= left.len() {
            j += 1;
        }
    }
    edits + usize::from(i < left.len() || j < right.len()) == 1
}

/// A `key: a; b` list from a skill's leading front matter, split on `sep`.
fn front_matter_list(text: &str, key: &str, sep: char) -> Vec<String> {
    let mut lines = text.lines();
    if lines.next() != Some("---") {
        return Vec::new();
    }
    lines
        .take_while(|line| *line != "---")
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
        .map(|list| {
            list.split(sep)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .take(12)
                .map(|item| item.chars().take(200).collect())
                .collect()
        })
        .unwrap_or_default()
}

/// The model-facing list_abilities row: enough to choose and load an ability. Full config,
/// skill text and tool policy arrive with load_ability, so discovery stays small.
pub fn discovery_row(entry: &Value) -> Value {
    let tools: Vec<&Value> = entry["tools"]
        .as_array()
        .map(|tools| tools.iter().map(|tool| &tool["name"]).collect())
        .unwrap_or_default();
    json!({
        "id": entry["id"],
        "name": entry["name"],
        "description": entry["description"],
        "group": entry["group_name"],
        "availability": entry["availability"],
        "requires_activation": entry["requires_activation"],
        "tools": tools,
        "selectable_skills": entry["selectable_skills"],
    })
}

pub(crate) fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > limit {
        bail!("Invalid ability file: {}", path.display());
    }
    // A concurrent growth after metadata inspection cannot create an unbounded read.
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .with_context(|| format!("Cannot read ability file: {}", path.display()))?;
    if bytes.len() as u64 > limit {
        bail!("Ability file grew beyond limit");
    }
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let data = read_bytes(path, MAX_MANIFEST_BYTES)?;
    serde_json::from_slice(&data)
        .with_context(|| format!("Invalid ability manifest: {}", path.display()))
}

/// An ability's own guidance: `skills/<ability>.skill.md`.
pub(crate) fn primary_skill_path(ability: &Path, id: &str) -> PathBuf {
    ability.join("skills").join(format!("{id}.skill.md"))
}

pub(crate) fn optional_skill(path: &Path) -> Result<Option<String>> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(_) => {}
    }
    let bytes = read_bytes(path, fs::symlink_metadata(path)?.len())?;
    let text = String::from_utf8(bytes)
        .with_context(|| format!("Invalid ability skill text: {}", path.display()))?;
    if text.trim().is_empty() {
        bail!("Empty ability skill: {}", path.display());
    }
    Ok(Some(text))
}

impl<H: Host> Catalog<H> {
    /// Freeze a completely validated catalog for one immutable publication revision.
    pub fn load_snapshot(root: &Path) -> Result<Self> {
        let mut catalog = Self::load_mode(root, false, true)?;
        catalog.published = true;
        for ability in &mut catalog.abilities {
            ability.frozen = true;
        }
        Ok(catalog)
    }
    /// The catalog of the selected publication `root`: `self` when it was loaded from that
    /// same immutable revision under the current optimization generation, else a fresh load.
    /// Guidance bodies and on-disk presence are still rechecked per use.
    pub fn snapshot_current(&self, root: &Path) -> Result<Self> {
        let generation = H::optimization_declaration()?.map(|(generation, _)| generation);
        if self.published && self.root == root && self.optimization_generation == generation {
            return Ok(self.clone());
        }
        Self::load_snapshot(root)
    }
    pub(crate) fn optimization_generation(&self) -> &Option<String> {
        &self.optimization_generation
    }
    pub(crate) fn freeze(&mut self) {
        self.published = true;
        for ability in &mut self.abilities {
            ability.frozen = true;
        }
    }
    /// Bind only a currently executable isolated owner; pending declarations cannot dispatch.
    pub fn plugin_binding(&self, name: &str) -> Option<Value> {
        self.abilities
            .iter()
            .filter_map(|a| a.plugin.as_ref().filter(|_| Self::executable(a)))
            .find_map(|p| p.binding(name))
    }
    /// Combine compiled contracts with present isolated schemas without probing unrelated modules.
    pub fn all_schemas(&self) -> Vec<Value> {
        let mut schemas = H::native_schemas_without_administrator();
        schemas.extend(self.native_schemas.clone());
        schemas.extend(
            self.abilities
                .iter()
                .filter_map(|a| a.plugin.as_ref().filter(|_| Self::present(a)))
                .flat_map(|p| p.schemas.clone()),
        );
        // Checkout edits: tools turned off leave, live descriptions replace the built ones.
        let drift = H::drift();
        schemas.retain(|schema| {
            !schema["name"]
                .as_str()
                .is_some_and(|name| drift.disabled(name))
        });
        for schema in &mut schemas {
            if let Some(definition) = schema["name"]
                .as_str()
                .and_then(|name| drift.definition(name))
            {
                schema["description"] = json!(definition.description);
            }
        }
        schemas
    }

    /// Validate manifest ownership and guidance before admitting any catalog revision.
    pub fn load(root: &Path) -> Result<Self> {
        Self::load_mode(root, false, false)
    }

    /// Local live sources may explicitly withdraw a declaration by unlinking its manifest.
    /// Present invalid manifests and all immutable publication snapshots stay fail-closed.
    pub fn load_current(root: &Path) -> Result<Self> {
        Self::load_mode(root, true, false)
    }

    fn load_mode(root: &Path, withdraw_missing: bool, publication: bool) -> Result<Self> {
        Self::load_mode_with_optimization(
            root,
            withdraw_missing,
            publication,
            H::optimization_declaration()?,
        )
    }
    pub(crate) fn load_mode_with_optimization(
        root: &Path,
        withdraw_missing: bool,
        publication: bool,
        optimization: Option<(String, Value)>,
    ) -> Result<Self> {
        let optimization_generation = optimization
            .as_ref()
            .map(|(generation, _)| generation.clone());
        let native_schemas = H::native_schemas_for_catalog(root, publication)?;
        let mut abilities = Vec::new();
        let mut ids = HashSet::new();
        // Built-in Base tools are owned before any folder is read.
        let mut owners: BTreeMap<String, String> = crate::base::TOOLS
            .iter()
            .map(|tool| (tool.to_string(), crate::base::ID.to_string()))
            .collect();
        if root.exists() && fs::symlink_metadata(root)?.file_type().is_symlink() {
            bail!("Symlink catalog root");
        }
        let groups = match fs::read_dir(root) {
            Ok(groups) => groups,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !publication => {
                let version = hex::encode(Sha256::digest(serde_json::to_vec(&json!([
                    abilities,
                    native_schemas
                ]))?));
                return Ok(Self {
                    host: std::marker::PhantomData,
                    optimization_generation,
                    abilities,
                    version,
                    root: root.to_owned(),
                    published: false,
                    native_schemas,
                });
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("Cannot read ability catalog: {}", root.display()))
            }
        };
        for group in groups {
            let group = group?;
            if group.file_type()?.is_symlink() {
                bail!("Symlink ability group");
            }
            if !group.file_type()?.is_dir() {
                continue;
            }
            let group_id = group.file_name().to_string_lossy().to_string();
            if group_id == "Administrator" && !H::administrator_tier() {
                continue;
            }
            if group_id.is_empty()
                || group_id.len() > 64
                || !group_id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                bail!("Invalid ability group identifier");
            }
            let group_path = group.path();
            let group_meta = group_path.join("_group.json");
            let group_meta: GroupManifest = match fs::symlink_metadata(&group_meta) {
                Ok(_) => read_json(&group_meta)?,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    GroupManifest::default()
                }
                Err(error) => return Err(error.into()),
            };
            let group_name = group_meta.name.unwrap_or_else(|| group_id.clone());
            if group_name.is_empty() || group_name.len() > 100 {
                bail!("Invalid ability group name");
            }
            for folder in fs::read_dir(&group_path)? {
                let folder = folder?;
                if folder.file_type()?.is_symlink() {
                    bail!("Symlink ability folder");
                }
                if !folder.file_type()?.is_dir() {
                    continue;
                }
                let id = folder.file_name().to_string_lossy().to_string();
                if !valid_id(&id) || !ids.insert(id.clone()) {
                    bail!("Invalid or duplicate ability identifier: {id}");
                }
                // Core/base holds only Base guidance files; Base itself is built in below.
                if group_id == "Core" && id == crate::base::ID {
                    continue;
                }
                if matches!(id.as_str(), "optimization" | crate::base::ID) {
                    bail!("Ability ID is reserved for its executable plugin");
                }
                let ability_path = folder.path();
                // Pin exactly the bytes parsed here; live edits withdraw cached authority
                // until a freshly validated catalog reload adopts the new declaration.
                // `native_tools` only holds definitions of Administrator native tools no
                // ability owns a folder for; retired abilities keep backend files only.
                if group_id == "Administrator"
                    && matches!(
                        id.as_str(),
                        "native_tools" | "computer_control" | "program_screenshot"
                    )
                    && !ability_path.join(format!("{id}.json")).exists()
                {
                    continue;
                }
                let manifest_path = ability_path.join(format!("{id}.json"));
                if withdraw_missing {
                    match fs::symlink_metadata(&manifest_path) {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                        Err(error) => return Err(error.into()),
                        Ok(_) => {}
                    }
                }
                let manifest_bytes = read_bytes(&manifest_path, MAX_MANIFEST_BYTES)?;
                let manifest_bytes_digest = hex::encode(Sha256::digest(&manifest_bytes));
                let mut manifest_digest = manifest_bytes_digest.clone();
                let mut manifest: Manifest = serde_json::from_slice(&manifest_bytes)
                    .with_context(|| format!("Invalid ability manifest: {id}"))?;
                // Per-tool folders replace the manifest's tool list; an invalid folder
                // drops only its own tool.
                let folder_tools = crate::tool_definitions::read_folder(&ability_path)?;
                let mut folder_skills = BTreeMap::new();
                if let Some(tools) = &folder_tools {
                    // Packaged abilities keep their manifest list for their package
                    // contract; it must then name exactly the tool folders.
                    let listed: BTreeSet<&str> =
                        manifest.tools.iter().map(String::as_str).collect();
                    let folders: BTreeSet<&str> =
                        tools.iter().map(|tool| tool.name.as_str()).collect();
                    if !manifest.tools.is_empty() && listed != folders {
                        bail!("Ability's manifest tools differ from its tool folders: {id}");
                    }
                    manifest_digest =
                        crate::tool_definitions::folded_digest(&manifest_digest, tools);
                    let order = std::mem::take(&mut manifest.tools);
                    let mut valid = BTreeSet::new();
                    for tool in tools {
                        if let Err(error) = &tool.definition {
                            eprintln!("Tool {} in ability {id} is disabled: {error:#}", tool.name);
                            continue;
                        }
                        valid.insert(tool.name.clone());
                        if let Some(skill) = &tool.skill {
                            folder_skills.insert(tool.name.clone(), skill.clone());
                        }
                    }
                    // Keep a listed order; otherwise folder order.
                    manifest.tools = if order.is_empty() {
                        valid.into_iter().collect()
                    } else {
                        order
                            .into_iter()
                            .filter(|name| valid.contains(name))
                            .collect()
                    };
                }
                if manifest.display_name.is_empty()
                    || manifest.display_name.len() > 100
                    || manifest.description.is_empty()
                    || manifest.description.len() > 1000
                    || manifest.tools.len() > 64
                    || manifest
                        .skill_summary
                        .as_ref()
                        .is_some_and(|s| s.len() > 2000)
                    || manifest
                        .migration_note
                        .as_ref()
                        .is_some_and(|s| s.len() > 2000)
                {
                    bail!("Invalid active ability: {id}");
                }
                let plugin =
                    crate::package::Package::load::<H>(&ability_path, &id, &manifest.tools)?;
                if plugin.is_some() && manifest.kind != EntryKind::Ability {
                    bail!("Isolated packages cannot declare trusted core or service kinds");
                }
                let installed = |name: &str| {
                    plugin.as_ref().is_some_and(|p| p.owns(name)) || Self::supported(name)
                };
                let readiness = manifest.readiness.clone().unwrap_or_else(|| {
                    if manifest.placeholder || manifest.kind == EntryKind::Placeholder {
                        Readiness::Placeholder
                    } else if manifest.tools.iter().any(|tool| installed(tool)) {
                        Readiness::Ready
                    } else {
                        Readiness::Pending
                    }
                });
                if (manifest.placeholder || manifest.kind == EntryKind::Placeholder)
                    && (readiness != Readiness::Placeholder || !manifest.tools.is_empty())
                {
                    bail!("Invalid placeholder: {id}");
                }
                if manifest
                    .required_module
                    .as_deref()
                    .is_some_and(|module| !valid_id(module) || !H::module_known(module))
                {
                    bail!("Unknown optional module requirement: {id}");
                }
                // An omitted optional service must not prevent ordinary app startup.
                // Execution still requires its live healthy package and schemas.
                // MCP's package is installed separately; `present` hides it until it runs.
                let omitted_module = id == "mcp"
                    || manifest
                        .required_module
                        .as_deref()
                        .is_some_and(|module| !H::module_available(module));
                if !omitted_module
                    && matches!(readiness, Readiness::Ready | Readiness::Partial)
                    && (!matches!(manifest.kind, EntryKind::Ability | EntryKind::VirtualCore)
                        || !manifest.tools.iter().any(|tool| installed(tool)))
                {
                    bail!("Ready entry has no installed adapter: {id}");
                }
                for directory in ["tools", "skills"] {
                    let path = ability_path.join(directory);
                    if fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir()) {
                        bail!("Invalid ability asset directory: {id}");
                    }
                }
                let mut local = HashSet::new();
                let mut tool_skills = BTreeMap::new();
                for tool in &manifest.tools {
                    if !valid_id(tool) {
                        bail!("Invalid tool identifier: {tool}");
                    }
                    // Installed schemas have exactly one authority-owning entry.
                    if owners.insert(tool.clone(), id.clone()).is_some() {
                        bail!("Duplicate installed tool owner: {tool}");
                    }
                    if !local.insert(tool) {
                        bail!("Ability {id} declares a duplicate tool: {tool}");
                    }
                    let skill = if folder_tools.is_some() {
                        folder_skills.remove(tool)
                    } else {
                        optional_skill(
                            &ability_path.join("tools").join(format!("{tool}.skill.md")),
                        )?
                    };
                    if let Some(skill) = skill {
                        tool_skills.insert(tool.clone(), skill);
                    }
                }
                let mut selectable_skills = BTreeMap::new();
                let skills_path = ability_path.join("skills");
                if skills_path.is_dir() {
                    for asset in fs::read_dir(&skills_path)? {
                        let asset = asset?;
                        let filename = asset.file_name().to_string_lossy().to_string();
                        let Some(key) = filename.strip_suffix(".skill.md") else {
                            bail!("Invalid selectable skill asset: {id}");
                        };
                        // `skills/<ability>.skill.md` is the ability's own guidance.
                        if key == id {
                            continue;
                        }
                        if !valid_id(key) || selectable_skills.len() >= 32 {
                            bail!("Invalid selectable skill identifier: {id}");
                        }
                        let text =
                            optional_skill(&asset.path())?.context("Missing selectable skill")?;
                        selectable_skills.insert(key.to_owned(), text);
                    }
                }
                if !manifest.config.is_null() && !manifest.config.is_object() {
                    bail!("Invalid configuration metadata: {id}");
                }
                let skill = optional_skill(&primary_skill_path(&ability_path, &id))?;
                abilities.push(Ability {
                    id,
                    path: ability_path,
                    manifest_digest,
                    manifest_bytes_digest,
                    tool_folders: folder_tools
                        .as_ref()
                        .map(|tools| tools.iter().map(|tool| tool.name.clone()).collect()),
                    plugin,
                    frozen: false,
                    group: group_id.clone(),
                    group_name: group_name.clone(),
                    group_order: group_meta.order,
                    name: manifest.display_name,
                    description: manifest.description,
                    skill_summary: manifest.skill_summary,
                    migration_note: manifest.migration_note,
                    required_module: manifest.required_module,
                    icon: manifest.icon,
                    color: manifest.color,
                    order: manifest.order,
                    kind: manifest.kind,
                    readiness,
                    config: manifest.config,
                    tools: manifest.tools,
                    skill,
                    tool_skills,
                    selectable_skills,
                });
                if abilities.len() > MAX_ABILITIES {
                    bail!("Too many abilities");
                }
            }
        }
        // A reserved dormant entry lets a validated package added after startup
        // become available without reloading unrelated ability declarations.
        if let Some((generation, declaration)) = optimization {
            let manifest: Manifest = serde_json::from_value(declaration)?;
            abilities.push(Ability {
                id: "optimization".into(),
                path: H::optimization_folder(),
                manifest_digest: generation,
                manifest_bytes_digest: String::new(),
                tool_folders: None,
                plugin: None,
                frozen: false,
                group: "Administrator".into(),
                group_name: "Administrator".into(),
                group_order: 100,
                name: manifest.display_name,
                description: manifest.description,
                skill_summary: manifest.skill_summary,
                required_module: manifest.required_module,
                migration_note: manifest.migration_note,
                icon: manifest.icon,
                color: manifest.color,
                order: manifest.order,
                kind: manifest.kind,
                readiness: Readiness::Ready,
                config: manifest.config,
                tools: manifest.tools,
                skill: H::optimization_guidance(),
                tool_skills: BTreeMap::new(),
                selectable_skills: BTreeMap::new(),
            });
        }
        // Base sorts with the Core group's other abilities when that group exists.
        let (group_name, group_order) = abilities
            .iter()
            .find(|ability| ability.group == "Core")
            .map_or(("Core".to_string(), 1), |core| {
                (core.group_name.clone(), core.group_order)
            });
        let base_path = root.join("Core").join(crate::base::ID);
        let base_skill = optional_skill(&primary_skill_path(&base_path, crate::base::ID))?;
        // Base tools are `tools/<tool>/` folders; their definitions load through
        // `tool_definitions`, and their skills and bytes join Base's identity here.
        let base_tools = crate::tool_definitions::read_folder(&base_path)?.unwrap_or_default();
        let base_tool_skills: BTreeMap<String, String> = base_tools
            .iter()
            .filter_map(|tool| Some((tool.name.clone(), tool.skill.clone()?)))
            .collect();
        let base_digest = hex::encode(Sha256::digest(serde_json::to_vec(&json!([
            base_skill,
            crate::tool_definitions::folded_digest("", &base_tools)
        ]))?));
        abilities.push(Ability {
            id: crate::base::ID.into(),
            path: base_path,
            manifest_digest: base_digest,
            manifest_bytes_digest: String::new(),
            tool_folders: None,
            plugin: None,
            frozen: false,
            group: "Core".into(),
            group_name,
            group_order,
            name: crate::base::NAME.into(),
            description: crate::base::DESCRIPTION.into(),
            skill_summary: None,
            migration_note: Some(crate::base::MIGRATION_NOTE.into()),
            required_module: None,
            icon: Some(crate::base::ICON.into()),
            color: Some(crate::base::COLOR.into()),
            order: 0,
            kind: EntryKind::VirtualCore,
            readiness: Readiness::Ready,
            config: json!({}),
            tools: crate::base::TOOLS
                .iter()
                .map(|tool| tool.to_string())
                .collect(),
            skill: base_skill,
            tool_skills: base_tool_skills,
            selectable_skills: BTreeMap::new(),
        });
        abilities.sort_by(|a, b| {
            (a.group_order, &a.group_name, a.order, &a.name, &a.id).cmp(&(
                b.group_order,
                &b.group_name,
                b.order,
                &b.name,
                &b.id,
            ))
        });
        let version = hex::encode(Sha256::digest(serde_json::to_vec(&json!([
            abilities,
            native_schemas
        ]))?));
        Ok(Self {
            host: std::marker::PhantomData,
            optimization_generation,
            abilities,
            version,
            root: root.to_owned(),
            published: false,
            native_schemas,
        })
    }

    pub fn empty() -> Self {
        Self {
            host: std::marker::PhantomData,
            optimization_generation: None,
            root: PathBuf::new(),
            published: false,
            native_schemas: Vec::new(),
            abilities: Vec::new(),
            version: hex::encode(Sha256::digest(b"[]")),
        }
    }
    pub fn snapshot_root(&self) -> &Path {
        &self.root
    }
    pub fn published(&self) -> bool {
        self.published
    }
    pub fn source_root(&self) -> Option<&Path> {
        self.root
            .ends_with("abilities")
            .then(|| self.root.parent())
            .flatten()
    }
    /// Recheck the Administrator schemas at the final dispatch fence: reread this
    /// snapshot's own tool folders and compare.
    pub fn native_snapshot_current(&self) -> Result<bool> {
        Ok(H::native_schemas_for_catalog(&self.root, self.published)? == self.native_schemas)
    }
    pub fn version(&self) -> &str {
        &self.version
    }

    fn present(ability: &Ability) -> bool {
        // MCP tools work only while its separately installed package is present.
        if ability.id == "mcp" && !H::mcp_available() {
            return false;
        }
        if ability.id == crate::base::ID {
            return true;
        }
        if ability.id == "optimization" {
            return H::optimization_available();
        }
        // Presence alone cannot retain an old executable declaration after an
        // operator disables or edits its manifest. Unreadable/changed bytes fail closed.
        // Catalog checks ask once per tool for every ability, so one disk answer per
        // ability folder and manifest digest is reused for a second.
        type Recent = HashMap<(PathBuf, String), (std::time::Instant, bool)>;
        static RECENT: std::sync::Mutex<Option<Recent>> = std::sync::Mutex::new(None);
        let key = (ability.path.clone(), ability.manifest_bytes_digest.clone());
        // Tests edit fixtures and expect the change at once, so they never reuse.
        if cfg!(not(any(test, feature = "test-support"))) {
            let recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((at, answer)) = recent.as_ref().and_then(|map| map.get(&key)) {
                if at.elapsed() < std::time::Duration::from_secs(1) {
                    return *answer;
                }
            }
        }
        let answer = Self::present_on_disk(ability);
        let mut recent = RECENT.lock().unwrap_or_else(|e| e.into_inner());
        let map = recent.get_or_insert_with(HashMap::new);
        // Bounded: one entry per catalog ability folder; old releases' paths are dropped.
        if map.len() > 1024 {
            map.clear();
        }
        map.insert(key, (std::time::Instant::now(), answer));
        answer
    }

    fn present_on_disk(ability: &Ability) -> bool {
        [
            ability.path.as_path(),
            ability.path.parent().unwrap_or(ability.path.as_path()),
            ability
                .path
                .parent()
                .and_then(Path::parent)
                .unwrap_or(ability.path.as_path()),
        ]
        .into_iter()
        .all(|path| fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()))
            && read_bytes(
                &ability.path.join(format!("{}.json", ability.id)),
                MAX_MANIFEST_BYTES,
            )
            .is_ok_and(|bytes| hex::encode(Sha256::digest(&bytes)) == ability.manifest_bytes_digest)
            && crate::tool_definitions::folder_names(&ability.path) == ability.tool_folders
    }

    // Only registered, validated bundle IDs can select an asset. Each model call
    // reads their current bodies from the pinned catalog source or published pointer.
    /// Published guidance follows the broker pointer; compatible Codebase Admin metadata is live.
    /// Tool/schema grants remain bound to the installed core interface.
    fn published_skill_path(
        ability: &Ability,
        kind: &str,
        name: &str,
        ui_root: Option<&Path>,
    ) -> Option<PathBuf> {
        if ability.frozen || ability.id != "codebase_admin" || !valid_id(name) {
            return None;
        }
        let root = ui_root.filter(|root| root.ends_with(Path::new("v3/ui")))?;
        let folder = root
            .parent()?
            .join(format!("abilities/Administrator/{}", ability.id));
        match kind {
            "ability" if name == ability.id => Some(primary_skill_path(&folder, &ability.id)),
            "tool" if ability.tools.iter().any(|tool| tool == name) => {
                Some(crate::tool_definitions::skill_path(&folder, name))
            }
            "selectable" if ability.selectable_skills.contains_key(name) => {
                Some(folder.join("skills").join(format!("{name}.skill.md")))
            }
            _ => None,
        }
    }
    /// Resolve supported independently published guidance through the trusted asset pointer.
    fn published_skill(ability: &Ability, kind: &str, name: &str) -> Option<PathBuf> {
        let root = std::env::var_os("V3_UI_ROOT").map(PathBuf::from);
        Self::published_skill_path(ability, kind, name, root.as_deref())
    }
    /// Read current text only for a registered asset. Frozen catalogs retain their
    /// revision-owned paths instead of following the mutable publication pointer.
    fn asset_text<'a>(
        ability: &'a Ability,
        kind: &str,
        name: &str,
        cached: &'a str,
    ) -> Option<Cow<'a, str>> {
        // Base guidance is read with each catalog load, so the cached text is current.
        if ability.id == crate::base::ID {
            let declared = match kind {
                "ability" => name == ability.id,
                "tool" => ability.tool_skills.contains_key(name),
                "selectable" => ability.selectable_skills.contains_key(name),
                _ => false,
            };
            return declared.then_some(Cow::Borrowed(cached));
        }
        if ability.id == "optimization" {
            return (kind == "ability" && name == "optimization")
                .then(H::optimization_guidance)
                .flatten()
                .map(Cow::Owned);
        }
        let declared = match kind {
            "ability" => name == ability.id && ability.skill.is_some(),
            "tool" => {
                ability.tools.iter().any(|tool| tool == name)
                    && ability.tool_skills.contains_key(name)
            }
            "selectable" => ability.selectable_skills.contains_key(name),
            _ => false,
        };
        if !declared || !valid_id(name) {
            return None;
        }
        // Changed guidance in the live checkout replaces the built text.
        let live = H::drift()
            .guidance(&ability.id, kind, name)
            .map(Path::to_path_buf);
        let path = live
            .or_else(|| Self::published_skill(ability, kind, name))
            .unwrap_or_else(|| match kind {
                "ability" => primary_skill_path(&ability.path, name),
                "tool" => crate::tool_definitions::skill_path(&ability.path, name),
                _ => ability.path.join("skills").join(format!("{name}.skill.md")),
            });
        if !path.parent().is_some_and(|parent| {
            fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_dir())
        }) {
            return None;
        }
        optional_skill(&path).ok().flatten().map(Cow::Owned)
    }

    /// Reject a body that changed between the source read and its use.
    fn skill_live(ability: &Ability, kind: &str, name: &str, text: &str) -> bool {
        Self::asset_text(ability, kind, name, text).is_some_and(|current| current == text)
    }

    /// Resolve current core guidance from its registered source.
    fn core_skill(ability: &Ability) -> Option<Cow<'_, str>> {
        if ability.id == "optimization" {
            return H::optimization_guidance().map(Cow::Owned);
        }
        ability
            .skill
            .as_deref()
            .and_then(|text| Self::asset_text(ability, "ability", &ability.id, text))
    }

    /// Fingerprint exact current bodies, including withdrawal and reappearance.
    /// Catalog declarations and executable grants remain independently validated.
    pub fn live_version(&self) -> String {
        let validity = self
            .abilities
            .iter()
            .map(|ability| {
                let present = Self::present(ability);
                let mut assets = Vec::new();
                let mut add = |kind: &str, name: &str, body: Option<Cow<'_, str>>| {
                    assets.push(json!([
                        kind,
                        name,
                        body.filter(|_| present)
                            .map(|text| hex::encode(Sha256::digest(text.as_bytes())))
                    ]));
                };
                add("ability", &ability.id, Self::core_skill(ability));
                for (name, text) in &ability.tool_skills {
                    add("tool", name, Self::asset_text(ability, "tool", name, text));
                }
                for (name, text) in &ability.selectable_skills {
                    add(
                        "selectable",
                        name,
                        Self::asset_text(ability, "selectable", name, text),
                    );
                }
                json!([ability.id, present, assets])
            })
            .collect::<Vec<_>>();
        hex::encode(Sha256::digest(
            serde_json::to_vec(&json!([
                self.version,
                validity,
                H::optimization_generation()
            ]))
            .expect("Current catalog validity is serializable"),
        ))
    }

    /// Read current registered bodies after executable, installed-tool and permission gates.
    fn bundled_candidates<'a>(
        &'a self,
        ability_id: &str,
        visibility: &Value,
        state: &Value,
        allow_ability: &impl Fn(&str) -> bool,
        allow_tool: &impl Fn(&str) -> bool,
    ) -> Result<(&'a Ability, Vec<(&'static str, &'a str, Cow<'a, str>)>)> {
        if !valid_id(ability_id) {
            bail!("Invalid bundled ability ID");
        }
        let ability = self
            .abilities
            .iter()
            .find(|a| a.id == ability_id)
            .filter(|a| {
                Self::available(a, visibility, state)
                    && allow_ability(ability_id)
                    && a.tools.iter().any(|tool| {
                        self.installed(tool)
                            && Self::tool_mode(visibility, tool) != "off"
                            && allow_tool(tool)
                    })
            })
            .context("Bundled skills unavailable under current access")?;
        let mut assets = Vec::new();
        if Self::skill_mode(visibility, ability_id) != "off" {
            if let Some(text) = Self::core_skill(ability) {
                assets.push(("ability", ability.id.as_str(), text));
            }
        }
        for (name, text) in &ability.tool_skills {
            if self.installed(name)
                && Self::tool_mode(visibility, name) != "off"
                && allow_tool(name)
            {
                if let Some(body) = Self::asset_text(ability, "tool", name, text) {
                    assets.push(("tool", name.as_str(), body));
                }
            }
        }
        for (name, text) in &ability.selectable_skills {
            let Some(body) = Self::asset_text(ability, "selectable", name, text) else {
                continue;
            };
            if Self::selectable_metadata(name, &body).is_some()
                && Self::selectable_mode(visibility, ability_id, name, &body) != "off"
            {
                assets.push(("selectable", name.as_str(), body));
            }
        }
        assets.retain(|(kind, name, text)| Self::skill_live(ability, kind, name, text));
        Ok((ability, assets))
    }

    /// Project current authorized skill metadata without returning instruction text.
    pub fn bundled_skill_list(
        &self,
        ability_id: &str,
        visibility: &Value,
        state: &Value,
        allow_ability: impl Fn(&str) -> bool,
        allow_tool: impl Fn(&str) -> bool,
    ) -> Result<Vec<Value>> {
        let (ability, assets) =
            self.bundled_candidates(ability_id, visibility, state, &allow_ability, &allow_tool)?;
        Ok(assets
            .into_iter()
            .map(|(kind, name, text)| {
                let mode = Self::asset_mode(visibility, ability, kind, name, &text);
                let supplied = mode == "visible";
                // Discovery hints: a tool skill uses its tool's, a core skill its ability's text.
                let (examples, keywords) = match kind {
                    "tool" => crate::tool_match::hints(name),
                    "ability" => (
                        [Some(&ability.description), ability.skill_summary.as_ref()]
                            .into_iter()
                            .flatten()
                            .cloned()
                            .collect(),
                        Vec::new(),
                    ),
                    _ => (
                        front_matter_list(&text, "examples", ';'),
                        front_matter_list(&text, "keywords", ','),
                    ),
                };
                json!({"ability_id":ability.id,"kind":kind,"skill_id":name,
                "examples":examples,"keywords":keywords,
                "mode":mode,"availability":if supplied {"visible"} else {"discoverable"},
                "requires_activation":!supplied,
                "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
                "summary":text.lines().take(16).find_map(|line|
                    line.strip_prefix("when_to_use:").or_else(||line.strip_prefix("title:"))
                        .or_else(||line.strip_prefix("# ")))
                    .map(|line|line.trim().chars().take(240).collect::<String>())})
            })
            .collect())
    }

    /// Resolve independent core, tool and selectable visibility from current caller configuration.
    fn asset_mode<'a>(
        visibility: &'a Value,
        ability: &Ability,
        kind: &str,
        name: &str,
        text: &str,
    ) -> &'a str {
        match kind {
            "ability" => Self::skill_mode(visibility, &ability.id),
            "tool" => Self::tool_mode(visibility, name),
            _ => Self::selectable_mode(visibility, &ability.id, name, text),
        }
    }

    /// Return one current body and observation metadata under the scoped catalog gates.
    pub fn bundled_skill_load(
        &self,
        ability_id: &str,
        kind: &str,
        skill_id: &str,
        visibility: &Value,
        state: &Value,
        allow_ability: impl Fn(&str) -> bool,
        allow_tool: impl Fn(&str) -> bool,
    ) -> Result<Value> {
        if !valid_id(skill_id) || !matches!(kind, "ability" | "tool" | "selectable") {
            bail!("Invalid bundled skill ID or kind");
        }
        let (ability, assets) =
            self.bundled_candidates(ability_id, visibility, state, &allow_ability, &allow_tool)?;
        let (_, _, text) = assets
            .into_iter()
            .find(|(k, name, _)| *k == kind && *name == skill_id)
            .context("Bundled skill unavailable under current access")?;
        Ok(
            json!({"ability_id":ability.id,"kind":kind,"skill_id":skill_id,
            "mode":Self::asset_mode(visibility,ability,kind,skill_id,&text),
            "availability":if Self::asset_mode(visibility,ability,kind,skill_id,&text)=="visible" {"visible"} else {"discoverable"},
            "requires_activation":Self::asset_mode(visibility,ability,kind,skill_id,&text)!="visible",
            "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
            "text":text,"source_kind":"untrusted_bundled_guidance","activates":false}),
        )
    }

    fn executable(ability: &Ability) -> bool {
        Self::present(ability)
            && ability
                .required_module
                .as_deref()
                .is_none_or(H::module_available)
            && matches!(ability.readiness, Readiness::Ready | Readiness::Partial)
            && matches!(ability.kind, EntryKind::Ability | EntryKind::VirtualCore)
    }

    pub fn ids(&self) -> Vec<String> {
        self.abilities
            .iter()
            .filter(|ability| Self::present(ability))
            .map(|ability| ability.id.clone())
            .collect()
    }

    pub fn known(&self, id: &str) -> bool {
        self.abilities
            .iter()
            .any(|ability| ability.id == id && Self::present(ability))
    }

    /// Optional package configuration is editable independently of installation.
    /// This metadata validator grants neither executable tools nor tenant authority.
    pub fn validate_optional_settings(&self, id: &str, settings: &Value) -> Option<bool> {
        let ability = self.abilities.iter().find(|ability| ability.id == id)?;
        if !Self::present(ability) {
            return Some(false);
        }
        let Some(declarations) = ability.config["settings"].as_array() else {
            return Some(false);
        };
        let Some(values) = settings.as_object() else {
            return Some(false);
        };
        if declarations.is_empty() || declarations.len() != values.len() {
            return Some(false);
        }
        let mut declared = HashSet::new();
        for declaration in declarations {
            let Some(key) = declaration["key"].as_str().filter(|key| valid_id(key)) else {
                return Some(false);
            };
            if !declared.insert(key) {
                return Some(false);
            }
            let Some(default) = declaration.get("default") else {
                return Some(false);
            };
            let Some(value) = values.get(key) else {
                return Some(false);
            };
            if !optional_setting_value_valid(declaration, default)
                || !optional_setting_value_valid(declaration, value)
            {
                return Some(false);
            }
        }
        Some(true)
    }

    /// Whether `name` can run; a tool the checkout turned off cannot.
    fn installed(&self, name: &str) -> bool {
        (self
            .abilities
            .iter()
            .any(|a| a.plugin.as_ref().is_some_and(|p| p.owns(name)))
            || Self::supported(name))
            && !H::drift().disabled(name)
    }
    fn supported(name: &str) -> bool {
        is_discovery(name)
            || H::administrator_supported(name)
            || H::native_schemas_without_administrator()
                .iter()
                .any(|schema| schema["name"] == name)
    }

    fn allowed(policy: &Value, name: &str) -> bool {
        matches!(
            policy["rules"][name].as_str(),
            Some("allow" | "require_approval")
        )
    }

    fn contains_id(list: &Value, id: &str) -> bool {
        list.as_array()
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(id)))
    }

    fn suppressed(state: &Value, id: &str) -> bool {
        Self::contains_id(&state["suppressed"], id)
    }

    fn active(state: &Value, id: &str) -> bool {
        Self::contains_id(&state["active"], id)
    }

    fn exposure<'a>(visibility: &'a Value, field: &str, id: &str, default: &'a str) -> &'a str {
        let configured = visibility[field][id].as_str().unwrap_or(default);
        let caps = &visibility["_capability_exposure"][field];
        let ceiling = caps
            .get(id)
            .or_else(|| caps.get("*"))
            .and_then(Value::as_str)
            .unwrap_or("visible");
        if configured == "off" || ceiling == "off" {
            "off"
        } else if configured == "discoverable" || ceiling == "discoverable" {
            "discoverable"
        } else {
            configured
        }
    }
    fn is_off(visibility: &Value, id: &str) -> bool {
        // Base holds the loaders and core tools, so it is always on; its tools keep their own settings.
        id != crate::base::ID
            && Self::exposure(visibility, "visibility", id, "discoverable") == "off"
    }

    fn skill_mode<'a>(visibility: &'a Value, id: &str) -> &'a str {
        Self::exposure(visibility, "skill_visibility", id, "discoverable")
    }

    fn selectable_metadata(name: &str, text: &str) -> Option<(String, String, &'static str)> {
        let mut lines = text.lines();
        if lines.next()? != "---" {
            return None;
        }
        let (mut id, mut title, mut when, mut mode) = (None, None, None, "discoverable");
        let mut closed = false;
        for line in lines.by_ref() {
            if line == "---" {
                closed = true;
                break;
            }
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim().trim_matches(|c| c == '\'' || c == '"');
            match key.trim() {
                "id" => id = Some(value),
                "title" => title = Some(value),
                "when_to_use" => when = Some(value),
                "default_mode" => mode = value,
                _ => {}
            }
        }
        let title = title.filter(|v| !v.is_empty())?;
        let when = when.filter(|v| !v.is_empty())?;
        if !closed || id != Some(name) || lines.all(|line| line.trim().is_empty()) {
            return None;
        }
        let mode = match mode {
            "visible" | "always" | "always_on" => "visible",
            "discoverable" | "selectable" => "discoverable",
            _ => return None,
        };
        Some((title.to_owned(), when.to_owned(), mode))
    }

    fn selectable_mode<'a>(
        visibility: &'a Value,
        ability: &str,
        name: &str,
        text: &str,
    ) -> &'a str {
        let key = format!("{ability}:{name}");
        Self::exposure(
            visibility,
            "skill_visibility",
            &key,
            Self::selectable_metadata(name, text)
                .map(|(_, _, mode)| mode)
                .unwrap_or("off"),
        )
    }

    fn tool_mode<'a>(visibility: &'a Value, name: &str) -> &'a str {
        // Abilities are groups: tools load one by one, so the loaders themselves stay visible.
        if BOOTSTRAP_TOOLS.contains(&name) {
            return "visible";
        }
        Self::exposure(visibility, "tool_modes", name, "discoverable")
    }

    pub fn has_tool(&self, ability_id: &str, tool_name: &str) -> bool {
        self.abilities.iter().any(|ability| {
            ability.id == ability_id
                && Self::present(ability)
                && ability.tools.iter().any(|tool| tool == tool_name)
        })
    }

    fn is_visible(visibility: &Value, id: &str) -> bool {
        Self::exposure(visibility, "visibility", id, "discoverable") == "visible"
    }

    fn available(ability: &Ability, visibility: &Value, state: &Value) -> bool {
        Self::executable(ability)
            && !Self::is_off(visibility, &ability.id)
            && !Self::suppressed(state, &ability.id)
    }

    fn describe(
        &self,
        ability: &Ability,
        policy: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Value {
        let tools = ability
            .tools
            .iter()
            .filter(|name| {
                Self::executable(ability)
                    && self.installed(name)
                    && Self::allowed(policy, name)
                    && Self::tool_mode(visibility, name) != "off"
            })
            .map(|name| {
                json!({"name":name,"policy":policy["rules"][name],
                        "mode":Self::tool_mode(visibility,name),"supported":self.installed(name)})
            })
            .collect::<Vec<_>>();
        let visible = Self::is_visible(visibility, &ability.id);
        let off = Self::is_off(visibility, &ability.id);
        let mut general_skills = Vec::new();
        if Self::core_skill(ability).is_some() {
            general_skills.push(
                json!({"skill_id":"core","title":"Skill","when_to_use":ability.skill_summary,
                "mode":Self::skill_mode(visibility,&ability.id),"default_mode":"discoverable"}),
            );
        }
        for (name, cached) in &ability.selectable_skills {
            let Some(text) = Self::asset_text(ability, "selectable", name, cached) else {
                continue;
            };
            if !Self::skill_live(ability, "selectable", name, &text) {
                continue;
            }
            if let Some((title, when, default_mode)) = Self::selectable_metadata(name, &text) {
                general_skills.push(json!({"skill_id":name,"title":title,"when_to_use":when,
                    "mode":Self::selectable_mode(visibility,&ability.id,name,&text),
                    "default_mode":default_mode}));
            }
        }
        let mut described = json!({"id":ability.id,"group":ability.group,"group_name":ability.group_name,
            "name":ability.name,"description":ability.description,
            "kind":ability.kind,"readiness":ability.readiness,"config":ability.config,
            "skill_summary":ability.skill_summary,"migration_note":ability.migration_note,
            "entitlement":crate::ability_access::ability_entitlement(&ability.id),
            "icon":ability.icon,"color":ability.color,"tools":tools,
            "has_skill":Self::core_skill(ability).is_some(),
            "selectable_skills":ability.selectable_skills.iter().filter_map(|(name,cached)| Self::asset_text(ability,"selectable",name,cached).filter(|text|Self::skill_live(ability,"selectable",name,text) && Self::selectable_metadata(name,text).is_some() && Self::selectable_mode(visibility,&ability.id,name,text)!="off").map(|_|name)).collect::<Vec<_>>(),
            "general_skills":general_skills,
            "skill_visibility":Self::skill_mode(visibility, &ability.id),
            "visibility":if off {"off"} else if visible {"visible"} else {"discoverable"},
            "active":Self::active(state, &ability.id),
            "availability":if visible {"visible"} else if Self::active(state, &ability.id) {"activated"} else {"discoverable"},
            "requires_activation":!visible && !Self::active(state, &ability.id)});
        if ability.id == "codebase_admin" && !ability.frozen {
            if let Some(path) = Self::published_skill(ability, "ability", "codebase_admin")
                .and_then(|p| p.parent().map(|p| p.join("codebase_admin.json")))
            {
                if let Ok(bytes) = read_bytes(&path, MAX_MANIFEST_BYTES) {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) {
                        for (field, key) in [
                            ("name", "display_name"),
                            ("description", "description"),
                            ("skill_summary", "skill_summary"),
                            ("migration_note", "migration_note"),
                            ("icon", "icon"),
                            ("color", "color"),
                        ] {
                            if let Some(v) = value.get(key) {
                                described[field] = v.clone();
                            }
                        }
                    }
                }
            }
        }
        described
    }

    pub fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value> {
        self.abilities
            .iter()
            .filter(|ability| {
                Self::executable(ability)
                    && !Self::is_off(visibility, &ability.id)
                    && !Self::suppressed(state, &ability.id)
                    && ability.tools.iter().any(|name| {
                        self.installed(name)
                            && Self::allowed(policy, name)
                            && Self::tool_mode(visibility, name) != "off"
                    })
            })
            .map(|ability| self.describe(ability, policy, visibility, state))
            .collect()
    }

    /// Project installed tools and guidance for administrators using one schema snapshot.
    pub fn configuration_catalog(&self, policy: &Value, visibility: &Value) -> Vec<Value> {
        // Resolve dynamic package availability once for this projection.
        let schemas = self.all_schemas();
        self.abilities
            .iter()
            .filter(|ability| Self::present(ability))
            .map(|ability| {
                let mut item = self.describe(ability, policy, visibility, &Value::Null);
                item["available"] = json!(Self::executable(ability) && ability.tools.iter().any(|name| self.installed(name)));
                item["tools"] = json!(ability.tools.iter().map(|name| {
                    let description=schemas.iter().find(|schema|schema["name"]==*name)
                        .and_then(|schema|schema["description"].as_str()).unwrap_or("").to_owned();
                    json!({"name":name,"policy":policy["rules"][name],
                        "mode":Self::tool_mode(visibility,name),"supported":self.installed(name),
                        "description":description,
                        "has_tool_skill":ability.tool_skills.get(name)
                            .is_some_and(|text|Self::asset_text(ability,"tool",name,text).is_some())})
                }).collect::<Vec<_>>());
                item
            })
            .collect()
    }

    /// Administrator viewer projection: current guidance for validated bundle identities.
    /// This does not activate the ability or alter model-facing visibility.
    pub fn configuration_skill(&self, id: &str) -> Option<Cow<'_, str>> {
        let ability = self
            .abilities
            .iter()
            .find(|ability| ability.id == id && Self::present(ability))?;
        Self::core_skill(ability)
    }

    /// Read current viewer guidance and validate selectable frontmatter from current text.
    pub fn configuration_skill_asset(
        &self,
        id: &str,
        kind: &str,
        name: &str,
    ) -> Option<Cow<'_, str>> {
        let ability = self
            .abilities
            .iter()
            .find(|a| a.id == id && Self::present(a))?;
        let text = match kind {
            "skill" if valid_id(name) => ability.selectable_skills.get(name),
            "tool" if valid_id(name) && ability.tools.iter().any(|tool| tool == name) => {
                ability.tool_skills.get(name)
            }
            _ => None,
        }?;
        let body = Self::asset_text(
            ability,
            if kind == "skill" {
                "selectable"
            } else {
                "tool"
            },
            name,
            text,
        )?;
        (kind != "skill" || Self::selectable_metadata(name, &body).is_some()).then_some(body)
    }

    /// Installed owners of a tool, independent of visibility or activation.
    /// A shared tool name must meet every owning ability permission.
    pub fn tool_abilities(
        &self,
        name: &str,
        _visibility: &Value,
        _state: &Value,
        _loaded: &Value,
    ) -> Vec<String> {
        if !self.installed(name) {
            return Vec::new();
        }
        self.abilities
            .iter()
            .filter(|ability| {
                Self::executable(ability) && ability.tools.iter().any(|tool| tool == name)
            })
            .map(|ability| ability.id.clone())
            .collect()
    }

    /// True when any installed ability of the Administrator group lists the tool, executable
    /// or not, so the administrator-role gate fails closed.
    pub fn administrator_tool(&self, name: &str) -> bool {
        self.abilities.iter().any(|ability| {
            ability.group == "Administrator" && ability.tools.iter().any(|tool| tool == name)
        })
    }

    /// True when an ability-provided tool is switched off for this agent: the tool
    /// itself, or every executable ability providing it. Tools no ability provides
    /// are never "off".
    pub fn tool_off(&self, name: &str, visibility: &Value) -> bool {
        let mut providers = self.abilities.iter().filter(|ability| {
            Self::executable(ability) && ability.tools.iter().any(|tool| tool == name)
        });
        let Some(first) = providers.next() else {
            return false;
        };
        Self::tool_mode(visibility, name) == "off"
            || std::iter::once(first)
                .chain(providers)
                .all(|ability| Self::is_off(visibility, &ability.id))
    }

    /// Separate installed support from the effective agent/session visibility.
    /// The two gates do not include operator, tier, or caller permissions.
    pub fn tool_gate(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> (bool, bool) {
        // Conversation selections and context use the same call-local activation.
        // This projection preserves Off, suppression and per-tool discovery gates.
        let state = self.selected_state(visibility, state, loaded);
        let supported = self.installed(name);
        let mut available = false;
        let mut visible = false;
        let selected = Self::contains_id(loaded, &crate::selection::marker(name));
        for ability in &self.abilities {
            if !Self::executable(ability) || !ability.tools.iter().any(|tool| tool == name) {
                continue;
            }
            available |= supported;
            visible |= supported
                && match Self::tool_mode(visibility, name) {
                    "off" => false,
                    // An explicitly activated ability (session state, /optimize) supplies
                    // its discoverable tools; loaded tools add only their own marker.
                    "discoverable" => selected || Self::active(&state, &ability.id),
                    _ => true,
                }
                && (Self::available(ability, visibility, &state)
                    || (selected
                        && !Self::is_off(visibility, &ability.id)
                        && !Self::suppressed(&state, &ability.id)));
        }
        (available, visible)
    }

    /// Whether ability `id` itself can supply tool `name` to this call: it is executable,
    /// declares the tool, and is neither off for the agent nor suppressed in the call-local
    /// selection (saved session state or loaded this run). It need not be activated yet: a
    /// discoverable ability's tools serve it as soon as the model calls them. Another
    /// ability offering the same tool does not count. Tool modes, policy and caller
    /// permissions are decided separately.
    pub fn supplies(
        &self,
        id: &str,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> bool {
        if !self.installed(name) {
            return false;
        }
        let state = self.selected_state(visibility, state, loaded);
        self.abilities.iter().any(|ability| {
            ability.id == id
                && ability.tools.iter().any(|tool| tool == name)
                && Self::available(ability, visibility, &state)
        })
    }

    pub fn tool_available(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> bool {
        let (available, visible) = self.tool_gate(name, visibility, state, loaded);
        available && visible
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn tool_loaded(&self, name: &str, loaded: &Value) -> bool {
        self.abilities.iter().any(|ability| {
            Self::present(ability)
                && ability.tools.iter().any(|tool| tool == name)
                && Self::contains_id(loaded, &ability.id)
        })
    }

    pub fn schemas_with(
        &self,
        policy: &Value,
        loaded: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Vec<Value> {
        self.all_schemas()
            .into_iter()
            .filter(|tool| {
                let name = tool["name"].as_str().unwrap_or("");
                Self::allowed(policy, name) && self.tool_available(name, visibility, state, loaded)
            })
            .collect()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn schemas(&self, policy: &Value, loaded: &Value) -> Vec<Value> {
        let state = json!({"active":loaded,"suppressed":[]});
        self.schemas_with(policy, loaded, &Value::Null, &state)
    }

    pub fn visible_guidance(&self, policy: &Value, visibility: &Value, state: &Value) -> String {
        self.visible_guidance_filtered(policy, visibility, state, |_| true, |_| true)
    }

    /// Assemble model-facing guidance using live ability and tool permissions.
    /// Denied abilities are excluded before their skill or tool text is read.
    pub fn visible_guidance_filtered(
        &self,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        allow_ability: impl Fn(&str) -> bool,
        allow_tool: impl Fn(&str) -> bool,
    ) -> String {
        self.visible_guidance_authorized(
            policy,
            visibility,
            state,
            allow_ability,
            &allow_tool,
            &allow_tool,
        )
    }

    /// Retain compatibility guidance using authorized eligibility and supplied schemas.
    pub fn visible_guidance_authorized(
        &self,
        policy: &Value,
        visibility: &Value,
        state: &Value,
        allow_ability: impl Fn(&str) -> bool,
        eligible_tool: impl Fn(&str) -> bool,
        supplied_tool: impl Fn(&str) -> bool,
    ) -> String {
        let mut text = String::new();
        for ability in &self.abilities {
            if !Self::executable(ability)
                || !allow_ability(&ability.id)
                || Self::is_off(visibility, &ability.id)
                || !ability.tools.iter().any(|tool| {
                    self.installed(tool) && Self::allowed(policy, tool) && eligible_tool(tool)
                })
                || Self::suppressed(state, &ability.id)
            {
                continue;
            }
            // Group guidance arrives with the group's first supplied tool; Visible
            // selectable skills are always on.
            let has_tool = ability.tools.iter().any(|tool| supplied_tool(tool));
            let header = format!("\n\nVisible ability {} ({}):", ability.id, ability.name);
            let mut part = header.clone();
            // Only typed saved settings enter guidance; they never grant use.
            for key in ["terminal_caution", "destructive_caution"] {
                if !has_tool {
                    break;
                }
                if let Some(value) = visibility["settings"][&ability.id][key]
                    .as_str()
                    .filter(|v| matches!(*v, "cautious" | "balanced" | "trusted"))
                {
                    part.push_str(&format!("\nConfigured {key}: {value}. Live Rust policy and approval remain authoritative."));
                }
            }

            // Visible core guidance is mandatory. Discoverable skill loads rebuild
            // their own context layer, independently of ability activation.
            if has_tool && Self::skill_mode(visibility, &ability.id) == "visible" {
                if let Some(skill) = Self::core_skill(ability) {
                    part.push_str("\n");
                    part.push_str(&skill);
                }
            }
            for (tool, cached) in &ability.tool_skills {
                let Some(skill) = Self::asset_text(ability, "tool", tool, cached) else {
                    continue;
                };
                if self.installed(tool)
                    && Self::allowed(policy, tool)
                    && supplied_tool(tool)
                    && Self::skill_live(ability, "tool", tool, &skill)
                    && Self::tool_mode(visibility, tool) != "off"
                {
                    part.push_str("\n\nTool ");
                    part.push_str(tool);
                    part.push_str(":\n");
                    part.push_str(&skill);
                }
            }
            for (name, cached) in &ability.selectable_skills {
                let Some(skill) = Self::asset_text(ability, "selectable", name, cached) else {
                    continue;
                };
                if Self::selectable_metadata(name, &skill).is_some()
                    && Self::selectable_mode(visibility, &ability.id, name, &skill) == "visible"
                    && Self::skill_live(ability, "selectable", name, &skill)
                {
                    part.push_str("\n\nSkill ");
                    part.push_str(name);
                    part.push_str(":\n");
                    part.push_str(&skill);
                }
            }
            // Administrator-selected guidance is mandatory; assembly budgets the complete request.
            if part != header {
                text.push_str(&part);
            }
        }
        text
    }

    // Resolve registered current sources and conversation identities for capability composition.
    /// Project conversation-selected identities into a call-local activation view.
    /// This never changes saved session state or overrides Off, suppression,
    /// executable presence, tool modes, policy, or caller permission checks.
    pub fn selected_state(&self, visibility: &Value, state: &Value, loaded: &Value) -> Value {
        let mut selected = state.clone();
        if !selected.is_object() {
            selected = json!({});
        }
        let mut active = state["active"].as_array().cloned().unwrap_or_default();
        for ability in &self.abilities {
            if !Self::contains_id(loaded, &ability.id)
                || !Self::executable(ability)
                || Self::is_off(visibility, &ability.id)
                || Self::suppressed(state, &ability.id)
                || !ability
                    .tools
                    .iter()
                    .any(|tool| self.installed(tool) && Self::tool_mode(visibility, tool) != "off")
            {
                continue;
            }
            if !active
                .iter()
                .any(|item| item.as_str() == Some(ability.id.as_str()))
            {
                active.push(json!(ability.id));
            }
        }
        selected["active"] = json!(active);
        selected
    }

    /// Read source-backed guidance under the host's already-authorized catalog
    /// and schema projections. This grants no execution authority; callers must
    /// recheck their complete source/policy fingerprint before model dispatch.
    pub fn capability_guidance(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        _loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<webagent_capability_contracts::Guidance> {
        use webagent_capability_contracts::{Guidance, GuidanceKind};
        let sent = schemas
            .iter()
            .filter_map(|schema| schema["name"].as_str())
            .collect::<HashSet<_>>();
        let mut guidance = Vec::new();
        for ability in &self.abilities {
            let id = ability.id.as_str();
            let Some(entry) = entries.iter().find(|entry| entry["id"] == id) else {
                continue;
            };
            if !Self::executable(ability)
                || Self::is_off(visibility, id)
                || Self::suppressed(state, id)
            {
                continue;
            }
            let names = entry["tools"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tool| tool["name"].as_str())
                .filter(|name| {
                    ability.tools.iter().any(|tool| tool.as_str() == *name)
                        && self.installed(name)
                        && Self::tool_mode(visibility, name) != "off"
                })
                .collect::<HashSet<_>>();
            if names.is_empty() {
                continue;
            }
            let normal = Self::available(ability, visibility, state);
            // Group guidance arrives with the group's first sent tool.
            let has_tool = names.iter().any(|name| sent.contains(name));
            let explicit = |kind: &str, name: &str| {
                normal
                    && sent.contains("load_skill")
                    && loaded_skills.as_array().is_some_and(|items| {
                        items.iter().any(|item| {
                            item["ability_id"] == id
                                && item["kind"] == kind
                                && item["skill_id"] == name
                        })
                    })
            };
            // Saved caution remains mandatory independently of core-skill mode.
            // Keeping it separate prevents it from promoting discoverable skill text.
            let mut settings = String::new();
            for key in ["terminal_caution", "destructive_caution"] {
                if let Some(value) = visibility["settings"][id][key]
                    .as_str()
                    .filter(|value| matches!(*value, "cautious" | "balanced" | "trusted"))
                {
                    settings.push_str(&format!("\nConfigured {key}: {value}. Live Rust policy and approval remain authoritative."));
                }
            }
            if !settings.is_empty() {
                guidance.push(Guidance {
                    id: format!("ability:{id}:settings"),
                    kind: GuidanceKind::Ability,
                    body: settings,
                    selected: false,
                    natural: has_tool,
                });
            }
            if Self::skill_mode(visibility, id) != "off" {
                if let Some(body) = Self::core_skill(ability) {
                    guidance.push(Guidance {
                        id: format!("ability:{id}"),
                        kind: GuidanceKind::Ability,
                        body: body.into_owned(),
                        selected: explicit("ability", id),
                        natural: Self::skill_mode(visibility, id) == "visible" && has_tool,
                    });
                }
            }
            for (name, cached) in &ability.tool_skills {
                if !names.contains(name.as_str()) {
                    continue;
                }
                let Some(body) = Self::asset_text(ability, "tool", name, cached) else {
                    continue;
                };
                let mode = Self::tool_mode(visibility, name);
                guidance.push(Guidance {
                    id: format!("tool:{id}:{name}"),
                    kind: GuidanceKind::Tool,
                    body: body.into_owned(),
                    selected: explicit("tool", name),
                    natural: sent.contains(name.as_str()) && mode != "off",
                });
            }
            for (name, cached) in &ability.selectable_skills {
                let Some(body) = Self::asset_text(ability, "selectable", name, cached) else {
                    continue;
                };
                if Self::selectable_metadata(name, &body).is_none() {
                    continue;
                }
                let mode = Self::selectable_mode(visibility, id, name, &body);
                if mode == "off" {
                    continue;
                }
                guidance.push(Guidance {
                    id: format!("selectable:{id}:{name}"),
                    kind: GuidanceKind::Selectable,
                    body: body.into_owned(),
                    selected: explicit("selectable", name),
                    natural: normal && mode == "visible",
                });
            }
        }
        guidance
    }

    /// Trace the same typed inclusion predicates used by guidance producers.
    /// Byte counts refer to included skill content, excluding section headings.
    pub fn context_inclusion(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<AbilityInclusion> {
        let sent = schemas
            .iter()
            .filter_map(|s| s["name"].as_str())
            .collect::<HashSet<_>>();
        entries
            .iter()
            .filter_map(|entry| {
                let id = entry["id"].as_str()?;
                let ability = self.abilities.iter().find(|ability| ability.id == id)?;
                let names = entry["tools"]
                    .as_array()?
                    .iter()
                    .filter_map(|t| t["name"].as_str())
                    .collect::<HashSet<_>>();
                let selected = |name: &str| {
                    sent.contains(name)
                        && Self::contains_id(loaded, &crate::selection::marker(name))
                };
                // Mirrors visible_guidance_authorized: group guidance follows a sent tool.
                let on = Self::available(ability, visibility, state) && !names.is_empty();
                let normal = on && names.iter().any(|name| sent.contains(name));
                let extra = names.iter().any(|name| selected(name));
                let mut skills = Vec::new();
                let mut add = |kind: &str, name: &str, mode: &str, body: &str, natural: bool| {
                    let explicit = mode != "off"
                        && Self::available(ability, visibility, state)
                        && sent.contains("load_skill")
                        && loaded_skills.as_array().is_some_and(|items| {
                            items.iter().any(|item| {
                                item["ability_id"] == id
                                    && item["kind"] == kind
                                    && item["skill_id"] == name
                            })
                        });
                    let included = natural || explicit;
                    let reason = if included {
                        "included"
                    } else if mode == "off" {
                        "off"
                    } else if mode == "discoverable" || entry["availability"] == "discoverable" {
                        "discoverable"
                    } else {
                        "unavailable"
                    };
                    skills.push(SkillInclusion {
                        kind: kind.into(),
                        id: name.into(),
                        mode: mode.into(),
                        included,
                        content_bytes: if included { body.len() } else { 0 },
                        reason: reason.into(),
                    });
                };
                if let Some(body) = Self::core_skill(ability) {
                    let mode = Self::skill_mode(visibility, id);
                    add(
                        "ability",
                        id,
                        mode,
                        &body,
                        mode == "visible" && (normal || extra),
                    );
                }
                for (name, cached) in &ability.tool_skills {
                    let Some(body) = Self::asset_text(ability, "tool", name, cached) else {
                        continue;
                    };
                    if names.contains(name.as_str())
                        && Self::skill_live(ability, "tool", name, &body)
                    {
                        let mode = Self::tool_mode(visibility, name);
                        add(
                            "tool",
                            name,
                            mode,
                            &body,
                            sent.contains(name.as_str()) && mode != "off",
                        );
                    }
                }
                for (name, cached) in &ability.selectable_skills {
                    let Some(body) = Self::asset_text(ability, "selectable", name, cached) else {
                        continue;
                    };
                    if Self::selectable_metadata(name, &body).is_some()
                        && Self::skill_live(ability, "selectable", name, &body)
                    {
                        let mode = Self::selectable_mode(visibility, id, name, &body);
                        add("selectable", name, mode, &body, on && mode == "visible");
                    }
                }
                Some(AbilityInclusion {
                    id: id.into(),
                    visibility: entry["visibility"].as_str()?.into(),
                    availability: entry["availability"].as_str()?.into(),
                    guidance_bytes: skills.iter().map(|skill| skill.content_bytes).sum(),
                    schemas: schemas
                        .iter()
                        .filter_map(|schema| schema["name"].as_str())
                        .filter(|name| names.contains(name))
                        .map(str::to_owned)
                        .collect(),
                    skills,
                })
            })
            .collect()
    }

    /// Render a complete inventory from the exact entries and schemas authorized
    /// for this model call. Discoverable abilities keep their tool schemas hidden.
    pub fn model_index(entries: &[Value], schemas: &[Value]) -> String {
        fn add(out: &mut String, line: &str) {
            out.push_str(line);
            out.push('\n');
        }
        fn short(value: &str) -> String {
            value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .chars()
                .take(160)
                .collect()
        }
        let callable = schemas
            .iter()
            .filter_map(|s| s["name"].as_str())
            .collect::<HashSet<_>>();
        let can_load = callable.contains("load_tool");
        let mut ready_tools = BTreeMap::new();
        // One line per group keeps the index small and stable however many tools it holds.
        let mut groups = Vec::new();
        for entry in entries {
            let Some(id) = entry["id"].as_str() else {
                continue;
            };
            let mut loadable = 0;
            for tool in entry["tools"].as_array().into_iter().flatten() {
                let Some(name) = tool["name"].as_str() else {
                    continue;
                };
                if callable.contains(name) {
                    let description = schemas
                        .iter()
                        .find(|s| s["name"] == name)
                        .and_then(|s| s["description"].as_str())
                        .unwrap_or("");
                    ready_tools.insert(name.to_owned(), short(description));
                } else if tool["mode"] == "discoverable" {
                    loadable += 1;
                }
            }
            if can_load && loadable > 0 {
                let name = short(entry["name"].as_str().unwrap_or(id));
                let summary = short(
                    entry["skill_summary"]
                        .as_str()
                        .or_else(|| entry["description"].as_str())
                        .unwrap_or(""),
                );
                groups.push(format!("- {id} ({loadable}) — {name}: {summary}"));
            }
        }
        if ready_tools.is_empty() && groups.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        add(&mut out, "# [TOOLS]");
        add(
            &mut out,
            "These tools can be called now. A call may still require approval.",
        );
        for (name, description) in ready_tools {
            let row = if description.is_empty() {
                format!("- {name}")
            } else {
                format!("- {name} — {description}")
            };
            add(&mut out, &row);
        }
        if !groups.is_empty() {
            add(&mut out, "");
            add(&mut out, "## Load on demand");
            add(&mut out, "These groups hold more tools (count in brackets). Find what a task needs with list_tools and a few task words (for example 'edit file' or 'screenshot page'); results are ranked, best match first. Use group=<id> only when a query finds nothing useful. Load everything you need with one load_tool call, passing several names in tool_names; each tool brings its own skill.");
            for row in groups {
                add(&mut out, &row);
            }
        }
        out
    }

    /// `list_abilities`/`load_ability` were removed: abilities are On/Off groups and tools load
    /// one by one. shortcut: the engine still routes these names here; delete the route with
    /// the next engine change.
    pub fn discover_with(
        &self,
        _name: &str,
        _args: &Value,
        _run: &mut Value,
        _policy: &Value,
        _visibility: &Value,
        _state: &Value,
    ) -> Value {
        json!({"status":"denied","output":"Abilities are groups and need no loading. Use list_tools and load_tool, or list_skills and load_skill."})
    }

    pub fn discover(&self, name: &str, args: &Value, run: &mut Value, policy: &Value) -> Value {
        self.discover_with(name, args, run, policy, &Value::Null, &Value::Null)
    }
}

/// Always-supplied tools that find and load everything else.
pub const BOOTSTRAP_TOOLS: [&str; 4] = ["list_tools", "load_tool", "list_skills", "load_skill"];

pub fn is_discovery(name: &str) -> bool {
    matches!(name, "list_abilities" | "load_ability")
}

#[cfg(feature = "test-support")]
pub mod registered_tests {
    use super::*;
    pub mod tests {
        use super::*;
        use tempfile::TempDir;

        pub fn index_guidance_is_confined_to_registered_published_assets<H: crate::TestHost>() {
            let (root, catalog) = fixture::<H>();
            let mut ability = catalog.abilities[0].clone();
            let ui = root.path().join("assets/current/v3/ui");
            assert_eq!(Catalog::<H>::published_skill_path(&ability, "ability", "codebase_admin", Some(&ui)).unwrap(),root.path().join("assets/current/v3/abilities/Administrator/codebase_admin/skills/codebase_admin.skill.md"));
            ability.tools = vec!["file_read".into()];
            assert!(
                Catalog::<H>::published_skill_path(&ability, "tool", "../secret", Some(&ui))
                    .is_none()
            );
            assert!(Catalog::<H>::published_skill_path(
                &ability,
                "tool",
                "unregistered",
                Some(&ui)
            )
            .is_none());
            assert!(Catalog::<H>::published_skill_path(
                &ability,
                "ability",
                "codebase_admin",
                Some(Path::new("/tmp/other"))
            )
            .is_none());
            let tool = Catalog::<H>::published_skill_path(&ability, "tool", "file_read", Some(&ui))
                .unwrap();
            fs::create_dir_all(tool.parent().unwrap()).unwrap();
            fs::write(&tool, "First published instructions").unwrap();
            assert_eq!(
                optional_skill(&tool).unwrap().unwrap(),
                "First published instructions"
            );
            fs::write(&tool, "Next published instructions").unwrap();
            assert_eq!(
                optional_skill(&tool).unwrap().unwrap(),
                "Next published instructions"
            );
        }

        /// Share the exact isolated native fixture with focused sibling capability tests.
        pub fn fixture<H: crate::TestHost>() -> (TempDir, Catalog<H>) {
            let root = tempfile::tempdir().unwrap();
            let group = root.path().join("Administrator");
            let ability = group.join("codebase_admin");
            fs::create_dir_all(ability.join("tools")).unwrap();
            fs::write(
                group.join("_group.json"),
                r#"{"name":"Administrator","order":1}"#,
            )
            .unwrap();
            fs::write(ability.join("codebase_admin.json"), r#"{
            "display_name":"Codebase Admin","kind":"ability",
            "description":"Inspect and edit an isolated workspace",
            "tools":["file_list","file_read","file_search","state_inspect","prompt_slot_read","prompt_slot_write","file_write","file_edit","workspace_seed","source_review","source_patch","shell","python"]
        }"#).unwrap();
            fs::create_dir_all(ability.join("skills")).unwrap();
            fs::write(
                ability.join("skills/codebase_admin.skill.md"),
                "Use scoped workspace tools.",
            )
            .unwrap();
            fs::write(
                ability.join("tools/file_edit.skill.md"),
                "Match one exact span.",
            )
            .unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            (root, catalog)
        }

        pub fn visible_large_guidance_is_complete_and_discovery_is_not_activation<
            H: crate::TestHost,
        >() {
            let (root, _) = fixture::<H>();
            let folder = root.path().join("Administrator/codebase_admin");
            let core = format!("CORE-BEGIN{}CORE-END", "c".repeat(30000));
            let tool = format!("TOOL-BEGIN{}TOOL-END", "t".repeat(30000));
            fs::create_dir_all(folder.join("skills")).unwrap();
            fs::write(folder.join("skills/codebase_admin.skill.md"), &core).unwrap();
            fs::write(folder.join("tools/file_edit.skill.md"), &tool).unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let p = policy::<H>();
            let v = json!({"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"visible","file_edit":"visible"}});
            let state = json!({"active":[],"suppressed":[]});
            let guidance = catalog.visible_guidance(&p, &v, &state);
            assert!(guidance.contains(&core));
            assert!(guidance.contains(&tool));
            assert!(!guidance.contains("omitted"));
            // The removed ability loaders are refused and change nothing.
            let mut run = json!({"loaded_abilities":[]});
            let before = run.clone();
            for name in ["list_abilities", "load_ability"] {
                let result = catalog.discover_with(
                    name,
                    &json!({"ability_id":"codebase_admin"}),
                    &mut run,
                    &p,
                    &v,
                    &state,
                );
                assert_eq!(result["status"], "denied");
            }
            assert_eq!(run, before);
            let schemas = catalog.schemas_with(&p, &run["loaded_abilities"], &v, &state);
            let entries = catalog.catalog_with(&p, &v, &state);
            let report =
                catalog.context_inclusion(&entries, &schemas, &v, &state, &json!([]), &json!([]));
            // Tool skills follow their tool; everything else Visible is included.
            assert!(report[0]
                .skills
                .iter()
                .filter(|s| s.kind != "tool" || report[0].schemas.contains(&s.id))
                .all(|s| s.included));
            assert!(report[0].schemas.contains(&"file_read".into()));
            let value = serde_json::to_value(AbilityContext {
                turn: 0,
                call: 1,
                abilities: report,
            })
            .unwrap();
            assert!(!value.to_string().contains("CORE-BEGIN"));
            assert!(public_context(&value).is_some());
        }

        pub fn discoverable_activation_is_durable_without_reloading_or_revealing_discoverable_tools<
            H: crate::TestHost,
        >() {
            let (_root, catalog) = fixture::<H>();
            let p = policy::<H>();
            // An On ability exposes nothing by default: each tool loads on its own.
            let v = json!({"visibility":{"codebase_admin":"discoverable"},
            "tool_modes":{"file_search":"discoverable","file_edit":"off"}});
            let mut state = json!({"active":[],"suppressed":[]});
            let none = json!([]);
            let picked = json!(["tool:file_read", "tool:file_edit"]);
            assert!(!catalog.tool_available("file_read", &v, &state, &none));
            assert!(catalog.tool_available("file_read", &v, &state, &picked));
            assert!(!catalog.tool_available("file_search", &v, &state, &picked));
            // Off stays hidden even when selected.
            assert!(!catalog.tool_available("file_edit", &v, &state, &picked));
            for name in BOOTSTRAP_TOOLS {
                assert_eq!(Catalog::<H>::tool_mode(&v, name), "visible");
            }
            let names = |loaded: &Value, state: &Value| {
                catalog
                    .schemas_with(&p, loaded, &v, state)
                    .into_iter()
                    .map(|s| s["name"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            assert!(!names(&none, &state).contains(&"file_read".into()));
            assert!(names(&picked, &state).contains(&"file_read".into()));
            state["suppressed"] = json!(["codebase_admin"]);
            assert!(catalog.visible_guidance(&p, &v, &state).is_empty());
            assert!(!catalog.tool_available("file_read", &v, &state, &picked));
        }

        pub fn diagnostic_projection_rejects_content_and_unknown_identifiers<H: crate::TestHost>() {
            let value = json!({"turn":1,"call":1,"abilities":[],"prompt":"PRIVATE"});
            assert!(public_context(&value).is_none());
        }

        pub fn search_activation_preserves_visible_tools_and_respects_denials<
            H: crate::TestHost,
        >() {
            let (_root, catalog) = fixture::<H>();
            let mut policy = policy::<H>();
            let visibility = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"visible","file_search":"discoverable","file_edit":"off"}});
            let state = json!({"active":[],"suppressed":[]});
            let mut run = json!({"pending":{"name":"load_tool"},"loaded_abilities":[]});
            let names = |run: &Value, policy: &Value, state: &Value| {
                catalog
                    .schemas_with(policy, &run["loaded_abilities"], &visibility, state)
                    .into_iter()
                    .map(|s| s["name"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            let before = names(&run, &policy, &state);
            assert!(before.contains(&"file_read".into()));
            assert!(!before.contains(&"file_search".into()));
            crate::selection::apply_selection(
                &mut run,
                &json!({"status":"success","output":{"activated":true,
                "selected_tools":["file_search","file_edit","python"]}}),
            );
            let after = names(&run, &policy, &state);
            assert!(before.iter().all(|name| after.contains(name)));
            assert!(after.contains(&"file_search".into()));
            assert!(!after.contains(&"file_edit".into()));
            assert!(!after.contains(&"python".into()));
            crate::selection::apply_selection(
                &mut run,
                &json!({"status":"success","output":{"activated":true,"selected_tools":[]}}),
            );
            assert_eq!(names(&run, &policy, &state), after);
            policy["rules"]["file_search"] = json!("deny");
            assert!(!names(&run, &policy, &state).contains(&"file_search".into()));
            assert!(!names(
                &run,
                &policy,
                &json!({"active":[],"suppressed":["codebase_admin"]})
            )
            .contains(&"file_read".into()));
        }

        pub fn changed_manifest_withdraws_cached_loaded_tools_until_reload<H: crate::TestHost>() {
            for change in ["pending", "remove_tool", "malformed"] {
                let (root, catalog) = fixture::<H>();
                let path = root
                    .path()
                    .join("Administrator/codebase_admin/codebase_admin.json");
                let original = fs::read(&path).unwrap();
                let mut manifest: Value = serde_json::from_slice(&original).unwrap();
                let p = policy::<H>();
                let visibility = json!({"visibility":{"codebase_admin":"visible"},
                    "tool_modes":{"file_read":"visible","file_write":"visible"}});
                let active = json!({"active":["codebase_admin"],"suppressed":[]});
                let loaded = json!(["codebase_admin"]);
                assert_eq!(
                    catalog.tool_gate("file_read", &visibility, &active, &loaded),
                    (true, true)
                );
                match change {
                    "pending" => manifest["readiness"] = json!("pending"),
                    "remove_tool" => manifest["tools"]
                        .as_array_mut()
                        .unwrap()
                        .retain(|t| t != "file_read"),
                    _ => {}
                }
                fs::write(
                    &path,
                    if change == "malformed" {
                        b"{".to_vec()
                    } else {
                        serde_json::to_vec(&manifest).unwrap()
                    },
                )
                .unwrap();
                // All cached entry authority is withdrawn, not just the edited tool.
                assert_eq!(
                    catalog.tool_gate("file_read", &visibility, &active, &loaded),
                    (false, false)
                );
                assert_eq!(
                    catalog.tool_gate("file_write", &visibility, &active, &loaded),
                    (false, false)
                );
                assert!(!catalog
                    .schemas_with(&p, &loaded, &visibility, &active)
                    .iter()
                    .any(|s| s["name"] == "file_read" || s["name"] == "file_write"));
                assert!(!catalog.known("codebase_admin"));
                if change == "remove_tool" {
                    let reloaded = Catalog::<H>::load(root.path()).unwrap();
                    assert_ne!(reloaded.version(), catalog.version());
                    assert_eq!(
                        reloaded.tool_gate("file_read", &visibility, &active, &loaded),
                        (false, false)
                    );
                    assert_eq!(
                        reloaded.tool_gate("file_write", &visibility, &active, &loaded),
                        (true, true)
                    );
                }
            }
        }

        pub fn bundled_skill_access_is_exact_scoped_and_withdraws_changed_assets<
            H: crate::TestHost,
        >() {
            let (root, _) = fixture::<H>();
            let folder = root.path().join("Administrator/codebase_admin");
            let theme = "---\nid: ui_theming\ntitle: UI theming\nwhen_to_use: Use for theme changes\ndefault_mode: visible\n---\nScoped theme guidance";
            fs::write(folder.join("skills/ui_theming.skill.md"), theme).unwrap();
            fs::write(folder.join("skills/ui_toggle_list.skill.md"),
            "---\nid: ui_toggle_list\ntitle: Toggle lists\nwhen_to_use: Use for list controls\ndefault_mode: discoverable\n---\nScoped list guidance").unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let visible = json!({"visibility":{"codebase_admin":"visible"}});
            let active = json!({"active":["codebase_admin"],"suppressed":[]});
            let list = catalog
                .bundled_skill_list(
                    "codebase_admin",
                    &visible,
                    &active,
                    |_| true,
                    |tool| tool == "file_read",
                )
                .unwrap();
            assert_eq!(list.len(), 3);
            assert!(!list.iter().any(|s| s["kind"] == "tool"));
            let loaded = catalog
                .bundled_skill_load(
                    "codebase_admin",
                    "selectable",
                    "ui_theming",
                    &visible,
                    &active,
                    |_| true,
                    |_| true,
                )
                .unwrap();
            assert_eq!(loaded["text"], theme);
            assert_eq!(loaded["source_kind"], "untrusted_bundled_guidance");
            assert_eq!(loaded["activates"], false);
            assert_eq!(
                loaded["sha256"],
                hex::encode(Sha256::digest(theme.as_bytes()))
            );
            assert!(catalog
                .bundled_skill_load(
                    "codebase_admin",
                    "selectable",
                    "../ui_theming",
                    &visible,
                    &active,
                    |_| true,
                    |_| true
                )
                .is_err());
            assert!(catalog
                .bundled_skill_load(
                    "codebase_admin",
                    "tool",
                    "file_edit",
                    &visible,
                    &active,
                    |_| true,
                    |tool| tool == "file_read"
                )
                .is_err());
            assert!(catalog
                .bundled_skill_list("codebase_admin", &visible, &active, |_| false, |_| true)
                .is_err());
            let off = json!({"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"off"}});
            let without_root = catalog
                .bundled_skill_list(
                    "codebase_admin",
                    &off,
                    &active,
                    |_| true,
                    |tool| tool == "file_read",
                )
                .unwrap();
            assert_eq!(without_root.len(), 2);
            assert!(!without_root.iter().any(|skill| skill["kind"] == "ability"));
            let separate = json!({"visibility":{"codebase_admin":"visible"},
            "skill_visibility":{"codebase_admin:ui_theming":"off"}});
            let separate_list = catalog
                .bundled_skill_list(
                    "codebase_admin",
                    &separate,
                    &active,
                    |_| true,
                    |tool| tool == "file_read",
                )
                .unwrap();
            assert!(!separate_list
                .iter()
                .any(|skill| skill["skill_id"] == "ui_theming"));
            assert!(separate_list
                .iter()
                .any(|skill| skill["skill_id"] == "ui_toggle_list"));
            assert!(catalog
                .bundled_skill_list(
                    "codebase_admin",
                    &visible,
                    &json!({"active":["codebase_admin"],"suppressed":["codebase_admin"]}),
                    |_| true,
                    |_| true
                )
                .is_err());
            // An On ability's skills are listable without activation; Off hides them.
            assert!(catalog
                .bundled_skill_list("codebase_admin", &json!({}), &json!({}), |_| true, |_| true)
                .is_ok());
            assert!(catalog
                .bundled_skill_list(
                    "codebase_admin",
                    &json!({"visibility":{"codebase_admin":"off"}}),
                    &json!({}),
                    |_| true,
                    |_| true
                )
                .is_err());

            let before = catalog.live_version();
            fs::write(
                folder.join("skills/ui_theming.skill.md"),
                "Unvalidated replacement",
            )
            .unwrap();
            assert_ne!(catalog.live_version(), before);
            assert!(catalog
                .bundled_skill_load(
                    "codebase_admin",
                    "selectable",
                    "ui_theming",
                    &visible,
                    &active,
                    |_| true,
                    |_| true
                )
                .is_err());
            assert!(!catalog
                .bundled_skill_list("codebase_admin", &visible, &active, |_| true, |_| true)
                .unwrap()
                .iter()
                .any(|s| s["skill_id"] == "ui_theming"));
            fs::remove_file(folder.join("skills/codebase_admin.skill.md")).unwrap();
            fs::remove_file(folder.join("tools/file_edit.skill.md")).unwrap();
            let guidance = catalog.visible_guidance(&policy::<H>(), &visible, &active);
            assert!(!guidance.contains("Use scoped workspace tools."));
            assert!(!guidance.contains("Match one exact span."));
        }

        #[cfg(unix)]

        pub fn symlink_replacement_cannot_restore_cached_bundled_guidance<H: crate::TestHost>() {
            use std::os::unix::fs::symlink;
            let (root, catalog) = fixture::<H>();
            let folder = root.path().join("Administrator/codebase_admin");
            let path = folder.join("skills/codebase_admin.skill.md");
            let original = fs::read(&path).unwrap();
            let alternate = root.path().join("replacement.skill.md");
            fs::write(&alternate, original).unwrap();
            let before = catalog.live_version();
            fs::remove_file(&path).unwrap();
            symlink(&alternate, &path).unwrap();
            assert_ne!(catalog.live_version(), before);
            assert!(catalog
                .bundled_skill_load(
                    "codebase_admin",
                    "ability",
                    "codebase_admin",
                    &json!({"visibility":{"codebase_admin":"visible"}}),
                    &json!({}),
                    |_| true,
                    |_| true
                )
                .is_err());
        }

        /// Keep focused sibling tests on the same installed-tool policy fixture.
        pub fn policy<H: crate::TestHost>() -> Value {
            json!({"rules":{"file_list":"allow","file_read":"allow","file_search":"allow",
            "state_inspect":"allow","prompt_slot_read":"allow","prompt_slot_write":"require_approval","file_write":"require_approval","file_edit":"require_approval","workspace_seed":"require_approval","source_review":"allow","source_patch":"require_approval",
            "shell":"require_approval","python":"deny"}})
        }

        pub fn model_index_advertises_discoverable_abilities_without_hidden_tools<
            H: crate::TestHost,
        >() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let state = json!({"active":[],"suppressed":[]});
            let entries = catalog.catalog_with(&policy, &Value::Null, &state);
            let mut schemas = catalog.schemas_with(&policy, &json!([]), &Value::Null, &state);
            // The fixture compiles no native loader; the index needs load_tool to offer groups.
            schemas.push(json!({"name":"load_tool"}));
            let index = Catalog::<H>::model_index(&entries, &schemas);
            // Discoverable tools appear only as one line per group, never by name.
            assert!(!index.contains("# [ABILITIES]"));
            assert!(index.contains("## Load on demand"));
            assert!(index.contains("- codebase_admin ("));
            assert!(index.contains(") — Codebase Admin"));
            assert!(!index.contains("- file_read"));
            assert!(!index.contains("Use scoped workspace tools."));
            let crowded = Catalog::<H>::model_index(&vec![entries[0].clone(); 512], &schemas);
            assert_eq!(
                crowded
                    .matches(&format!("- {} (", entries[0]["id"].as_str().unwrap()))
                    .count(),
                512
            );

            let visible = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"visible"}});
            let entries = catalog.catalog_with(&policy, &visible, &state);
            let schemas = catalog.schemas_with(&policy, &json!([]), &visible, &state);
            let index = Catalog::<H>::model_index(&entries, &schemas);
            assert!(index.contains("- file_read"));
            assert!(!index.contains("- python"));
            assert!(!index.contains("- file_edit"));

            let suppressed = json!({"active":[],"suppressed":["codebase_admin"]});
            assert!(!Catalog::<H>::model_index(
                &catalog.catalog_with(&policy, &visible, &suppressed),
                &catalog.schemas_with(&policy, &json!([]), &visible, &suppressed)
            )
            .contains("codebase_admin"));
            assert!(!Catalog::<H>::model_index(&[], &[]).contains("codebase_admin"));
        }

        pub fn manifest_and_skill_are_loaded_without_exposing_denied_tools<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let names = |loaded: &Value| {
                catalog
                    .schemas(&policy, loaded)
                    .into_iter()
                    .map(|schema| schema["name"].as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            let start = names(&json!([]));
            assert!(!start.contains(&"list_abilities".to_owned()));
            assert!(!start.contains(&"load_ability".to_owned()));
            assert!(!start.contains(&"file_edit".to_owned()));
            let loaded = json!(["tool:file_edit", "tool:state_inspect", "tool:python"]);
            let names = names(&loaded);
            assert!(names.contains(&"file_edit".to_owned()));
            assert!(names.contains(&"state_inspect".to_owned()));
            // Selection never lifts a policy denial.
            assert!(!names.contains(&"python".to_owned()));
        }

        /// Reject unknown identities while adopting current bytes from the registered source.
        pub fn configuration_skill_rejects_unknown_and_reads_current_assets<H: crate::TestHost>() {
            let (root, catalog) = fixture::<H>();
            assert_eq!(
                catalog.configuration_skill("codebase_admin"),
                Some(Cow::Borrowed("Use scoped workspace tools."))
            );
            assert_eq!(catalog.configuration_skill("../codebase_admin"), None);
            assert_eq!(catalog.configuration_skill("missing"), None);
            fs::write(
                root.path()
                    .join("Administrator/codebase_admin/skills/codebase_admin.skill.md"),
                "changed",
            )
            .unwrap();
            assert_eq!(
                catalog.configuration_skill("codebase_admin").as_deref(),
                Some("changed")
            );
        }

        pub fn duplicate_tools_fail_closed<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            let path = root
                .path()
                .join("Administrator/codebase_admin/codebase_admin.json");
            fs::write(
                &path,
                r#"{"display_name":"Codebase Admin","kind":"ability",
            "description":"Run tools","tools":["shell","shell","python"]}"#,
            )
            .unwrap();
            assert!(Catalog::<H>::load(root.path()).is_err());
        }

        pub fn missing_catalog_or_ability_removes_tools_from_runtime<H: crate::TestHost>() {
            let (root, catalog) = fixture::<H>();
            let missing = root.path().join("removed");
            let empty = Catalog::<H>::load(&missing).unwrap();
            let visibility = json!({"visibility":{"codebase_admin":"visible"}});
            let state = json!({"active":["codebase_admin"]});
            assert!(empty
                .configuration_catalog(&policy::<H>(), &visibility)
                .iter()
                .all(|entry| entry["id"] == crate::base::ID));
            assert!(!empty.tool_available("file_read", &visibility, &state, &Value::Null));
            assert_ne!(empty.version(), catalog.version());
            fs::remove_dir_all(root.path().join("Administrator/codebase_admin")).unwrap();
            assert!(!catalog.tool_available("file_read", &visibility, &state, &Value::Null));
            assert!(catalog
                .configuration_catalog(&policy::<H>(), &visibility)
                .iter()
                .all(|entry| entry["id"] == crate::base::ID));
            let partial = Catalog::<H>::load(root.path()).unwrap();
            assert!(!partial.tool_available("file_read", &visibility, &state, &Value::Null));
        }

        pub fn partial_readiness_exposes_only_compiled_tools_and_withdraws_on_edit<
            H: crate::TestHost,
        >() {
            let (root, _) = fixture::<H>();
            let path = root.path().join("Core/core_probe");
            fs::create_dir_all(&path).unwrap();
            let manifest = path.join("core_probe.json");
            fs::write(
                &manifest,
                r#"{"display_name":"Core probe","kind":"virtual_core",
            "description":"Partial core","readiness":"partial",
            "tools":["get_weather","legacy_unwired_tool"]}"#,
            )
            .unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let policy = json!({"rules":{"get_weather":"allow","legacy_unwired_tool":"allow"}});
            let visibility = json!({"visibility":{"core_probe":"visible"},
            "tool_modes":{"get_weather":"visible","legacy_unwired_tool":"visible"}});
            let state = json!({"active":["core_probe"]});
            let item = catalog
                .configuration_catalog(&policy, &visibility)
                .into_iter()
                .find(|entry| entry["id"] == "core_probe")
                .unwrap();
            assert_eq!(item["readiness"], "partial");
            assert_eq!(item["available"], true);
            assert!(catalog.tool_available("get_weather", &visibility, &state, &Value::Null));
            assert!(!catalog.tool_available(
                "legacy_unwired_tool",
                &visibility,
                &state,
                &Value::Null
            ));
            fs::write(
                &manifest,
                r#"{"display_name":"Core probe","kind":"virtual_core",
            "description":"Disabled","readiness":"pending","tools":["get_weather"]}"#,
            )
            .unwrap();
            assert!(!catalog.tool_available("get_weather", &visibility, &state, &Value::Null));
            let disabled = Catalog::<H>::load(root.path()).unwrap();
            assert!(!disabled.tool_available("get_weather", &visibility, &state, &Value::Null));
        }

        pub fn catalog_only_v2_tool_is_listed_but_never_executable<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            // Discovery tools belong to built-in Base and are always supported.
            let discovery = Catalog::<H>::load(root.path())
                .unwrap()
                .configuration_catalog(&policy::<H>(), &Value::Null)
                .into_iter()
                .find(|item| item["id"] == "base")
                .unwrap();
            assert!(discovery["tools"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|tool| tool["name"].as_str().is_some_and(is_discovery))
                .all(|tool| tool["supported"] == true));
            let path = root.path().join("Administrator/computer_control");
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("computer_control.json"),
                r#"{"display_name":"Computer Control","kind":"ability",
                "description":"Unsupported Computer Control","tools":["computer_screenshot"]}"#,
            )
            .unwrap();
            fs::create_dir_all(path.join("skills")).unwrap();
            fs::write(
                path.join("skills/computer_control.skill.md"),
                "Control the excluded attached computer.",
            )
            .unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let policy = policy::<H>();
            let visibility = json!({"visibility":{"computer_control":"visible"}});
            let item = catalog
                .configuration_catalog(&policy, &visibility)
                .into_iter()
                .find(|item| item["id"] == "computer_control")
                .unwrap();
            assert_eq!(item["available"], false);
            assert_eq!(item["tools"][0]["supported"], false);
            assert!(!catalog
                .catalog_with(&policy, &visibility, &Value::Null)
                .iter()
                .any(|item| item["id"] == "computer_control"));
            assert!(!catalog
                .visible_guidance(&policy, &visibility, &Value::Null)
                .contains("Control the excluded attached computer."));
            let mut run = json!({"loaded_abilities":[]});
            assert_eq!(
                catalog.discover_with(
                    "load_ability",
                    &json!({"ability_id":"computer_control"}),
                    &mut run,
                    &policy,
                    &visibility,
                    &Value::Null
                )["status"],
                "denied"
            );
            assert!(!catalog.tool_available(
                "computer_screenshot",
                &visibility,
                &json!({"active":["computer_control"],"suppressed":[]}),
                &Value::Null
            ));
        }

        pub fn base_stays_on_and_skills_default_discoverable<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let state = json!({"active":[],"suppressed":[]});
            let off = json!({"visibility":{"base":"off","codebase_admin":"visible"}});
            // Saving Base as Off never hides its loaders.
            assert_eq!(
                catalog.tool_gate("list_tools", &off, &state, &Value::Null),
                (true, true)
            );
            let entries = catalog.catalog_with(&policy::<H>(), &off, &state);
            let admin = entries
                .iter()
                .find(|e| e["id"] == "codebase_admin")
                .unwrap();
            // Ability core skills default to discoverable, so none enter guidance unasked.
            assert_eq!(admin["skill_visibility"], "discoverable");
        }

        pub fn loaded_tool_and_skill_owners_expose_no_other_tools<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let state = json!({"active":[],"suppressed":[]});
            let visibility = json!({"visibility":{"codebase_admin":"visible"}});
            // A loaded tool records only its own marker (context-asset hydration adds no
            // owner), so the group's other tools stay hidden.
            let loaded = json!([crate::selection::marker("file_read")]);
            assert!(catalog.tool_available("file_read", &visibility, &state, &loaded));
            assert!(!catalog.tool_available("file_edit", &visibility, &state, &loaded));
            // Explicit session activation still supplies the group's discoverable tools.
            let active = json!({"active":["codebase_admin"],"suppressed":[]});
            assert!(catalog.tool_available("file_edit", &visibility, &active, &loaded));
        }

        pub fn tool_gate_distinguishes_missing_tools_from_hidden_tools<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let state = json!({"active":[],"suppressed":[]});
            let hidden = json!({"visibility":{"codebase_admin":"off"}});
            assert_eq!(
                catalog.tool_gate("file_read", &hidden, &state, &Value::Null),
                (true, false)
            );
            assert_eq!(
                catalog.tool_gate("missing", &hidden, &state, &Value::Null),
                (false, false)
            );
            let visible = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"visible"}});
            assert_eq!(
                catalog.tool_gate("file_read", &visible, &state, &Value::Null),
                (true, true)
            );
            // Discoverable by default: hidden until selected.
            let on = json!({"visibility":{"codebase_admin":"visible"}});
            assert_eq!(
                catalog.tool_gate("file_read", &on, &state, &Value::Null),
                (true, false)
            );
            assert_eq!(
                catalog.tool_gate("file_read", &on, &state, &json!(["tool:file_read"])),
                (true, true)
            );
            assert_eq!(
                catalog.tool_abilities("file_read", &visible, &state, &Value::Null),
                vec!["codebase_admin".to_owned()]
            );
            assert_eq!(
                catalog.tool_abilities("file_read", &hidden, &state, &Value::Null),
                vec!["codebase_admin".to_owned()]
            );
            let disabled_tool = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"off"}});
            assert_eq!(
                catalog.tool_gate("file_read", &disabled_tool, &state, &Value::Null),
                (true, false)
            );
            assert!(catalog.tool_off("file_read", &hidden));
            assert!(catalog.tool_off("file_read", &disabled_tool));
            assert!(!catalog.tool_off("file_read", &visible));
            assert!(!catalog.tool_off("file_read", &json!({})));
            assert!(!catalog.tool_off("missing", &hidden));
        }

        pub fn catalog_version_changes_with_skill<H: crate::TestHost>() {
            let (root, catalog) = fixture::<H>();
            fs::write(
                root.path()
                    .join("Administrator/codebase_admin/skills/codebase_admin.skill.md"),
                "New skill guidance",
            )
            .unwrap();
            assert_ne!(
                catalog.version(),
                Catalog::<H>::load(root.path()).unwrap().version()
            );
        }

        pub fn visible_skills_do_not_depend_on_tool_schema_exposure<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            let path = root.path().join("Administrator/codebase_admin/skills");
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join("ui_theme.skill.md"),"---\nid: ui_theme\ntitle: Theme\nwhen_to_use: Edit UI\ndefault_mode: visible\n---\nUI THEME").unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let p = json!({"rules":{"file_read":"allow","file_edit":"allow"}});
            let v = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"discoverable","file_edit":"discoverable"}});
            let state = json!({"active":[],"suppressed":[]});
            let guidance =
                catalog.visible_guidance_authorized(&p, &v, &state, |_| true, |_| true, |_| false);
            // A Visible selectable skill is always on; group guidance waits for a tool.
            assert!(!guidance.contains("Use scoped workspace tools."));
            assert!(guidance.contains("UI THEME"));
            assert!(!guidance.contains("Match one exact span."));
            let entries = catalog.catalog_with(&p, &v, &state);
            let schemas = catalog.schemas_with(&p, &json!([]), &v, &state);
            let report =
                catalog.context_inclusion(&entries, &schemas, &v, &state, &json!([]), &json!([]));
            let entry = &report[0];
            assert!(entry.schemas.is_empty());
            assert!(entry
                .skills
                .iter()
                .any(|s| s.kind == "ability" && !s.included));
            assert!(entry
                .skills
                .iter()
                .any(|s| s.kind == "selectable" && s.included));
            assert!(entry
                .skills
                .iter()
                .filter(|s| s.kind == "tool")
                .all(|s| !s.included));
        }

        pub fn visible_guidance_and_schema_respect_suppression<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let visibility = json!({"revision":1,"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_edit":"visible"}});
            let state = json!({"revision":0,"active":[],"suppressed":[]});
            let names = catalog
                .schemas_with(&policy, &json!([]), &visibility, &state)
                .into_iter()
                .map(|v| v["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert!(names.contains(&"file_edit".to_owned()));
            assert!(catalog
                .visible_guidance(&policy, &visibility, &state)
                .contains("Use scoped workspace tools."));
            let suppressed = json!({"revision":1,"active":[],"suppressed":["codebase_admin"]});
            let names = catalog
                .schemas_with(
                    &policy,
                    &json!(["codebase_admin"]),
                    &visibility,
                    &suppressed,
                )
                .into_iter()
                .map(|v| v["name"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>();
            assert!(!names.contains(&"file_edit".to_owned()));
            assert!(catalog
                .visible_guidance(&policy, &visibility, &suppressed)
                .is_empty());
            // A selected tool stays hidden while its ability is suppressed.
            assert!(!catalog.tool_available(
                "file_edit",
                &visibility,
                &suppressed,
                &json!(["tool:file_edit"])
            ));
        }

        pub fn visible_guidance_filters_denied_ability_and_tool_text<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let visibility = json!({"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"visible"}});
            let state = json!({"active":[],"suppressed":[]});
            let normal = catalog.visible_guidance(&policy, &visibility, &state);
            assert!(normal.contains("Use scoped workspace tools."));
            assert!(normal.contains("Match one exact span."));

            let denied_ability = catalog.visible_guidance_filtered(
                &policy,
                &visibility,
                &state,
                |_| false,
                |_| true,
            );
            assert!(denied_ability.is_empty());

            let denied_tool = catalog.visible_guidance_filtered(
                &policy,
                &visibility,
                &state,
                |_| true,
                |tool| tool != "file_edit",
            );
            assert!(denied_tool.contains("Use scoped workspace tools."));
            assert!(!denied_tool.contains("Match one exact span."));

            let all_tools_denied = catalog.visible_guidance_filtered(
                &policy,
                &visibility,
                &state,
                |_| true,
                |_| false,
            );
            assert!(all_tools_denied.is_empty());
        }

        pub fn persisted_active_discoverable_ability_exposes_schemas<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            // An explicitly activated ability (session state, /optimize) supplies its tools.
            let state = json!({"revision":1,"active":["codebase_admin"],"suppressed":[]});
            assert!(catalog
                .schemas_with(&policy, &json!([]), &Value::Null, &state)
                .iter()
                .any(|schema| schema["name"] == "file_edit"));
            let cleared = json!({"revision":2,"active":[],"suppressed":[]});
            // A retained conversation selection survives an activation-list reset.
            let selected = json!(["tool:file_edit"]);
            assert!(catalog.tool_available("file_edit", &Value::Null, &cleared, &selected));
            assert!(catalog
                .schemas_with(&policy, &selected, &Value::Null, &cleared)
                .iter()
                .any(|schema| schema["name"] == "file_edit"));
            // Unloading the selection removes its exposure. Explicit suppression also
            // wins while a selected identity remains in the conversation ledger.
            assert!(!catalog.tool_available("file_edit", &Value::Null, &cleared, &json!([])));
            assert!(!catalog
                .schemas_with(&policy, &json!([]), &Value::Null, &cleared)
                .iter()
                .any(|schema| schema["name"] == "file_edit"));
            let suppressed = json!({"revision":3,"active":[],"suppressed":["codebase_admin"]});
            assert!(!catalog.tool_available("file_edit", &Value::Null, &suppressed, &selected));
            assert!(!catalog
                .schemas_with(&policy, &selected, &Value::Null, &suppressed)
                .iter()
                .any(|schema| schema["name"] == "file_edit"));
            assert!(catalog.known("codebase_admin"));
            assert!(!catalog.known("unknown"));
        }

        pub fn agent_off_revokes_loaded_ability_but_keeps_configuration_row<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let visibility = json!({"revision":1,"visibility":{"codebase_admin":"off"}});
            let state = json!({"revision":1,"active":["codebase_admin"],"suppressed":[]});
            assert_eq!(
                catalog.configuration_catalog(&policy, &visibility)[0]["visibility"],
                "off"
            );
            assert!(catalog
                .catalog_with(&policy, &visibility, &state)
                .is_empty());
            assert!(!catalog.tool_available(
                "file_edit",
                &visibility,
                &state,
                &json!(["codebase_admin"])
            ));
            assert!(!catalog
                .schemas_with(&policy, &json!(["codebase_admin"]), &visibility, &state)
                .iter()
                .any(|schema| schema["name"] == "file_edit"));
            assert!(catalog
                .visible_guidance(&policy, &visibility, &state)
                .is_empty());
            let mut run = json!({"loaded_abilities":["codebase_admin"]});
            assert_eq!(
                catalog.discover_with(
                    "load_ability",
                    &json!({"ability_id":"codebase_admin"}),
                    &mut run,
                    &policy,
                    &visibility,
                    &state
                )["status"],
                "denied"
            );
        }

        pub fn bundled_skill_mode_is_independent_of_ability_mode<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let policy = policy::<H>();
            let state = json!({"active":[],"suppressed":[]});
            let mut visibility = json!({"visibility":{"codebase_admin":"visible"},
            "skill_visibility":{"codebase_admin":"off"}});
            assert!(!catalog
                .visible_guidance(&policy, &visibility, &state)
                .contains("Use scoped workspace tools."));
            visibility["skill_visibility"]["codebase_admin"] = json!("discoverable");
            assert!(!catalog
                .visible_guidance(&policy, &visibility, &state)
                .contains("Use scoped workspace tools."));
        }

        // SKILL-LIFETIME-REGRESSION: activation survives transcript folding but not unload.

        pub fn ability_activation_preserves_independent_skill_visibility<H: crate::TestHost>() {
            let (_root, catalog) = fixture::<H>();
            let p = policy::<H>();
            let v = json!({"visibility":{"codebase_admin":"discoverable"},"skill_visibility":{"codebase_admin":"discoverable"}});
            let active = json!({"active":["codebase_admin"],"suppressed":[]});
            assert!(!catalog
                .visible_guidance(&p, &v, &active)
                .contains("Use scoped workspace tools."));
            let visible = json!({"visibility":{"codebase_admin":"discoverable"},"skill_visibility":{"codebase_admin":"visible"}});
            assert!(catalog
                .visible_guidance(&p, &visible, &active)
                .contains("Use scoped workspace tools."));
            let suppressed = json!({"active":[],"suppressed":["codebase_admin"]});
            assert!(!catalog
                .visible_guidance(&p, &v, &suppressed)
                .contains("Use scoped workspace tools."));
        }

        pub fn typed_pending_entries_never_expose_tools_or_guidance<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            for (id, kind) in [
                ("core_entry", "virtual_core"),
                ("connection", "oauth"),
                ("chat_channel", "channel"),
                ("background", "service"),
                ("future", "placeholder"),
            ] {
                let path = root.path().join("Other").join(id);
                fs::create_dir_all(&path).unwrap();
                fs::write(
                    path.join(format!("{id}.json")),
                    serde_json::to_vec(&json!({
                        "display_name":id,"description":"Catalog<H> only","kind":kind,
                        "readiness":if kind == "placeholder" {"placeholder"} else {"pending"},
                        "tools":[], "config":{"settings":[]}
                    }))
                    .unwrap(),
                )
                .unwrap();
                fs::create_dir_all(path.join("skills")).unwrap();
                fs::write(
                    path.join(format!("skills/{id}.skill.md")),
                    "Unsafe v2 instructions",
                )
                .unwrap();
            }
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            assert_eq!(
                catalog
                    .configuration_catalog(&policy::<H>(), &Value::Null)
                    .len(),
                // The five typed entries and fixture Codebase Admin, plus built-in Base.
                7
            );
            for id in [
                "core_entry",
                "connection",
                "chat_channel",
                "background",
                "future",
            ] {
                let vis = json!({"visibility":{id:"visible"}});
                let mut run = json!({"loaded_abilities":[]});
                assert_eq!(
                    catalog.discover_with(
                        "load_ability",
                        &json!({"ability_id":id}),
                        &mut run,
                        &policy::<H>(),
                        &vis,
                        &Value::Null
                    )["status"],
                    "denied"
                );
                assert_eq!(run["loaded_abilities"], json!([]));
                assert!(!catalog
                    .visible_guidance(&policy::<H>(), &vis, &Value::Null)
                    .contains("Unsafe v2"));
            }
        }

        pub fn installed_tool_ownership_and_identifiers_fail_closed<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            let path = root.path().join("Other/duplicate");
            fs::create_dir_all(&path).unwrap();
            for tool in ["file_read", "../../escape"] {
                fs::write(
                    path.join("duplicate.json"),
                    serde_json::to_vec(&json!({
                        "display_name":"Duplicate","description":"Invalid owner","kind":"ability",
                        "tools":[tool]
                    }))
                    .unwrap(),
                )
                .unwrap();
                assert!(Catalog::<H>::load(root.path()).is_err());
            }
        }

        pub fn unsupported_allow_rule_does_not_expose_catalog_or_skill<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            let path = root.path().join("Other/legacy");
            fs::create_dir_all(path.join("tools")).unwrap();
            fs::write(path.join("legacy.json"), r#"{"display_name":"Legacy","description":"Pending","kind":"ability","tools":["v2_only"]}"#).unwrap();
            fs::write(path.join("legacy.skill.md"), "Unsafe guidance").unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let p = json!({"rules":{"v2_only":"allow"}});
            let v = json!({"visibility":{"legacy":"visible"}});
            assert!(catalog.catalog_with(&p, &v, &Value::Null).is_empty());
            assert!(catalog.visible_guidance(&p, &v, &Value::Null).is_empty());
        }

        pub fn malformed_and_oversized_assets_fail_closed<H: crate::TestHost>() {
            let (root, _) = fixture::<H>();
            let path = root
                .path()
                .join("Administrator/codebase_admin/codebase_admin.json");
            let original = fs::read(&path).unwrap();
            for bytes in [b"{".to_vec(), vec![b' '; MAX_MANIFEST_BYTES as usize + 1]] {
                fs::write(&path, bytes).unwrap();
                assert!(Catalog::<H>::load(root.path()).is_err());
            }
            fs::write(&path, original).unwrap();
            let skill = root
                .path()
                .join("Administrator/codebase_admin/tools/file_edit.skill.md");
            fs::write(&skill, vec![0xff]).unwrap();
            assert!(Catalog::<H>::load(root.path()).is_err());
        }

        #[cfg(unix)]

        pub fn symlink_assets_fail_closed<H: crate::TestHost>() {
            use std::os::unix::fs::symlink;
            let (root, _) = fixture::<H>();
            let skill = root
                .path()
                .join("Administrator/codebase_admin/tools/file_edit.skill.md");
            fs::remove_file(&skill).unwrap();
            symlink(root.path().join("missing"), &skill).unwrap();
            assert!(Catalog::<H>::load(root.path()).is_err());
        }

        pub fn current_catalog_withdrawal_preserves_strict_snapshot_validation<
            H: crate::TestHost,
        >() {
            use std::os::unix::fs::symlink;
            let (root, _) = fixture::<H>();
            let path = root
                .path()
                .join("Administrator/codebase_admin/codebase_admin.json");
            let original = fs::read(&path).unwrap();
            fs::remove_file(&path).unwrap();
            assert!(Catalog::<H>::load(root.path()).is_err());
            let current = Catalog::<H>::load_current(root.path()).unwrap();
            assert!(!current
                .abilities
                .iter()
                .any(|entry| entry.id == "codebase_admin"));
            fs::write(&path, &original).unwrap();
            assert!(Catalog::<H>::load_current(root.path())
                .unwrap()
                .abilities
                .iter()
                .any(|entry| entry.id == "codebase_admin"));
            fs::write(&path, b"invalid declaration").unwrap();
            assert!(Catalog::<H>::load_current(root.path()).is_err());
            fs::remove_file(&path).unwrap();
            symlink(root.path().join("missing"), &path).unwrap();
            assert!(Catalog::<H>::load_current(root.path()).is_err());
        }

        pub fn selected_tool_markers_preserve_off_and_suppressed_controls<H: crate::TestHost>() {
            let catalog = H::native_fixture();
            let loaded = json!(["tool:file_read"]);
            let mut visibility = json!({"visibility":{"codebase_admin":"discoverable"},
            "tool_modes":{"file_read":"discoverable"}});
            let state = json!({"active":[],"suppressed":[]});
            assert!(catalog.tool_available("file_read", &visibility, &state, &loaded));
            visibility["visibility"]["codebase_admin"] = json!("off");
            assert!(!catalog.tool_available("file_read", &visibility, &state, &loaded));
            visibility["visibility"]["codebase_admin"] = json!("discoverable");
            assert!(!catalog.tool_available(
                "file_read",
                &visibility,
                &json!({"active":[],"suppressed":["codebase_admin"]}),
                &loaded
            ));
            assert_eq!(state, json!({"active":[],"suppressed":[]}));
        }

        /// Verify native seed inventory against the tools actually registered by the current host.

        pub fn production_seed_manifest_loads<H: crate::TestHost>() {
            let catalog = H::native_fixture();
            let ordinary = catalog
                .abilities
                .iter()
                .filter(|entry| entry.id != "optimization")
                .collect::<Vec<_>>();
            assert!(!ordinary.is_empty());
            assert_eq!(
                catalog
                    .abilities
                    .iter()
                    .filter(|entry| entry.id == "mcp")
                    .count(),
                1
            );
            assert!(!catalog.known("mcp"));
            assert_eq!(
                catalog
                    .abilities
                    .iter()
                    .filter(|entry| entry.id == "optimization")
                    .count(),
                usize::from(H::optimization_declaration().unwrap().is_some())
            );
            assert!(!catalog.known("optimization"));
            assert_eq!(
                catalog
                    .abilities
                    .iter()
                    .filter(|entry| entry.readiness == Readiness::Partial)
                    .count(),
                0
            );
            for ability in catalog.abilities.iter().filter(|a| {
                a.group == "Core" && !matches!(a.id.as_str(), "p2p" | "automation" | "mcp")
            }) {
                for name in &ability.tools {
                    assert!(
                        Catalog::<H>::supported(name) || is_discovery(name),
                        "Unwired Core tool: {}::{name}",
                        ability.id
                    );
                }
            }
            let automation = catalog
                .abilities
                .iter()
                .find(|entry| entry.id == "automation")
                .unwrap();
            assert_eq!(automation.required_module.as_deref(), Some("automation"));
            assert!(!Catalog::<H>::executable(automation));
            assert!(catalog.abilities.iter().any(|entry| entry.id == "decision"
                && entry.tools == ["decide_with_jev", "score_directory_with_jev"]));
            let base = catalog
                .abilities
                .iter()
                .find(|entry| entry.id == "base")
                .unwrap();
            assert_eq!(base.tools, crate::base::TOOLS);
            let p2p = catalog.abilities.iter().find(|a| a.id == "p2p").unwrap();
            assert_eq!(p2p.readiness, Readiness::Pending);
            assert!(!Catalog::<H>::executable(p2p));
            for tool in &p2p.tools {
                assert!(!Catalog::<H>::supported(tool));
            }
            for name in &base.tools {
                assert!(
                    Catalog::<H>::supported(name) || is_discovery(name),
                    "Unwired Base tool: {name}"
                );
            }
            assert!(!catalog
                .abilities
                .iter()
                .any(|ability| matches!(ability.id.as_str(), "v3_build" | "v3_workspace")));
            let coding = catalog
                .abilities
                .iter()
                .find(|ability| ability.id == "codebase_admin")
                .unwrap();
            assert_eq!(
                coding.tools,
                [
                    "file_list",
                    "file_read",
                    "file_search",
                    "file_edit",
                    "file_write",
                    "build_work",
                    "state_inspect",
                    "prompt_slot_read",
                    "prompt_slot_write",
                    "shell",
                    "python",
                    "checkout_edit"
                ]
            );
            assert!(catalog
                .abilities
                .iter()
                .any(|ability| ability.id == "diagnostics"));
            assert!(["computer_control", "program_screenshot"]
                .iter()
                .all(|id| !catalog.known(id)));
            assert!(["terminal_control", "remote_tunnel"]
                .iter()
                .all(|id| catalog.known(id)));
        }

        pub fn web_access_loading_policy_and_visibility_gates<H: crate::TestHost>() {
            let catalog = H::native_fixture();
            let mut p = H::policy_fixture();
            let mut v = json!({"visibility":{"web_access":"discoverable"}});
            let state = json!({"active":[],"suppressed":[]});
            let loaded = json!(["tool:maps_geocode", "tool:get_weather"]);
            assert!(!catalog.tool_available("maps_geocode", &v, &state, &json!([])));
            assert!(catalog.tool_available("maps_geocode", &v, &state, &loaded));
            assert_eq!(
                catalog.tool_abilities("maps_geocode", &v, &state, &loaded),
                vec!["web_access"]
            );
            p["rules"]["get_weather"] = json!("deny");
            assert!(!catalog
                .schemas_with(&p, &loaded, &v, &state)
                .iter()
                .any(|s| s["name"] == "get_weather"));
            v["visibility"]["web_access"] = json!("off");
            assert!(!catalog.tool_available("maps_geocode", &v, &state, &loaded));
        }
    }

    pub mod optional_module_tests {
        use super::*;

        pub fn absent_optional_module_loads_without_exposing_execution<H: crate::TestHost>() {
            let root = tempfile::tempdir().unwrap();
            let folder = root.path().join("Core/automation");
            fs::create_dir_all(&folder).unwrap();
            fs::write(
                folder.join("automation.json"),
                json!({
                    "display_name":"Automation","kind":"ability","description":"Optional",
                    "readiness":"partial","required_module":"automation","tools":["schedule_task"]
                })
                .to_string(),
            )
            .unwrap();
            let catalog = Catalog::<H>::load(root.path()).unwrap();
            let entry = catalog
                .abilities
                .iter()
                .find(|a| a.id == "automation")
                .unwrap();
            assert!(!H::module_available("automation"));
            assert!(!Catalog::<H>::executable(entry));
            assert!(!catalog.tool_available(
                "schedule_task",
                &json!({"visibility":{"automation":"visible"}}),
                &json!({"active":["automation"]}),
                &Value::Null
            ));
        }
    }

    pub mod optional_settings_schema_tests {
        use super::tests::{fixture, policy};
        use super::*;
        pub fn catalog<H: crate::TestHost>() -> Catalog<H> {
            H::native_fixture()
        }
        pub fn defaults<H: crate::TestHost>(catalog: &Catalog<H>) -> Value {
            let ability = catalog
                .abilities
                .iter()
                .find(|ability| ability.required_module.as_deref() == Some("automation"))
                .unwrap();
            Value::Object(
                ability.config["settings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|field| {
                        (
                            field["key"].as_str().unwrap().to_owned(),
                            field["default"].clone(),
                        )
                    })
                    .collect(),
            )
        }

        pub fn optional_schema_enforces_full_declared_types_keys_and_numeric_bounds<
            H: crate::TestHost,
        >() {
            let catalog = catalog::<H>();
            let settings = defaults::<H>(&catalog);
            assert_eq!(
                catalog.validate_optional_settings("automation", &settings),
                Some(true)
            );
            let ability = catalog
                .abilities
                .iter()
                .find(|ability| ability.id == "automation")
                .unwrap();
            for field in ability.config["settings"].as_array().unwrap() {
                let key = field["key"].as_str().unwrap();
                let mut missing = settings.clone();
                missing.as_object_mut().unwrap().remove(key);
                assert_eq!(
                    catalog.validate_optional_settings("automation", &missing),
                    Some(false),
                    "missing {key}"
                );
                let mut malformed = settings.clone();
                malformed[key] = json!("incorrect-type");
                assert_eq!(
                    catalog.validate_optional_settings("automation", &malformed),
                    Some(false),
                    "type {key}"
                );
                if field["type"] == "number" {
                    for boundary in [
                        field["min"].as_i64().unwrap(),
                        field["max"].as_i64().unwrap(),
                    ] {
                        let mut valid = settings.clone();
                        valid[key] = json!(boundary);
                        assert_eq!(
                            catalog.validate_optional_settings("automation", &valid),
                            Some(true)
                        );
                    }
                    for outside in [
                        field["min"].as_i64().unwrap() - 1,
                        field["max"].as_i64().unwrap() + 1,
                    ] {
                        let mut invalid = settings.clone();
                        invalid[key] = json!(outside);
                        assert_eq!(
                            catalog.validate_optional_settings("automation", &invalid),
                            Some(false)
                        );
                    }
                    let mut fractional = settings.clone();
                    fractional[key] = json!(1.5);
                    assert_eq!(
                        catalog.validate_optional_settings("automation", &fractional),
                        Some(false)
                    );
                    let mut floating_integer = settings.clone();
                    floating_integer[key] = json!(2.0);
                    assert_eq!(
                        catalog.validate_optional_settings("automation", &floating_integer),
                        Some(false)
                    );
                } else {
                    for boolean in [false, true] {
                        let mut valid = settings.clone();
                        valid[key] = json!(boolean);
                        assert_eq!(
                            catalog.validate_optional_settings("automation", &valid),
                            Some(true)
                        );
                    }
                }
            }
            let mut unknown = settings.clone();
            unknown["unreviewed_setting"] = json!(true);
            assert_eq!(
                catalog.validate_optional_settings("automation", &unknown),
                Some(false)
            );
            assert_eq!(
                catalog.validate_optional_settings("automation", &Value::Null),
                Some(false)
            );
            assert_eq!(
                catalog.validate_optional_settings("automation", &json!([])),
                Some(false)
            );
            assert_eq!(
                catalog.validate_optional_settings("unknown", &settings),
                None
            );
            assert_eq!(
                catalog.validate_optional_settings("codebase_admin", &settings),
                Some(false)
            );
        }

        pub fn malformed_optional_declarations_fail_closed<H: crate::TestHost>() {
            for mutation in [
                "duplicate",
                "unknown-type",
                "invalid-default",
                "invalid-bound",
                "reversed-bounds",
                "missing-default",
            ] {
                let mut catalog = catalog::<H>();
                let settings = defaults::<H>(&catalog);
                let ability = catalog
                    .abilities
                    .iter_mut()
                    .find(|ability| ability.id == "automation")
                    .unwrap();
                let fields = ability.config["settings"].as_array_mut().unwrap();
                match mutation {
                    "duplicate" => fields[1] = fields[0].clone(),
                    "unknown-type" => fields[0]["type"] = json!("unsupported"),
                    "invalid-default" => fields[0]["default"] = json!(0),
                    "invalid-bound" => fields[2]["min"] = json!("zero"),
                    "reversed-bounds" => fields[2]["min"] = json!(201),
                    "missing-default" => {
                        fields[0].as_object_mut().unwrap().remove("default");
                    }
                    _ => unreachable!(),
                }
                assert_eq!(
                    catalog.validate_optional_settings("automation", &settings),
                    Some(false),
                    "{mutation}"
                );
            }
        }

        /// Adopt selected current text while preserving source, mode, permission and schema boundaries.
        pub fn capability_guidance_reads_selected_live_sources_and_withdraws_invalid_or_denied_assets<
            H: crate::TestHost,
        >() {
            let (root, _) = fixture::<H>();
            let folder = root.path().join("Administrator/codebase_admin");
            fs::create_dir_all(folder.join("skills")).unwrap();
            let skill = |body: &str| {
                format!("---\nid: verification\ntitle: Verification\nwhen_to_use: Review implementation\ndefault_mode: discoverable\n---\n{body}")
            };
            let path = folder.join("skills/verification.skill.md");
            fs::write(&path, skill("FIRST BODY")).unwrap();
            let catalog = Catalog::<H>::load_snapshot(root.path()).unwrap();
            let visible = json!({"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"visible"},
            "settings":{"codebase_admin":{"terminal_caution":"cautious"}}});
            let state = json!({"active":[],"suppressed":[]});
            let entries = catalog.catalog_with(&policy::<H>(), &visible, &state);
            let schemas = vec![json!({"name":"load_skill"}), json!({"name":"file_edit"})];
            let selected = json!([{"ability_id":"codebase_admin","kind":"selectable","skill_id":"verification","sha256":"obsolete"}]);
            let read = || {
                catalog.capability_guidance(
                    &entries,
                    &schemas,
                    &visible,
                    &state,
                    &Value::Null,
                    &selected,
                )
            };
            let first = read();
            assert!(first.iter().any(|item| item.id == "ability:codebase_admin"
                && item.natural
                && item.body == "Use scoped workspace tools."));
            assert!(first
                .iter()
                .any(|item| item.id == "ability:codebase_admin:settings"
                    && item.natural
                    && item.body.contains("Configured terminal_caution: cautious")));
            let mut hidden_core = visible.clone();
            hidden_core["skill_visibility"] = json!({"codebase_admin":"discoverable"});
            let hidden = catalog.capability_guidance(
                &entries,
                &schemas,
                &hidden_core,
                &state,
                &Value::Null,
                &selected,
            );
            assert!(hidden.iter().any(|item| item.id == "ability:codebase_admin"
                && !item.natural
                && !item.selected));
            assert!(hidden
                .iter()
                .any(|item| item.id == "ability:codebase_admin:settings" && item.natural));
            hidden_core["skill_visibility"]["codebase_admin"] = json!("off");
            let off_core = catalog.capability_guidance(
                &entries,
                &schemas,
                &hidden_core,
                &state,
                &Value::Null,
                &selected,
            );
            assert!(!off_core
                .iter()
                .any(|item| item.id == "ability:codebase_admin"));
            assert!(off_core
                .iter()
                .any(|item| item.id == "ability:codebase_admin:settings" && item.natural));
            assert!(first
                .iter()
                .any(|item| item.id == "tool:codebase_admin:file_edit" && item.natural));
            assert!(first
                .iter()
                .any(|item| item.id == "selectable:codebase_admin:verification"
                    && item.selected
                    && !item.natural
                    && item.body == skill("FIRST BODY")));
            let version = catalog.live_version();
            fs::write(&path, skill("CURRENT BODY")).unwrap();
            assert_ne!(catalog.live_version(), version);
            let current = read();
            assert!(current
                .iter()
                .any(|item| item.selected && item.body == skill("CURRENT BODY")));
            assert!(!current.iter().any(|item| item.body.contains("FIRST BODY")));
            fs::write(&path, "Missing required selectable metadata").unwrap();
            assert!(!read().iter().any(|item| item.id.starts_with("selectable:")));
            fs::remove_file(&path).unwrap();
            assert!(!read().iter().any(|item| item.id.starts_with("selectable:")));
            fs::write(&path, skill("REAPPEARED BODY")).unwrap();
            assert!(read()
                .iter()
                .any(|item| item.selected && item.body == skill("REAPPEARED BODY")));
            let mut off = visible.clone();
            off["skill_visibility"] = json!({"codebase_admin:verification":"off"});
            assert!(!catalog
                .capability_guidance(&entries, &schemas, &off, &state, &Value::Null, &selected)
                .iter()
                .any(|item| item.id.starts_with("selectable:")));
            assert!(catalog
                .capability_guidance(
                    &entries,
                    &schemas,
                    &visible,
                    &json!({"suppressed":["codebase_admin"]}),
                    &Value::Null,
                    &selected
                )
                .is_empty());
            assert!(catalog
                .capability_guidance(&[], &schemas, &visible, &state, &Value::Null, &selected)
                .is_empty());
            let without_load = catalog.capability_guidance(
                &entries,
                &[json!({"name":"file_edit"})],
                &visible,
                &state,
                &Value::Null,
                &selected,
            );
            assert!(without_load
                .iter()
                .filter(|item| item.id.starts_with("selectable:"))
                .all(|item| !item.selected && !item.natural));
            let no_tools = catalog.capability_guidance(
                &entries,
                &[],
                &visible,
                &state,
                &Value::Null,
                &selected,
            );
            // Group guidance waits for one of its tools.
            assert!(no_tools
                .iter()
                .any(|item| item.id == "ability:codebase_admin" && !item.natural));
            assert!(no_tools
                .iter()
                .filter(|item| item.id.starts_with("tool:"))
                .all(|item| !item.natural));
        }

        /// Prevent retained identities from reviving denied or Off tool guidance.
        pub fn capability_guidance_filters_current_authorized_tool_names_and_modes<
            H: crate::TestHost,
        >() {
            let (_root, catalog) = fixture::<H>();
            let visible = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_edit":"discoverable"}});
            let state = json!({});
            let mut entries = catalog.catalog_with(&policy::<H>(), &visible, &state);
            let schemas = vec![json!({"name":"file_edit"})];
            let selected = json!(["tool:file_edit"]);
            let body = catalog.capability_guidance(
                &entries,
                &schemas,
                &visible,
                &state,
                &selected,
                &Value::Null,
            );
            assert!(body
                .iter()
                .any(|item| item.id == "tool:codebase_admin:file_edit" && item.natural));
            entries[0]["tools"] = json!([{"name":"file_read"}]);
            assert!(!catalog
                .capability_guidance(
                    &entries,
                    &schemas,
                    &visible,
                    &state,
                    &selected,
                    &Value::Null
                )
                .iter()
                .any(|item| item.id == "tool:codebase_admin:file_edit"));
            let mut off = visible;
            off["tool_modes"]["file_edit"] = json!("off");
            let entries = catalog.catalog_with(&policy::<H>(), &off, &state);
            assert!(!catalog
                .capability_guidance(&entries, &schemas, &off, &state, &selected, &Value::Null)
                .iter()
                .any(|item| item.id == "tool:codebase_admin:file_edit"));
        }

        /// Project identities without changing saved state or overriding restrictions.
        pub fn selected_state_is_call_local_and_preserves_off_and_suppression<
            H: crate::TestHost,
        >() {
            let (_root, catalog) = fixture::<H>();
            let visibility = json!({"visibility":{"codebase_admin":"discoverable"}});
            let state = json!({"active":["another"],"suppressed":[],"revision":7});
            let original = state.clone();
            let loaded = json!([
                "codebase_admin",
                "codebase_admin",
                "unknown",
                "tool:file_edit"
            ]);
            let projected = catalog.selected_state(&visibility, &state, &loaded);
            assert_eq!(state, original);
            assert_eq!(projected["active"], json!(["another", "codebase_admin"]));
            assert_eq!(projected["revision"], 7);
            assert_eq!(
                catalog.tool_gate("file_edit", &visibility, &state, &loaded),
                (true, true)
            );
            // An activated ability supplies its discoverable tools, never its Off ones.
            let tool_off = json!({"visibility":{"codebase_admin":"discoverable"},"tool_modes":{"file_edit":"off"}});
            assert_eq!(
                catalog.tool_gate("file_edit", &tool_off, &state, &json!(["codebase_admin"])),
                (true, false)
            );
            assert_eq!(
                catalog.selected_state(&visibility, &projected, &loaded),
                projected
            );
            let off = json!({"visibility":{"codebase_admin":"off"}});
            assert_eq!(catalog.selected_state(&off, &state, &loaded), state);
            assert_eq!(
                catalog.tool_gate("file_edit", &off, &state, &loaded),
                (true, false)
            );
            let suppressed = json!({"active":[],"suppressed":["codebase_admin"]});
            assert_eq!(
                catalog.selected_state(&visibility, &suppressed, &loaded),
                suppressed
            );
            assert_eq!(
                catalog.tool_gate("file_edit", &visibility, &suppressed, &loaded),
                (true, false)
            );
            assert_eq!(
                catalog.selected_state(&visibility, &state, &json!(["tool:file_edit"])),
                state
            );
            assert_eq!(
                catalog.selected_state(&visibility, &state, &json!(["unknown"])),
                state
            );
        }
    }
}

#[cfg(test)]
mod discovery_row_tests {
    use super::discovery_row;
    use serde_json::json;

    #[test]
    fn discovery_row_keeps_choice_fields_and_drops_bulk() {
        let row = discovery_row(&json!({
            "id": "codebase_admin", "name": "Codebase Admin", "description": "Edit source",
            "group_name": "Administrator", "availability": "discoverable",
            "requires_activation": true, "selectable_skills": ["rust_quality"],
            "tools": [{"name": "file_read", "policy": "allow", "mode": "visible"}],
            "config": {"bias": {}}, "general_skills": ["long text"],
        }));
        assert_eq!(row["tools"], json!(["file_read"]));
        assert_eq!(row["group"], "Administrator");
        assert!(row.get("config").is_none() && row.get("general_skills").is_none());
    }
}
