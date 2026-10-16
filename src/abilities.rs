use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    borrow::Cow,
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
const MAX_ABILITIES: usize = 128;

/// Content-free evidence generated from the same authorized model-call assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AbilityContext {
    pub turn: u64,
    pub call: u64,
    pub abilities: Vec<AbilityInclusion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AbilityInclusion {
    pub id: String,
    pub visibility: String,
    pub availability: String,
    pub guidance_bytes: usize,
    pub schemas: Vec<String>,
    pub skills: Vec<SkillInclusion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SkillInclusion {
    pub kind: String,
    pub id: String,
    pub mode: String,
    pub included: bool,
    pub content_bytes: usize,
    pub reason: String,
}
impl AbilityContext {
    pub(crate) fn public(value: &Value) -> Option<Self> {
        let report: Self = serde_json::from_value(value.clone()).ok()?;
        if report.abilities.len() > MAX_ABILITIES || report.abilities.iter().any(|a|
            !valid_id(&a.id) || !matches!(a.visibility.as_str(), "visible" | "discoverable")
            || !matches!(a.availability.as_str(), "visible" | "activated" | "discoverable")
            || a.schemas.len() > 512 || a.schemas.iter().any(|id| !valid_id(id))
            || a.skills.len() > 512 || a.skills.iter().any(|s| !valid_id(&s.id)
                || !matches!(s.kind.as_str(), "ability" | "tool" | "selectable" | "own")
                || !matches!(s.mode.as_str(), "visible" | "discoverable" | "off")
                || !matches!(s.reason.as_str(), "included" | "off" | "discoverable" | "unavailable"))) {
            return None;
        }
        Some(report)
    }
}


#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum EntryKind { Ability, VirtualCore, Oauth, Channel, Service, Placeholder }

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Readiness { Ready, Partial, Pending, Placeholder }

#[derive(Clone, Debug, Serialize)]
struct Ability {
    id: String,
    #[serde(skip_serializing)]
    path: PathBuf,
    manifest_digest: String,
    plugin: Option<crate::ability_plugins::Package>,
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
pub struct Catalog {
    root:PathBuf,
    published:bool,
    native_overrides:BTreeMap<String,(String,String)>,
    abilities: Vec<Ability>,
    version: String,
}

fn optional_setting_value_valid(declaration: &Value, value: &Value) -> bool {
    match declaration["type"].as_str() {
        Some("boolean") => value.is_boolean(),
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

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

pub(crate) fn search_words(text: &str) -> Vec<String> {
    text.to_lowercase().split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty()).map(str::to_owned).collect()
}

pub(crate) fn near_word(query: &str, word: &str) -> bool {
    if query == word || query.len() >= 4 && word.starts_with(query) { return true; }
    let aliases: &[&str] = match query {
        "code" | "coding" => &["source", "repository"],
        "edit" | "editing" | "change" | "remove" | "delete" | "fix" => &["write", "patch"],
        "inspect" | "browse" => &["read", "review", "search"],
        "compile" | "build" => &["cargo", "worker"],
        "skill" | "skills" => &["guidance"],
        "theme" => &["theming", "appearance", "palette"],
        _ => &[],
    };
    if aliases.contains(&word) { return true; }
    if !query.is_ascii() || !word.is_ascii() { return false; }
    let (left, right) = (query.as_bytes(), word.as_bytes());
    if left.len() >= 4 && left.len() == right.len() {
        for index in 0..left.len() - 1 {
            if left[index] == right[index + 1] && left[index + 1] == right[index]
                && left[..index] == right[..index] && left[index + 2..] == right[index + 2..] {
                return true;
            }
        }
    }
    if query.len() < 5 || query.len().abs_diff(word.len()) > 1 { return false; }
    let (mut i, mut j, mut edits) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        if left[i] == right[j] { i += 1; j += 1; continue; }
        edits += 1;
        if edits > 1 { return false; }
        if left.len() >= right.len() { i += 1; }
        if right.len() >= left.len() { j += 1; }
    }
    edits + usize::from(i < left.len() || j < right.len()) == 1
}

fn discovery_score(ability: &Value, query: &str) -> usize {
    let fields = [("id", 5), ("name", 5), ("group_name", 2),
        ("description", 2), ("skill_summary", 2)];
    search_words(query).into_iter()
        .filter(|term| !matches!(term.as_str(),
            "a" | "an" | "and" | "for" | "in" | "my" | "of" | "on" |
            "or" | "please" | "the" | "this" | "to" | "with"))
        .map(|term| {
            let field_score = fields.iter().filter(|(field, _)| ability[*field].as_str()
                .is_some_and(|text| search_words(text).iter().any(|word| near_word(&term, word))))
                .map(|(_, weight)| *weight).max().unwrap_or(0);
            let tool_score = ability["tools"].as_array().is_some_and(|tools| tools.iter()
                .filter_map(|tool| tool["name"].as_str())
                .any(|name| search_words(name).iter().any(|word| near_word(&term, word))));
            field_score.max(usize::from(tool_score) * 3)
        }).sum()
}

fn read_bytes(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.file_type().is_file() || meta.len() > limit {
        bail!("Invalid ability file: {}", path.display());
    }
    // A concurrent growth after metadata inspection cannot create an unbounded read.
    use std::io::Read;
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit + 1).read_to_end(&mut bytes)
        .with_context(|| format!("Cannot read ability file: {}", path.display()))?;
    if bytes.len() as u64 > limit { bail!("Ability file grew beyond limit"); }
    Ok(bytes)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let data = read_bytes(path, MAX_MANIFEST_BYTES)?;
    serde_json::from_slice(&data)
        .with_context(|| format!("Invalid ability manifest: {}", path.display()))
}

fn optional_skill(path: &Path) -> Result<Option<String>> {
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

impl Catalog {
    /// Freeze a completely validated catalog for one immutable publication revision.
    pub(crate) fn load_snapshot(root:&Path)->Result<Self> {
        let mut catalog=Self::load(root)?;
        catalog.published=true;
        for ability in &mut catalog.abilities {ability.frozen=true;}
        Ok(catalog)
    }
    /// Bind only a currently executable isolated owner; pending declarations cannot dispatch.
    pub(crate) fn plugin_binding(&self,name:&str)->Option<Value> {
        self.abilities.iter().filter_map(|a|a.plugin.as_ref().filter(|_|Self::executable(a))).find_map(|p|p.binding(name))
    }
    /// Combine compiled contracts with present isolated schemas without probing unrelated modules.
    pub(crate) fn all_schemas(&self)->Vec<Value> {
        let mut schemas=crate::tools::native_schemas();
        for schema in &mut schemas {
            if let Some((original,description))=schema["name"].as_str().and_then(|name|self.native_overrides.get(name)){
                if let Some(text)=schema["description"].as_str(){schema["description"]=json!(text.replace(original,description));}
            }
        }
        schemas.extend(self.abilities.iter().filter_map(|a|a.plugin.as_ref().filter(|_|Self::present(a))).flat_map(|p|p.schemas.clone()));
        schemas
    }

    /// Validate manifest ownership and guidance before admitting any catalog revision.
    pub fn load(root: &Path) -> Result<Self> { Self::load_mode(root,false) }

    /// Local live sources may explicitly withdraw a declaration by unlinking its manifest.
    /// Present invalid manifests and all immutable publication snapshots stay fail-closed.
    pub(crate) fn load_current(root:&Path)->Result<Self> { Self::load_mode(root,true) }

    fn load_mode(root:&Path,withdraw_missing:bool)->Result<Self> {
        let native_overrides=crate::live_assets::native_schema_overrides(root)?;
        let mut abilities = Vec::new();
        let mut ids = HashSet::new();
        let mut owners = BTreeMap::new();
        if root.exists() && fs::symlink_metadata(root)?.file_type().is_symlink() { bail!("Symlink catalog root"); }
        let groups = match fs::read_dir(root) {
            Ok(groups) => groups,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let version = hex::encode(Sha256::digest(b"[]"));
                return Ok(Self { abilities, version,root:root.to_owned(),published:false,native_overrides });
            }
            Err(error) => return Err(error).with_context(|| format!("Cannot read ability catalog: {}", root.display())),
        };
        for group in groups {
            let group = group?;
            if group.file_type()?.is_symlink() { bail!("Symlink ability group"); }
            if !group.file_type()?.is_dir() {
                continue;
            }
            let group_id = group.file_name().to_string_lossy().to_string();
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
            let group_meta: GroupManifest = if group_meta.exists() {
                read_json(&group_meta)?
            } else {
                GroupManifest::default()
            };
            let group_name = group_meta.name.unwrap_or_else(|| group_id.clone());
            if group_name.is_empty() || group_name.len() > 100 {
                bail!("Invalid ability group name");
            }
            for folder in fs::read_dir(&group_path)? {
                let folder = folder?;
                if folder.file_type()?.is_symlink() { bail!("Symlink ability folder"); }
                if !folder.file_type()?.is_dir() {
                    continue;
                }
                let id = folder.file_name().to_string_lossy().to_string();
                if !valid_id(&id) || !ids.insert(id.clone()) {
                    bail!("Invalid or duplicate ability identifier: {id}");
                }
                if matches!(id.as_str(),"optimization"|"mcp") { bail!("Ability ID is reserved for its executable plugin"); }
                let ability_path = folder.path();
                // Pin exactly the bytes parsed here; live edits withdraw cached authority
                // until a freshly validated catalog reload adopts the new declaration.
                // Retired coding abilities keep their backend files for operator
                // commands and durable recovery, but are no longer catalog entries.
                if group_id == "Administrator" && matches!(id.as_str(), "v3_build"|"v3_workspace")
                    && !ability_path.join(format!("{id}.json")).exists() { continue; }
                let manifest_path=ability_path.join(format!("{id}.json"));
                if withdraw_missing {
                    match fs::symlink_metadata(&manifest_path) {
                        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>continue,
                        Err(error)=>return Err(error.into()),
                        Ok(_)=>{},
                    }
                }
                let manifest_bytes = read_bytes(&manifest_path, MAX_MANIFEST_BYTES)?;
                let manifest_digest = hex::encode(Sha256::digest(&manifest_bytes));
                let manifest: Manifest = serde_json::from_slice(&manifest_bytes)
                    .with_context(|| format!("Invalid ability manifest: {id}"))?;
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
                let plugin=crate::ability_plugins::Package::load(&ability_path,&id,&manifest.tools)?;
                if plugin.is_some() && manifest.kind!=EntryKind::Ability {bail!("Isolated packages cannot declare trusted core or service kinds");}
                let installed=|name:&str|plugin.as_ref().is_some_and(|p|p.owns(name)) || Self::supported(name);
                let readiness = manifest.readiness.clone().unwrap_or_else(|| {
                    if manifest.placeholder || manifest.kind == EntryKind::Placeholder { Readiness::Placeholder }
                    else if manifest.tools.iter().any(|tool| installed(tool)) { Readiness::Ready }
                    else { Readiness::Pending }
                });
                if (manifest.placeholder || manifest.kind == EntryKind::Placeholder)
                    && (readiness != Readiness::Placeholder || !manifest.tools.is_empty()) {
                    bail!("Invalid placeholder: {id}");
                }
                if manifest.required_module.as_deref().is_some_and(|module|
                    !valid_id(module) || crate::modules::package_root(module).is_none()) {
                    bail!("Unknown optional module requirement: {id}");
                }
                // An omitted optional service must not prevent ordinary app startup.
                // Execution still requires its live healthy package and schemas.
                let omitted_module=manifest.required_module.as_deref()
                    .is_some_and(|module|!crate::modules::available(module));
                if !omitted_module && matches!(readiness, Readiness::Ready | Readiness::Partial) &&
                    (!matches!(manifest.kind, EntryKind::Ability | EntryKind::VirtualCore) ||
                     !manifest.tools.iter().any(|tool| installed(tool))) {
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
                    if !valid_id(tool) { bail!("Invalid tool identifier: {tool}"); }
                    // Installed schemas have exactly one authority-owning entry.
                    if owners.insert(tool.clone(), id.clone()).is_some() {
                        bail!("Duplicate installed tool owner: {tool}");
                    }
                    if !local.insert(tool) {
                        bail!("Ability {id} declares a duplicate tool: {tool}");
                    }
                    if let Some(skill) = optional_skill(
                        &ability_path.join("tools").join(format!("{tool}.skill.md")),
                    )? {
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
                        if !valid_id(key) || selectable_skills.len() >= 32 {
                            bail!("Invalid selectable skill identifier: {id}");
                        }
                        let text = optional_skill(&asset.path())?
                            .context("Missing selectable skill")?;
                        selectable_skills.insert(key.to_owned(), text);
                    }
                }
                if !manifest.config.is_null() && !manifest.config.is_object() {
                    bail!("Invalid configuration metadata: {id}");
                }
                let skill = optional_skill(
                    &ability_path.join(format!("{id}.skill.md")),
                )?;
                abilities.push(Ability {
                    id,
                    path: ability_path,
                    manifest_digest,
                    plugin, frozen:false,
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
        if !ids.contains("optimization") {
            let bytes = include_bytes!("plugin_packages/optimization/optimization.json");
            let manifest: Manifest = serde_json::from_slice(bytes)?;
            abilities.push(Ability {
                id: "optimization".into(), path: crate::optimization::folder(),
                manifest_digest: hex::encode(Sha256::digest(bytes)),
                plugin:None, frozen:false,
                group: "Administrator".into(), group_name: "Administrator".into(),
                group_order: 100, name: manifest.display_name,
                description: manifest.description, skill_summary: manifest.skill_summary,
                required_module: manifest.required_module,
                migration_note: manifest.migration_note, icon: manifest.icon, color: manifest.color,
                order: manifest.order, kind: manifest.kind, readiness: Readiness::Ready,
                config: manifest.config, tools: manifest.tools,
                skill: Some(include_str!("plugin_packages/optimization/optimization.skill.md").into()),
                tool_skills: BTreeMap::new(), selectable_skills: BTreeMap::new(),
            });
        }
        abilities.push(Ability {
            id:"mcp".into(),path:crate::mcp::bridge::folder(),manifest_digest:"mcp-v1".into(),plugin:None,frozen:false,
            group:"Connections".into(),group_name:"Connections".into(),group_order:90,
            name:"MCP".into(),description:"Connect this agent to external MCP servers and explicitly permitted operations.".into(),
            skill_summary:Some("Discover MCP servers, manage agent connections and call permitted remote tools.".into()),
            migration_note:None,required_module:None,icon:Some("plug".into()),color:None,order:1,
            kind:EntryKind::Ability,readiness:Readiness::Ready,config:json!({}),
            tools:crate::mcp::all_schemas().iter().filter_map(|s|s["name"].as_str().map(str::to_owned)).collect(),
            skill:Some(crate::mcp::GUIDANCE.into()),tool_skills:BTreeMap::new(),selectable_skills:BTreeMap::new(),
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
        let version = hex::encode(Sha256::digest(serde_json::to_vec(&json!([abilities,native_overrides]))?));
        Ok(Self { abilities, version,root:root.to_owned(),published:false,native_overrides })
    }

    pub(crate) fn empty()->Self {Self{root:PathBuf::new(),published:false,native_overrides:BTreeMap::new(),abilities:Vec::new(),version:hex::encode(Sha256::digest(b"[]"))}}
    pub(crate) fn snapshot_root(&self)->&Path {&self.root}
    pub(crate) fn published(&self)->bool {self.published}
    pub(crate) fn source_root(&self)->Option<&Path> {
        self.root.ends_with("plugins/abilities").then(||self.root.parent().and_then(Path::parent)).flatten()
    }
    pub fn version(&self) -> &str {
        &self.version
    }

    fn present(ability: &Ability) -> bool {
        if ability.id=="codebase_admin" && !ability.frozen {
            if let Some(path)=Self::published_skill(ability,"ability","codebase_admin") {
                let Some(folder)=path.parent() else {return false;};
                let metadata=read_bytes(&folder.join("plugin.json"),4096).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
                let declaration=read_bytes(&folder.join("codebase_admin.json"),MAX_MANIFEST_BYTES).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
                return metadata.is_some_and(|v|v["id"]=="codebase_admin" && v["protocol"]==1 && v["interface"]==1 && v["entrypoint"]=="runner.py" && v["runtime"]=="python3" && v["version"]==1 && v["tools"]==json!(ability.tools))
                    && declaration.is_some_and(|v|v["tools"]==json!(ability.tools) && v["kind"]=="ability" && v["readiness"]=="ready")
                    && read_bytes(&folder.join("runner.py"),2*1024*1024).is_ok();
            }
        }
        if ability.id=="mcp" {return crate::mcp::bridge::available();}
        if ability.id == "optimization" {
            return crate::optimization::available();
        }
        // Presence alone cannot retain an old executable declaration after an
        // operator disables or edits its manifest. Unreadable/changed bytes fail closed.
        [ability.path.as_path(), ability.path.parent().unwrap_or(ability.path.as_path()),
            ability.path.parent().and_then(Path::parent).unwrap_or(ability.path.as_path())]
            .into_iter().all(|path|fs::symlink_metadata(path).is_ok_and(|m|m.file_type().is_dir()))
            && read_bytes(&ability.path.join(format!("{}.json", ability.id)), MAX_MANIFEST_BYTES)
                .is_ok_and(|bytes| hex::encode(Sha256::digest(&bytes)) == ability.manifest_digest)
    }

    // Only registered, validated bundle IDs can select an asset. Each model call
    // reads their current bodies from the pinned catalog source or published pointer.
    /// Published guidance follows the broker pointer; compatible Codebase Admin metadata is live.
    /// Tool/schema grants remain bound to the installed core interface.
    fn published_skill_path(ability:&Ability,kind:&str,name:&str,ui_root:Option<&Path>)->Option<PathBuf>{
        if ability.frozen || !matches!(ability.id.as_str(),"code_index"|"codebase_admin") || !valid_id(name){return None;}
        let root=ui_root.filter(|root|root.ends_with(Path::new("v3/ui")))?;
        let folder=root.parent()?.join(format!("plugins/abilities/Administrator/{}",ability.id));
        match kind{
            "ability" if name==ability.id=>Some(folder.join(format!("{}.skill.md",ability.id))),
            "tool" if ability.tools.iter().any(|tool|tool==name)=>Some(folder.join("tools").join(format!("{name}.skill.md"))),
            "selectable" if ability.selectable_skills.contains_key(name)=>Some(folder.join("skills").join(format!("{name}.skill.md"))),
            _=>None,
        }
    }
    /// Resolve supported independently published guidance through the trusted asset pointer.
    fn published_skill(ability:&Ability,kind:&str,name:&str)->Option<PathBuf>{
        let root=std::env::var_os("V3_UI_ROOT").map(PathBuf::from);
        Self::published_skill_path(ability,kind,name,root.as_deref())
    }
    /// Read current text only for a registered asset. Frozen catalogs retain their
    /// revision-owned paths instead of following the mutable publication pointer.
    fn asset_text<'a>(ability:&'a Ability,kind:&str,name:&str,cached:&'a str)->Option<Cow<'a,str>> {
        if ability.id=="mcp" {
            return (crate::mcp::bridge::available() && kind=="ability" && name=="mcp"
                && cached==crate::mcp::GUIDANCE).then_some(Cow::Borrowed(cached));
        }
        if ability.id=="optimization" {
            return (kind=="ability" && name=="optimization")
                .then(crate::optimization::guidance).flatten().map(Cow::Owned);
        }
        let declared=match kind {
            "ability"=>name==ability.id && ability.skill.is_some(),
            "tool"=>ability.tools.iter().any(|tool|tool==name) && ability.tool_skills.contains_key(name),
            "selectable"=>ability.selectable_skills.contains_key(name),
            _=>false,
        };
        if !declared || !valid_id(name) {return None;}
        let path=Self::published_skill(ability,kind,name).unwrap_or_else(|| match kind {
            "ability"=>ability.path.join(format!("{name}.skill.md")),
            "tool"=>ability.path.join("tools").join(format!("{name}.skill.md")),
            _=>ability.path.join("skills").join(format!("{name}.skill.md")),
        });
        if !path.parent().is_some_and(|parent|
            fs::symlink_metadata(parent).is_ok_and(|m|m.file_type().is_dir())) {return None;}
        optional_skill(&path).ok().flatten().map(Cow::Owned)
    }

    /// Reject a body that changed between the source read and its use.
    fn skill_live(ability:&Ability,kind:&str,name:&str,text:&str)->bool {
        Self::asset_text(ability,kind,name,text).is_some_and(|current|current==text)
    }

    /// Resolve current core guidance from its registered source.
    fn core_skill(ability:&Ability)->Option<Cow<'_,str>> {
        if ability.id=="optimization" {return crate::optimization::guidance().map(Cow::Owned);}
        ability.skill.as_deref().and_then(|text|Self::asset_text(ability,"ability",&ability.id,text))
    }

    /// Fingerprint exact current bodies, including withdrawal and reappearance.
    /// Catalog declarations and executable grants remain independently validated.
    pub fn live_version(&self)->String {
        let validity=self.abilities.iter().map(|ability| {
            let present=Self::present(ability);
            let mut assets=Vec::new();
            let mut add=|kind:&str,name:&str,body:Option<Cow<'_,str>>| {
                assets.push(json!([kind,name,body.filter(|_|present)
                    .map(|text|hex::encode(Sha256::digest(text.as_bytes())))]));
            };
            add("ability",&ability.id,Self::core_skill(ability));
            for (name,text) in &ability.tool_skills {
                add("tool",name,Self::asset_text(ability,"tool",name,text));
            }
            for (name,text) in &ability.selectable_skills {
                add("selectable",name,Self::asset_text(ability,"selectable",name,text));
            }
            json!([ability.id,present,assets])
        }).collect::<Vec<_>>();
        hex::encode(Sha256::digest(serde_json::to_vec(&json!([self.version,validity,crate::optimization::generation()]))
            .expect("Current catalog validity is serializable")))
    }

    /// Read current registered bodies after executable, installed-tool and permission gates.
    fn bundled_candidates<'a>(
        &'a self, ability_id: &str, visibility: &Value, state: &Value,
        allow_ability: &impl Fn(&str) -> bool, allow_tool: &impl Fn(&str) -> bool,
    ) -> Result<(&'a Ability, Vec<(&'static str, &'a str, Cow<'a, str>)>)> {
        if !valid_id(ability_id) { bail!("Invalid bundled ability ID"); }
        let ability = self.abilities.iter().find(|a| a.id == ability_id)
            .filter(|a| Self::available(a, visibility, state)
                && allow_ability(ability_id)
                && a.tools.iter().any(|tool| self.installed(tool)
                    && Self::tool_mode(visibility, tool) != "off" && allow_tool(tool)))
            .context("Bundled skills unavailable under current access")?;
        let mut assets = Vec::new();
        if Self::skill_mode(visibility, ability_id) != "off" {
            if let Some(text) = Self::core_skill(ability) { assets.push(("ability", ability.id.as_str(), text)); }
        }
        for (name, text) in &ability.tool_skills {
            if self.installed(name) && Self::tool_mode(visibility, name) != "off" && allow_tool(name) {
                if let Some(body)=Self::asset_text(ability,"tool",name,text){assets.push(("tool",name.as_str(),body));}
            }
        }
        for (name,text) in &ability.selectable_skills {
            let Some(body)=Self::asset_text(ability,"selectable",name,text) else {continue;};
            if Self::selectable_metadata(name,&body).is_some()
                && Self::selectable_mode(visibility,ability_id,name,&body)!="off" {
                assets.push(("selectable",name.as_str(),body));
            }
        }
        assets.retain(|(kind, name, text)| Self::skill_live(ability, kind, name, text));
        Ok((ability, assets))
    }

    /// Project current authorized skill metadata without returning instruction text.
    pub(crate) fn bundled_skill_list(
        &self, ability_id: &str, visibility: &Value, state: &Value,
        allow_ability: impl Fn(&str) -> bool, allow_tool: impl Fn(&str) -> bool,
    ) -> Result<Vec<Value>> {
        let (ability, assets) = self.bundled_candidates(ability_id, visibility, state, &allow_ability, &allow_tool)?;
        Ok(assets.into_iter().map(|(kind, name, text)| {
            let mode = Self::asset_mode(visibility, ability, kind, name, &text);
            let supplied = mode == "visible";
            json!({"ability_id":ability.id,"kind":kind,"skill_id":name,
                "mode":mode,"availability":if supplied {"visible"} else {"discoverable"},
                "requires_activation":!supplied,
                "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
                "summary":text.lines().take(16).find_map(|line|
                    line.strip_prefix("when_to_use:").or_else(||line.strip_prefix("title:"))
                        .or_else(||line.strip_prefix("# ")))
                    .map(|line|line.trim().chars().take(240).collect::<String>())})
        }).collect())
    }

    /// Resolve independent core, tool and selectable visibility from current caller configuration.
    fn asset_mode<'a>(visibility: &'a Value, ability: &Ability, kind: &str, name: &str, text: &str) -> &'a str {
        match kind {
            "ability" => Self::skill_mode(visibility,&ability.id),
            "tool" => Self::tool_mode(visibility,name),
            _ => Self::selectable_mode(visibility,&ability.id,name,text),
        }
    }

    /// Return one current body and observation metadata under the scoped catalog gates.
    pub(crate) fn bundled_skill_load(
        &self, ability_id: &str, kind: &str, skill_id: &str, visibility: &Value, state: &Value,
        allow_ability: impl Fn(&str) -> bool, allow_tool: impl Fn(&str) -> bool,
    ) -> Result<Value> {
        if !valid_id(skill_id) || !matches!(kind, "ability" | "tool" | "selectable") {
            bail!("Invalid bundled skill ID or kind");
        }
        let (ability, assets) = self.bundled_candidates(ability_id, visibility, state, &allow_ability, &allow_tool)?;
        let (_, _, text) = assets.into_iter().find(|(k, name, _)| *k == kind && *name == skill_id)
            .context("Bundled skill unavailable under current access")?;
        Ok(json!({"ability_id":ability.id,"kind":kind,"skill_id":skill_id,
            "mode":Self::asset_mode(visibility,ability,kind,skill_id,&text),
            "availability":if Self::asset_mode(visibility,ability,kind,skill_id,&text)=="visible" {"visible"} else {"discoverable"},
            "requires_activation":Self::asset_mode(visibility,ability,kind,skill_id,&text)!="visible",
            "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
            "text":text,"source_kind":"untrusted_bundled_guidance","activates":false}))
    }

    fn executable(ability: &Ability) -> bool {
        Self::present(ability) && ability.required_module.as_deref().is_none_or(crate::modules::available)
            && matches!(ability.readiness, Readiness::Ready | Readiness::Partial)
            && matches!(ability.kind, EntryKind::Ability | EntryKind::VirtualCore)
    }

    pub fn ids(&self) -> Vec<String> {
        self.abilities.iter().filter(|ability| Self::present(ability))
            .map(|ability| ability.id.clone()).collect()
    }

    pub fn known(&self, id: &str) -> bool {
        self.abilities.iter().any(|ability| ability.id == id && Self::present(ability))
    }

    /// Optional package configuration is editable independently of installation.
    /// This metadata validator grants neither executable tools nor tenant authority.
    pub(crate) fn validate_optional_settings(&self, id: &str, settings: &Value) -> Option<bool> {
        let ability = self.abilities.iter().find(|ability| ability.id == id)?;
        ability.required_module.as_ref()?;
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

    fn installed(&self,name:&str)->bool {
        self.abilities.iter().any(|a|a.plugin.as_ref().is_some_and(|p|p.owns(name))) || Self::supported(name)
    }
    fn supported(name: &str) -> bool {
        crate::tools::native_schemas().iter().any(|schema| schema["name"] == name)
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

    fn is_off(visibility: &Value, id: &str) -> bool {
        visibility["visibility"][id] == "off"
    }

    fn skill_mode<'a>(visibility: &'a Value, id: &str) -> &'a str {
        visibility["skill_visibility"][id].as_str().unwrap_or("visible")
    }

    fn selectable_metadata(name: &str, text: &str) -> Option<(String, String, &'static str)> {
        let mut lines = text.lines();
        if lines.next()? != "---" { return None; }
        let (mut id, mut title, mut when, mut mode) = (None, None, None, "visible");
        let mut closed = false;
        for line in lines.by_ref() {
            if line == "---" { closed = true; break; }
            let Some((key, value)) = line.split_once(':') else { continue; };
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
        if !closed || id != Some(name) || lines.all(|line| line.trim().is_empty()) { return None; }
        let mode = match mode {
            "visible" | "always" | "always_on" => "visible",
            "discoverable" | "selectable" => "discoverable",
            _ => return None,
        };
        Some((title.to_owned(), when.to_owned(), mode))
    }

    fn selectable_mode<'a>(visibility: &'a Value, ability: &str, name: &str, text: &str) -> &'a str {
        let key = format!("{ability}:{name}");
        visibility["skill_visibility"][&key].as_str().unwrap_or_else(||
            Self::selectable_metadata(name, text).map(|(_, _, mode)| mode).unwrap_or("off"))
    }

    fn tool_mode<'a>(visibility: &'a Value, name: &str) -> &'a str {
        visibility["tool_modes"][name].as_str().unwrap_or("visible")
    }

    pub fn has_tool(&self, ability_id: &str, tool_name: &str) -> bool {
        self.abilities.iter().any(|ability|
            ability.id == ability_id && Self::present(ability) && ability.tools.iter().any(|tool| tool == tool_name))
    }

    fn is_visible(visibility: &Value, id: &str) -> bool {
        visibility["visibility"][id] == "visible"
    }

    fn available(ability: &Ability, visibility: &Value, state: &Value) -> bool {
        Self::executable(ability)
            && !Self::is_off(visibility, &ability.id)
            && !Self::suppressed(state, &ability.id)
            && (Self::is_visible(visibility, &ability.id) || Self::active(state, &ability.id))
    }

    fn describe(&self, ability: &Ability, policy: &Value, visibility: &Value, state: &Value) -> Value {
        let tools = ability
            .tools
            .iter()
            .filter(|name| Self::executable(ability) && self.installed(name) && Self::allowed(policy, name) && Self::tool_mode(visibility, name) != "off")
            .map(|name| json!({"name":name,"policy":policy["rules"][name],
                        "mode":Self::tool_mode(visibility,name),"supported":self.installed(name)}))
            .collect::<Vec<_>>();
        let visible = Self::is_visible(visibility, &ability.id);
        let off = Self::is_off(visibility, &ability.id);
        let mut general_skills=Vec::new();
        if Self::core_skill(ability).is_some() {
            general_skills.push(json!({"skill_id":"core","title":"Skill","when_to_use":ability.skill_summary,
                "mode":Self::skill_mode(visibility,&ability.id)}));
        }
        for (name,cached) in &ability.selectable_skills {
            let Some(text)=Self::asset_text(ability,"selectable",name,cached) else {continue;};
            if !Self::skill_live(ability,"selectable",name,&text) {continue;}
            if let Some((title,when,_))=Self::selectable_metadata(name,&text) {
                general_skills.push(json!({"skill_id":name,"title":title,"when_to_use":when,
                    "mode":Self::selectable_mode(visibility,&ability.id,name,&text)}));
            }
        }
        let mut described=json!({"id":ability.id,"group":ability.group,"group_name":ability.group_name,
            "name":ability.name,"description":ability.description,
            "kind":ability.kind,"readiness":ability.readiness,"config":ability.config,
            "skill_summary":ability.skill_summary,"migration_note":ability.migration_note,
            "icon":ability.icon,"color":ability.color,"tools":tools,
            "has_skill":Self::core_skill(ability).is_some(),
            "selectable_skills":ability.selectable_skills.iter().filter_map(|(name,cached)| Self::asset_text(ability,"selectable",name,cached).filter(|text|Self::skill_live(ability,"selectable",name,text) && Self::selectable_metadata(name,text).is_some() && Self::selectable_mode(visibility,&ability.id,name,text)!="off").map(|_|name)).collect::<Vec<_>>(),
            "general_skills":general_skills,
            "skill_visibility":Self::skill_mode(visibility, &ability.id),
            "visibility":if off {"off"} else if visible {"visible"} else {"discoverable"},
            "active":Self::active(state, &ability.id),
            "availability":if visible {"visible"} else if Self::active(state, &ability.id) {"activated"} else {"discoverable"},
            "requires_activation":!visible && !Self::active(state, &ability.id)});
        if ability.id=="codebase_admin" && !ability.frozen {
            if let Some(path)=Self::published_skill(ability,"ability","codebase_admin").and_then(|p|p.parent().map(|p|p.join("codebase_admin.json"))) {
                if let Ok(bytes)=read_bytes(&path,MAX_MANIFEST_BYTES) {
                    if let Ok(value)=serde_json::from_slice::<Value>(&bytes) {
                        for (field,key) in [("name","display_name"),("description","description"),("skill_summary","skill_summary"),("migration_note","migration_note"),("icon","icon"),("color","color")] {
                            if let Some(v)=value.get(key) {described[field]=v.clone();}
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
                    && ability.tools.iter().any(|name| self.installed(name) && Self::allowed(policy, name) && Self::tool_mode(visibility,name) != "off")
            })
            .map(|ability| self.describe(ability, policy, visibility, state))
            .collect()
    }

    /// Project installed tools and guidance for administrators using one schema snapshot.
    pub fn configuration_catalog(&self, policy: &Value, visibility: &Value) -> Vec<Value> {
        // Resolve dynamic package availability once for this projection.
        let schemas=self.all_schemas();
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
    pub fn configuration_skill(&self, id: &str) -> Option<Cow<'_ , str>> {
        let ability = self.abilities.iter().find(|ability| ability.id == id && Self::present(ability))?;
        Self::core_skill(ability)
    }

    /// Read current viewer guidance and validate selectable frontmatter from current text.
    pub fn configuration_skill_asset(&self, id: &str, kind: &str, name: &str) -> Option<Cow<'_ ,str>> {
        let ability = self.abilities.iter().find(|a| a.id == id && Self::present(a))?;
        let text = match kind {
            "skill" if valid_id(name) => ability.selectable_skills.get(name),
            "tool" if valid_id(name) && ability.tools.iter().any(|tool| tool == name) => ability.tool_skills.get(name),
            _ => None,
        }?;
        let body=Self::asset_text(ability,if kind=="skill"{"selectable"}else{"tool"},name,text)?;
        (kind!="skill" || Self::selectable_metadata(name,&body).is_some()).then_some(body)
    }

    /// Installed owners of a tool, independent of visibility or activation.
    /// A shared tool name must meet every owning ability permission.
    pub fn tool_abilities(&self,name:&str,_visibility:&Value,_state:&Value,
        _loaded:&Value) -> Vec<String> {
        if !self.installed(name) {return Vec::new();}
        self.abilities.iter().filter(|ability| {
            Self::executable(ability) && ability.tools.iter().any(|tool|tool==name)
        }).map(|ability|ability.id.clone()).collect()
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
        let state=self.selected_state(visibility,state,loaded);
        let supported = self.installed(name);
        let mut available = false;
        let mut visible = false;
        let selected=Self::contains_id(loaded,&crate::tools::core_tool_search::marker(name));
        for ability in &self.abilities {
            if !Self::executable(ability) || !ability.tools.iter().any(|tool| tool == name) {
                continue;
            }
            available |= supported;
            visible |= supported
                && match Self::tool_mode(visibility, name) {
                    "off" => false,
                    "discoverable" => selected,
                    _ => true,
                }
                && (Self::available(ability, visibility, &state)
                    || (selected && !Self::is_off(visibility,&ability.id)
                        && !Self::suppressed(&state,&ability.id)));
        }
        (available, visible)
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

    #[cfg(test)]
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
        let mut schemas = vec![
            json!({"type":"function","name":"list_abilities",
                "description":"Search permitted discoverable, inactive abilities to load. Visible and activated abilities are already available and omitted by default; set include_available=true to inspect the full permitted catalog. Prefer task words to a full listing.",
                "parameters":{"type":"object","properties":{"query":{"type":"string"},"include_available":{"type":"boolean","description":"Include already visible and activated abilities; no loading is needed for them"}},
                    "required":[],"additionalProperties":false}}),
            json!({"type":"function","name":"load_ability",
                "description":"Activate a discoverable ability by ID. Visible or already activated abilities return only an acknowledgment. Individual tool and skill visibility still applies; use list_tools/load_tool or search_tools for discoverable tools, and list_skills/load_skill for discoverable guidance.",
                "parameters":{"type":"object","properties":{"ability_id":{"type":"string"}},
                    "required":["ability_id"],"additionalProperties":false}}),
        ];
        schemas.extend(self.all_schemas().into_iter().filter(|tool| {
            let name = tool["name"].as_str().unwrap_or("");
            Self::allowed(policy, name) && self.tool_available(name, visibility, state, loaded)
        }));
        schemas
    }

    #[cfg(test)]
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
        self.visible_guidance_authorized(policy,visibility,state,allow_ability,&allow_tool,&allow_tool)
    }

    /// Retain compatibility guidance using authorized eligibility and supplied schemas.
    pub(crate) fn visible_guidance_authorized(
        &self, policy: &Value, visibility: &Value, state: &Value,
        allow_ability: impl Fn(&str)->bool,
        eligible_tool: impl Fn(&str)->bool,
        supplied_tool: impl Fn(&str)->bool,
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
                || !(Self::is_visible(visibility, &ability.id) || Self::active(state, &ability.id))
            {
                continue;
            }
            let mut part = format!("\n\nVisible ability {} ({}):", ability.id, ability.name);
            // Only typed saved settings enter guidance; they never grant use.
            for key in ["terminal_caution","destructive_caution"] {
                if let Some(value)=visibility["settings"][&ability.id][key].as_str()
                    .filter(|v|matches!(*v,"cautious"|"balanced"|"trusted")) {
                    part.push_str(&format!("\nConfigured {key}: {value}. Live Rust policy and approval remain authoritative."));
                }
            }

            // Visible core guidance is mandatory. Discoverable skill loads rebuild
            // their own context layer, independently of ability activation.
            if Self::skill_mode(visibility, &ability.id) == "visible" {
                if let Some(skill) = Self::core_skill(ability) {
                    part.push_str("\n");
                    part.push_str(&skill);
                }
            }
            for (tool, cached) in &ability.tool_skills {
                let Some(skill)=Self::asset_text(ability,"tool",tool,cached)else{continue;};
                if self.installed(tool) && Self::allowed(policy, tool)
                    && supplied_tool(tool)
                    && Self::skill_live(ability, "tool", tool, &skill)
                    && Self::tool_mode(visibility, tool) == "visible" {
                    part.push_str("\n\nTool ");
                    part.push_str(tool);
                    part.push_str(":\n");
                    part.push_str(&skill);
                }
            }
            for (name, cached) in &ability.selectable_skills {
                let Some(skill)=Self::asset_text(ability,"selectable",name,cached)else{continue;};
                if Self::selectable_metadata(name,&skill).is_some()
                    && Self::selectable_mode(visibility, &ability.id, name, &skill) == "visible"
                    && Self::skill_live(ability, "selectable", name, &skill) {
                    part.push_str("\n\nSkill ");
                    part.push_str(name);
                    part.push_str(":\n");
                    part.push_str(&skill);
                }
            }
            // Administrator-selected guidance is mandatory; assembly budgets the complete request.
            text.push_str(&part);
        }
        text
    }

    // @code-index {"version":1,"feature":"live-capability-sources","aliases":["live skill refresh","permanent selections"],"purpose":"Resolve registered current sources and conversation identities for capability composition. Caller policy and encrypted visibility remain separate authority; fixed path/source safety is not configurable.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Live encrypted policy/visibility and validated catalog state own authority. This group maps those inputs to safe selectors/current bytes; another JSON switch would duplicate or bypass those checks."}}
    /// Project conversation-selected identities into a call-local activation view.
    /// This never changes saved session state or overrides Off, suppression,
    /// executable presence, tool modes, policy, or caller permission checks.
    pub(crate) fn selected_state(&self,visibility:&Value,state:&Value,loaded:&Value)->Value {
        let mut selected=state.clone();
        if !selected.is_object() {selected=json!({});}
        let mut active=state["active"].as_array().cloned().unwrap_or_default();
        for ability in &self.abilities {
            if !Self::contains_id(loaded,&ability.id) || !Self::executable(ability)
                || Self::is_off(visibility,&ability.id) || Self::suppressed(state,&ability.id)
                || !ability.tools.iter().any(|tool|self.installed(tool)
                    && Self::tool_mode(visibility,tool)!="off") {continue;}
            if !active.iter().any(|item|item.as_str()==Some(ability.id.as_str())) {active.push(json!(ability.id));}
        }
        selected["active"]=json!(active);
        selected
    }

    /// Read source-backed guidance under the host's already-authorized catalog
    /// and schema projections. This grants no execution authority; callers must
    /// recheck their complete source/policy fingerprint before model dispatch.
    pub(crate) fn capability_guidance(&self,entries:&[Value],schemas:&[Value],
        visibility:&Value,state:&Value,loaded:&Value,loaded_skills:&Value)
        ->Vec<webagent_capability_contracts::Guidance> {
        use webagent_capability_contracts::{Guidance,GuidanceKind};
        let sent=schemas.iter().filter_map(|schema|schema["name"].as_str()).collect::<HashSet<_>>();
        let mut guidance=Vec::new();
        for ability in &self.abilities {
            let id=ability.id.as_str();
            let Some(entry)=entries.iter().find(|entry|entry["id"]==id) else {continue;};
            if !Self::executable(ability) || Self::is_off(visibility,id) || Self::suppressed(state,id) {continue;}
            let names=entry["tools"].as_array().into_iter().flatten()
                .filter_map(|tool|tool["name"].as_str())
                .filter(|name|ability.tools.iter().any(|tool|tool.as_str()==*name)
                    && self.installed(name) && Self::tool_mode(visibility,name)!="off")
                .collect::<HashSet<_>>();
            if names.is_empty() {continue;}
            let selected_tool=|name:&str|sent.contains(name)
                && Self::contains_id(loaded,&crate::tools::core_tool_search::marker(name));
            let normal=Self::available(ability,visibility,state);
            let extra=names.iter().any(|name|selected_tool(name));
            let explicit=|kind:&str,name:&str|normal && sent.contains("load_skill")
                && loaded_skills.as_array().is_some_and(|items|items.iter().any(|item|
                    item["ability_id"]==id && item["kind"]==kind && item["skill_id"]==name));
            // Saved caution remains mandatory independently of core-skill mode.
            // Keeping it separate prevents it from promoting discoverable skill text.
            let mut settings=String::new();
            for key in ["terminal_caution","destructive_caution"] {
                if let Some(value)=visibility["settings"][id][key].as_str()
                    .filter(|value|matches!(*value,"cautious"|"balanced"|"trusted")) {
                    settings.push_str(&format!("\nConfigured {key}: {value}. Live Rust policy and approval remain authoritative."));
                }
            }
            if !settings.is_empty() {
                guidance.push(Guidance {id:format!("ability:{id}:settings"),kind:GuidanceKind::Ability,
                    body:settings,selected:false,natural:normal||extra});
            }
            if Self::skill_mode(visibility,id)!="off" {
                if let Some(body)=Self::core_skill(ability) {
                    guidance.push(Guidance {id:format!("ability:{id}"),kind:GuidanceKind::Ability,body:body.into_owned(),
                        selected:explicit("ability",id),
                        natural:Self::skill_mode(visibility,id)=="visible" && (normal||extra)});
                }
            }
            for (name,cached) in &ability.tool_skills {
                if !names.contains(name.as_str()) {continue;}
                let Some(body)=Self::asset_text(ability,"tool",name,cached) else {continue;};
                let mode=Self::tool_mode(visibility,name);
                guidance.push(Guidance {id:format!("tool:{id}:{name}"),kind:GuidanceKind::Tool,body:body.into_owned(),
                    selected:explicit("tool",name),
                    natural:sent.contains(name.as_str()) && ((normal&&mode=="visible") || selected_tool(name))});
            }
            for (name,cached) in &ability.selectable_skills {
                let Some(body)=Self::asset_text(ability,"selectable",name,cached) else {continue;};
                if Self::selectable_metadata(name,&body).is_none() {continue;}
                let mode=Self::selectable_mode(visibility,id,name,&body);
                if mode=="off" {continue;}
                guidance.push(Guidance {id:format!("selectable:{id}:{name}"),kind:GuidanceKind::Selectable,
                    body:body.into_owned(),selected:explicit("selectable",name),natural:normal&&mode=="visible"});
            }
        }
        guidance
    }

    /// Trace the same typed inclusion predicates used by guidance producers.
    /// Byte counts refer to included skill content, excluding section headings.
    pub(crate) fn context_inclusion(&self, entries: &[Value], schemas: &[Value],
        visibility: &Value, state: &Value, loaded: &Value, loaded_skills: &Value) -> Vec<AbilityInclusion> {
        let sent = schemas.iter().filter_map(|s|s["name"].as_str()).collect::<HashSet<_>>();
        entries.iter().filter_map(|entry| {
            let id = entry["id"].as_str()?;
            let ability = self.abilities.iter().find(|ability| ability.id == id)?;
            let names = entry["tools"].as_array()?.iter()
                .filter_map(|t| t["name"].as_str()).collect::<HashSet<_>>();
            let selected = |name: &str| sent.contains(name)
                && Self::contains_id(loaded,&crate::tools::core_tool_search::marker(name));
            let normal = Self::available(ability,visibility,state) && !names.is_empty();
            let extra = names.iter().any(|name|selected(name));
            let mut skills = Vec::new();
            let mut add = |kind: &str, name: &str, mode: &str, body: &str, natural: bool| {
                let explicit = mode!="off" && Self::available(ability,visibility,state) && sent.contains("load_skill") && loaded_skills.as_array().is_some_and(|items|items.iter().any(|item|
                    item["ability_id"]==id && item["kind"]==kind && item["skill_id"]==name));
                let included = natural || explicit;
                let reason = if included {"included"} else if mode=="off" {"off"}
                    else if mode=="discoverable" || entry["availability"]=="discoverable" {"discoverable"}
                    else {"unavailable"};
                skills.push(SkillInclusion {kind:kind.into(),id:name.into(),mode:mode.into(),included,
                    content_bytes:if included {body.len()} else {0},reason:reason.into()});
            };
            if let Some(body)=Self::core_skill(ability) {
                let mode=Self::skill_mode(visibility,id);
                add("ability",id,mode,&body,mode=="visible"&&(normal||extra));
            }
            for (name,cached) in &ability.tool_skills {
                let Some(body)=Self::asset_text(ability,"tool",name,cached)else{continue;};
                if names.contains(name.as_str()) && Self::skill_live(ability,"tool",name,&body) {
                    let mode=Self::tool_mode(visibility,name);
                    add("tool",name,mode,&body,sent.contains(name.as_str())
                        && ((normal&&mode=="visible") || (mode!="off"&&selected(name))));
                }
            }
            for (name,cached) in &ability.selectable_skills {
                let Some(body)=Self::asset_text(ability,"selectable",name,cached)else{continue;};
                if Self::selectable_metadata(name,&body).is_some() && Self::skill_live(ability,"selectable",name,&body) {
                    let mode=Self::selectable_mode(visibility,id,name,&body);
                    add("selectable",name,mode,&body,normal&&mode=="visible");
                }
            }
            Some(AbilityInclusion {id:id.into(),visibility:entry["visibility"].as_str()?.into(),
                availability:entry["availability"].as_str()?.into(),
                guidance_bytes:skills.iter().map(|skill|skill.content_bytes).sum(),
                schemas:schemas.iter().filter_map(|schema|schema["name"].as_str())
                    .filter(|name|names.contains(name)).map(str::to_owned).collect(),skills})
        }).collect()
    }

    /// Render a complete inventory from the exact entries and schemas authorized
    /// for this model call. Discoverable abilities keep their tool schemas hidden.
    pub fn model_index(entries: &[Value], schemas: &[Value]) -> String {
        fn add(out: &mut String, line: &str) {
            out.push_str(line);out.push('\n');
        }
        fn short(value: &str) -> String {
            value.split_whitespace().collect::<Vec<_>>().join(" ")
                .chars().take(160).collect()
        }
        let callable = schemas.iter().filter_map(|s| s["name"].as_str()).collect::<HashSet<_>>();
        let can_list = callable.contains("list_abilities");
        let can_load = callable.contains("load_ability");
        let mut discoverable = Vec::new();
        let mut ready_tools = BTreeMap::new();
        let mut loadable_tools = BTreeMap::new();
        for entry in entries {
            let Some(id) = entry["id"].as_str() else { continue };
            if entry["visibility"] == "discoverable" && entry["active"] != true {
                if can_load {
                    let name = short(entry["name"].as_str().unwrap_or(id));
                    let summary = short(entry["skill_summary"].as_str()
                        .or_else(|| entry["description"].as_str()).unwrap_or(""));
                    discoverable.push((id, name, summary));
                }
                continue;
            }
            if let Some(tools) = entry["tools"].as_array() {
                for tool in tools {
                    if let Some(name) = tool["name"].as_str() {
                        if is_discovery(name) { continue; }
                        if callable.contains(name) {
                            let description = schemas.iter().find(|s| s["name"] == name)
                                .and_then(|s| s["description"].as_str()).unwrap_or("");
                            ready_tools.insert(name.to_owned(), short(description));
                        } else if can_load && tool["mode"] == "discoverable" {
                            loadable_tools.insert(name.to_owned(), id.to_owned());
                        }
                    }
                }
            }
        }
        if !can_list && !can_load && discoverable.is_empty() && ready_tools.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        add(&mut out, "# [TOOLS]");
        add(&mut out, "These tools can be called now. A call may still require approval.");
        if can_list { add(&mut out, "- list_abilities — find inactive discoverable abilities; include_available=true also shows visible and activated abilities"); }
        if can_load { add(&mut out, "- load_ability — activate a discoverable ability; visible and activated abilities need no reload"); }
        for (name, description) in ready_tools {
            let row = if description.is_empty() { format!("- {name}") }
                else { format!("- {name} — {description}") };
            add(&mut out, &row);
        }
        if !loadable_tools.is_empty() {
            add(&mut out, "");
            add(&mut out, "## Load on demand");
            add(&mut out, "These tools are discoverable; their schemas are withheld independently of ability visibility. Use list_tools/load_tool to select an exact tool, or search_tools for model-ranked selection, before use.");
            for (name, id) in loadable_tools {
                add(&mut out, &format!("- {name} — discoverable tool in ability {id}"));
            }
        }
        if !discoverable.is_empty() {
            add(&mut out, "");
            add(&mut out, "# [ABILITIES]");
            add(&mut out, "These abilities are available but not loaded. Call load_ability with an ID to activate the ability. Tool and skill visibility are independent; use list_tools/load_tool and list_skills/load_skill for discoverable items. An empty loaded list does not mean no abilities are available. Search list_abilities with task words or an empty query for inactive discoverable abilities. Visible and activated abilities are already available; use their supplied tools directly.");
            for (id, name, summary) in discoverable {
                let row = if summary.is_empty() { format!("- {id} — {name}") }
                    else { format!("- {id} — {name}: {summary}") };
                add(&mut out, &row);
            }
        }
        out
    }

    pub fn discover_with(
        &self,
        name: &str,
        args: &Value,
        run: &mut Value,
        policy: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Value {
        match name {
            "list_abilities" => {
                let query = args["query"].as_str().unwrap_or("").trim().to_lowercase();
                if query.len() > 200 {
                    return json!({"status":"error","output":"Query is too long"});
                }
                let include_available = match args.get("include_available") {
                    None | Some(Value::Null) => false,
                    Some(Value::Bool(value)) => *value,
                    _ => return json!({"status":"error","output":"include_available must be a boolean"}),
                };
                let mut found = self.catalog_with(policy, visibility, state)
                    .into_iter()
                    .filter(|ability| include_available || ability["requires_activation"] == true)
                    .filter_map(|ability| {
                        let score = discovery_score(&ability, &query);
                        (query.is_empty() || score > 0).then_some((score, ability))
                    }).collect::<Vec<_>>();
                if !query.is_empty() {
                    found.sort_by(|(left_score, left), (right_score, right)| {
                        right_score.cmp(left_score).then_with(||
                            left["id"].as_str().cmp(&right["id"].as_str()))
                    });
                }
                let found = found.into_iter().map(|(_, ability)| ability).collect::<Vec<_>>();
                json!({"status":"ok","catalog_version":self.version,"include_available":include_available,"abilities":found})
            }
            "load_ability" => {
                let Some(id) = args["ability_id"].as_str() else {
                    return json!({"status":"error","output":"ability_id is required"});
                };
                let Some(entry) = self.abilities.iter().find(|ability| ability.id == id && Self::present(ability)) else {
                    return json!({"status":"denied","output":"Ability unavailable"});
                };
                if !Self::executable(entry) {
                    return json!({"status":"denied","output":"Ability pending or non-executable"});
                }
                if Self::is_off(visibility, id) {
                    return json!({"status":"denied","output":"Ability disabled for this agent"});
                }
                if Self::suppressed(state, id) {
                    return json!({"status":"denied","output":"Ability suppressed in this session"});
                }
                let ability = self.describe(entry, policy, visibility, state);
                if ability["tools"].as_array().is_none_or(Vec::is_empty) {
                    return json!({"status":"denied","output":"Ability unavailable under current policy"});
                }
                if Self::is_visible(visibility, id) || Self::active(state, id) {
                    return json!({"status":"already_available","ability_id":id,
                        "availability":ability["availability"],"scope":"session","catalog_version":self.version,
                        "context_source":"live_ability_catalog",
                        "message":"This ability is already available; use its supplied tools and guidance directly."});
                }
                if !run["loaded_abilities"].is_array() {
                    run["loaded_abilities"] = json!([]);
                }
                let loaded = run["loaded_abilities"].as_array_mut().unwrap();
                if !loaded.iter().any(|item| item.as_str() == Some(id)) {
                    loaded.push(json!(id));
                }
                json!({"status":"ok","ability_id":id,"availability":"activated",
                    "scope":"session","catalog_version":self.version,"context_source":"live_ability_catalog",
                    "message":"Ability activated. Its available tools and guidance are supplied in the next model context; individual visibility and permissions still apply."})
            }
            _ => json!({"status":"denied","output":"Unknown discovery operation"}),
        }
    }

    pub fn discover(&self, name: &str, args: &Value, run: &mut Value, policy: &Value) -> Value {
        self.discover_with(name, args, run, policy, &Value::Null, &Value::Null)
    }
}

pub fn is_discovery(name: &str) -> bool {
    matches!(name, "list_abilities" | "load_ability")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;


    #[test]
    fn index_guidance_is_confined_to_registered_published_assets() {
        let (root, catalog) = fixture();
        let mut ability = catalog.abilities[0].clone();
        let ui = root.path().join("assets/current/v3/ui");
        assert_eq!(Catalog::published_skill_path(&ability, "ability", "codebase_admin", Some(&ui)).unwrap(),root.path().join("assets/current/v3/plugins/abilities/Administrator/codebase_admin/codebase_admin.skill.md"));
        ability.id = "code_index".into();
        ability.tools = vec!["code_index_lookup".into()];
        let path = Catalog::published_skill_path(&ability, "ability", "code_index", Some(&ui)).unwrap();
        assert_eq!(path, root.path().join("assets/current/v3/plugins/abilities/Administrator/code_index/code_index.skill.md"));
        assert!(Catalog::published_skill_path(&ability, "tool", "../secret", Some(&ui)).is_none());
        assert!(Catalog::published_skill_path(&ability, "tool", "unregistered", Some(&ui)).is_none());
        assert!(Catalog::published_skill_path(&ability, "ability", "code_index", Some(Path::new("/tmp/other"))).is_none());
        let tool = Catalog::published_skill_path(&ability, "tool", "code_index_lookup", Some(&ui)).unwrap();
        fs::create_dir_all(tool.parent().unwrap()).unwrap();
        fs::write(&tool, "First published instructions").unwrap();
        assert_eq!(optional_skill(&tool).unwrap().unwrap(), "First published instructions");
        fs::write(&tool, "Next published instructions").unwrap();
        assert_eq!(optional_skill(&tool).unwrap().unwrap(), "Next published instructions");
    }

    /// Share the exact isolated native fixture with focused sibling capability tests.
    pub(super) fn fixture() -> (TempDir, Catalog) {
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
            "tools":["file_list","file_read","file_search","state_inspect","prompt_slot_read","prompt_slot_write","file_write","file_edit","workspace_seed","source_review","source_patch","promotion_propose","shell","python"]
        }"#).unwrap();
        fs::write(
            ability.join("codebase_admin.skill.md"),
            "Use scoped workspace tools.",
        )
        .unwrap();
        fs::write(
            ability.join("tools/file_edit.skill.md"),
            "Match one exact span.",
        )
        .unwrap();
        let catalog = Catalog::load(root.path()).unwrap();
        (root, catalog)
    }

    #[test]
    fn visible_large_guidance_is_complete_and_discovery_is_not_activation() {
        let (root, _) = fixture();
        let folder=root.path().join("Administrator/codebase_admin");
        let core=format!("CORE-BEGIN{}CORE-END", "c".repeat(30000));
        let tool=format!("TOOL-BEGIN{}TOOL-END", "t".repeat(30000));
        fs::write(folder.join("codebase_admin.skill.md"), &core).unwrap();
        fs::write(folder.join("tools/file_edit.skill.md"), &tool).unwrap();
        let catalog=Catalog::load(root.path()).unwrap();
        let p=policy(); let v=json!({"visibility":{"codebase_admin":"visible"}});
        let state=json!({"active":[],"suppressed":[]});
        let guidance=catalog.visible_guidance(&p,&v,&state);
        assert!(guidance.contains(&core));assert!(guidance.contains(&tool));
        assert!(!guidance.contains("omitted"));
        let mut run=json!({"loaded_abilities":[]});let before=run.clone();
        let hidden=catalog.discover_with("list_abilities",&json!({}),&mut run,&p,&v,&state);
        assert!(hidden["abilities"].as_array().unwrap().is_empty());
        let all=catalog.discover_with("list_abilities",&json!({"include_available":true}),&mut run,&p,&v,&state);
        assert_eq!(all["abilities"][0]["availability"],"visible");
        assert_eq!(all["abilities"][0]["requires_activation"],false);
        let load=catalog.discover_with("load_ability",&json!({"ability_id":"codebase_admin"}),&mut run,&p,&v,&state);
        assert_eq!(load["status"],"already_available");
        assert!(load.get("skill").is_none());assert!(load.get("tool_skills").is_none());assert_eq!(run,before);
        let schemas=catalog.schemas_with(&p,&run["loaded_abilities"],&v,&state);
        let entries=catalog.catalog_with(&p,&v,&state);
        let report=catalog.context_inclusion(&entries,&schemas,&v,&state,&json!([]),&json!([]));
        assert!(report[0].skills.iter().all(|s|s.included));
        assert!(report[0].schemas.contains(&"file_read".into()));
        let value=serde_json::to_value(AbilityContext{turn:0,call:1,abilities:report}).unwrap();
        assert!(!value.to_string().contains("CORE-BEGIN"));
        assert!(AbilityContext::public(&value).is_some());
    }

    #[test]
    fn discoverable_activation_is_durable_without_reloading_or_revealing_discoverable_tools() {
        let (_root,catalog)=fixture();let p=policy();
        let v=json!({"visibility":{"codebase_admin":"discoverable"},
            "tool_modes":{"file_search":"discoverable","file_edit":"off"}});
        let mut state=json!({"active":[],"suppressed":[]});let mut run=json!({"loaded_abilities":[]});
        let list=catalog.discover_with("list_abilities",&json!({}),&mut run,&p,&v,&state);
        assert_eq!(list["abilities"][0]["availability"],"discoverable");
        assert_eq!(list["abilities"][0]["requires_activation"],true);
        let first=catalog.discover_with("load_ability",&json!({"ability_id":"codebase_admin"}),&mut run,&p,&v,&state);
        assert_eq!(first["status"],"ok");
        state["active"]=json!(["codebase_admin"]);
        assert!(catalog.visible_guidance(&p,&v,&state).contains("Use scoped workspace tools."));
        assert!(catalog.tool_available("file_read",&v,&state,&run["loaded_abilities"]));
        assert!(!catalog.tool_available("file_search",&v,&state,&run["loaded_abilities"]));
        let saved=run.clone();
        let repeat=catalog.discover_with("load_ability",&json!({"ability_id":"codebase_admin"}),&mut run,&p,&v,&state);
        assert_eq!(repeat["status"],"already_available");assert!(repeat.get("skill").is_none());assert_eq!(run,saved);
        let all=catalog.discover_with("list_abilities",&json!({"include_available":true}),&mut run,&p,&v,&state);
        assert_eq!(all["abilities"][0]["availability"],"activated");
        assert!(catalog.discover_with("list_abilities",&json!({}),&mut run,&p,&v,&state)["abilities"].as_array().unwrap().is_empty());
        let empty=json!({"loaded_abilities":[]});
        assert!(catalog.schemas_with(&p,&empty["loaded_abilities"],&v,&state).iter().any(|s|s["name"]=="file_read"));
        state["suppressed"]=json!(["codebase_admin"]);
        assert!(catalog.visible_guidance(&p,&v,&state).is_empty());
        assert_eq!(catalog.discover_with("load_ability",&json!({"ability_id":"codebase_admin"}),&mut run,&p,&v,&state)["status"],"denied");
    }

    #[test]
    fn diagnostic_projection_rejects_content_and_unknown_identifiers() {
        let value=json!({"turn":1,"call":1,"abilities":[],"prompt":"PRIVATE"});
        assert!(AbilityContext::public(&value).is_none());
    }

    #[test]
    fn search_activation_preserves_visible_tools_and_respects_denials() {
        let (_root, catalog) = fixture();
        let mut policy = policy();
        let visibility = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_search":"discoverable","file_edit":"off"}});
        let state = json!({"active":[],"suppressed":[]});
        let mut run = json!({"pending":{"name":"search_tools"},"loaded_abilities":[]});
        let names = |run: &Value, policy: &Value, state: &Value| {
            catalog.schemas_with(policy, &run["loaded_abilities"], &visibility, state)
                .into_iter().map(|s|s["name"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
        };
        let before = names(&run, &policy, &state);
        assert!(before.contains(&"file_read".into()));
        assert!(!before.contains(&"file_search".into()));
        crate::tools::core_tool_search::apply_selection(&mut run,
            &json!({"status":"success","output":{"activated":true,
                "selected_tools":["file_search","file_edit","python"]}}));
        let after = names(&run, &policy, &state);
        assert!(before.iter().all(|name|after.contains(name)));
        assert!(after.contains(&"file_search".into()));
        assert!(!after.contains(&"file_edit".into()));
        assert!(!after.contains(&"python".into()));
        crate::tools::core_tool_search::apply_selection(&mut run,
            &json!({"status":"success","output":{"activated":true,"selected_tools":[]}}));
        assert_eq!(names(&run, &policy, &state), after);
        policy["rules"]["file_search"] = json!("deny");
        assert!(!names(&run, &policy, &state).contains(&"file_search".into()));
        assert!(!names(&run, &policy, &json!({"active":[],"suppressed":["codebase_admin"]}))
            .contains(&"file_read".into()));
    }

    #[test]
    fn natural_task_search_finds_permitted_ability_and_tools() {
        let (_root, catalog) = fixture();
        let mut run = json!({"loaded_abilities":[]});
        let policy = policy();
        let visible = Value::Null;
        let active = json!({"active":[],"suppressed":[]});
        let result = catalog.discover_with("list_abilities",
            &json!({"query":"Inspect and edit the current session workspace source code"}),
            &mut run, &policy, &visible, &active);
        assert_eq!(result["abilities"][0]["id"], "codebase_admin");
        let typo = catalog.discover_with("list_abilities", &json!({"query":"edti"}),
            &mut run, &policy, &visible, &active);
        assert_eq!(typo["abilities"][0]["id"], "codebase_admin");
        let denied = json!({"rules":{"file_list":"deny","file_read":"deny",
            "file_search":"deny","source_review":"deny","state_inspect":"deny",
            "prompt_slot_read":"deny","file_write":"deny","file_edit":"deny",
            "workspace_seed":"deny","source_patch":"deny","promotion_propose":"deny",
            "shell":"deny","python":"deny"}});
        assert!(catalog.discover_with("list_abilities", &json!({"query":"source"}),
            &mut run, &denied, &visible, &active)["abilities"].as_array().unwrap().is_empty());
    }

    #[test]
    fn changed_manifest_withdraws_cached_loaded_tools_until_reload() {
        for change in ["pending", "remove_tool", "malformed"] {
            let (root, catalog) = fixture();
            let path = root.path().join("Administrator/codebase_admin/codebase_admin.json");
            let original = fs::read(&path).unwrap();
            let mut manifest: Value = serde_json::from_slice(&original).unwrap();
            let p = policy();
            let visibility = json!({"visibility":{"codebase_admin":"visible"}});
            let active = json!({"active":["codebase_admin"],"suppressed":[]});
            let loaded = json!(["codebase_admin"]);
            assert_eq!(catalog.tool_gate("file_read", &visibility, &active, &loaded), (true, true));
            match change {
                "pending" => manifest["readiness"] = json!("pending"),
                "remove_tool" => manifest["tools"].as_array_mut().unwrap().retain(|t| t != "file_read"),
                _ => {}
            }
            fs::write(&path, if change == "malformed" { b"{".to_vec() }
                else { serde_json::to_vec(&manifest).unwrap() }).unwrap();
            // All cached entry authority is withdrawn, not just the edited tool.
            assert_eq!(catalog.tool_gate("file_read", &visibility, &active, &loaded), (false, false));
            assert_eq!(catalog.tool_gate("file_write", &visibility, &active, &loaded), (false, false));
            assert!(!catalog.schemas_with(&p, &loaded, &visibility, &active)
                .iter().any(|s| s["name"] == "file_read" || s["name"] == "file_write"));
            assert!(!catalog.known("codebase_admin"));
            let mut run = json!({"loaded_abilities":loaded});
            let found = catalog.discover_with("list_abilities", &json!({}),
                &mut run, &p, &visibility, &active);
            assert!(found["abilities"].as_array().unwrap().is_empty());
            assert_eq!(catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
                &mut run, &p, &visibility, &active)["status"], "denied");
            if change == "remove_tool" {
                let reloaded = Catalog::load(root.path()).unwrap();
                assert_ne!(reloaded.version(), catalog.version());
                assert_eq!(reloaded.tool_gate("file_read", &visibility, &active, &loaded), (false, false));
                assert_eq!(reloaded.tool_gate("file_write", &visibility, &active, &loaded), (true, true));
            }
        }
    }

    #[test]
    fn bundled_skill_access_is_exact_scoped_and_withdraws_changed_assets() {
        let (root, _) = fixture();
        let folder = root.path().join("Administrator/codebase_admin");
        fs::create_dir(folder.join("skills")).unwrap();
        let theme = "---\nid: ui_theming\ntitle: UI theming\nwhen_to_use: Use for theme changes\ndefault_mode: visible\n---\nScoped theme guidance";
        fs::write(folder.join("skills/ui_theming.skill.md"), theme).unwrap();
        fs::write(folder.join("skills/ui_toggle_list.skill.md"),
            "---\nid: ui_toggle_list\ntitle: Toggle lists\nwhen_to_use: Use for list controls\ndefault_mode: discoverable\n---\nScoped list guidance").unwrap();
        let catalog = Catalog::load(root.path()).unwrap();
        let visible = json!({"visibility":{"codebase_admin":"visible"}});
        let active = json!({"active":["codebase_admin"],"suppressed":[]});
        let list = catalog.bundled_skill_list("codebase_admin", &visible, &active,
            |_| true, |tool| tool == "file_read").unwrap();
        assert_eq!(list.len(), 3);
        assert!(!list.iter().any(|s| s["kind"] == "tool"));
        let loaded = catalog.bundled_skill_load("codebase_admin", "selectable", "ui_theming",
            &visible, &active, |_| true, |_| true).unwrap();
        assert_eq!(loaded["text"], theme);
        assert_eq!(loaded["source_kind"], "untrusted_bundled_guidance");
        assert_eq!(loaded["activates"], false);
        assert_eq!(loaded["sha256"], hex::encode(Sha256::digest(theme.as_bytes())));
        assert!(catalog.bundled_skill_load("codebase_admin", "selectable", "../ui_theming",
            &visible, &active, |_| true, |_| true).is_err());
        assert!(catalog.bundled_skill_load("codebase_admin", "tool", "file_edit",
            &visible, &active, |_| true, |tool| tool == "file_read").is_err());
        assert!(catalog.bundled_skill_list("codebase_admin", &visible, &active, |_| false, |_| true).is_err());
        let off = json!({"visibility":{"codebase_admin":"visible"},"skill_visibility":{"codebase_admin":"off"}});
        let without_root=catalog.bundled_skill_list("codebase_admin", &off, &active,
            |_| true, |tool|tool=="file_read").unwrap();
        assert_eq!(without_root.len(),2);
        assert!(!without_root.iter().any(|skill|skill["kind"]=="ability"));
        let separate=json!({"visibility":{"codebase_admin":"visible"},
            "skill_visibility":{"codebase_admin:ui_theming":"off"}});
        let separate_list=catalog.bundled_skill_list("codebase_admin",&separate,&active,
            |_|true,|tool|tool=="file_read").unwrap();
        assert!(!separate_list.iter().any(|skill|skill["skill_id"]=="ui_theming"));
        assert!(separate_list.iter().any(|skill|skill["skill_id"]=="ui_toggle_list"));
        assert!(catalog.bundled_skill_list("codebase_admin", &visible,
            &json!({"active":["codebase_admin"],"suppressed":["codebase_admin"]}), |_| true, |_| true).is_err());
        assert!(catalog.bundled_skill_list("codebase_admin", &json!({}), &json!({}), |_| true, |_| true).is_err());

        let before = catalog.live_version();
        fs::write(folder.join("skills/ui_theming.skill.md"), "Unvalidated replacement").unwrap();
        assert_ne!(catalog.live_version(), before);
        assert!(catalog.bundled_skill_load("codebase_admin", "selectable", "ui_theming",
            &visible, &active, |_| true, |_| true).is_err());
        assert!(!catalog.bundled_skill_list("codebase_admin", &visible, &active, |_| true, |_| true)
            .unwrap().iter().any(|s| s["skill_id"] == "ui_theming"));
        let mut run = json!({"loaded_abilities":["codebase_admin"]});
        let discovery = catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
            &mut run, &policy(), &visible, &active);
        assert_eq!(discovery["status"], "already_available");
        assert!(discovery.get("selectable_skills").is_none());
        fs::remove_file(folder.join("codebase_admin.skill.md")).unwrap();
        fs::remove_file(folder.join("tools/file_edit.skill.md")).unwrap();
        let guidance = catalog.visible_guidance(&policy(), &visible, &active);
        assert!(!guidance.contains("Use scoped workspace tools."));
        assert!(!guidance.contains("Match one exact span."));
        let discovery = catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
            &mut run, &policy(), &visible, &active);
        assert!(discovery["skill"].is_null());
        assert!(discovery.get("tool_skills").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_replacement_cannot_restore_cached_bundled_guidance() {
        use std::os::unix::fs::symlink;
        let (root, catalog) = fixture();
        let folder = root.path().join("Administrator/codebase_admin");
        let path = folder.join("codebase_admin.skill.md");
        let original = fs::read(&path).unwrap();
        let alternate = root.path().join("replacement.skill.md");
        fs::write(&alternate, original).unwrap();
        let before = catalog.live_version();
        fs::remove_file(&path).unwrap();
        symlink(&alternate, &path).unwrap();
        assert_ne!(catalog.live_version(), before);
        assert!(catalog.bundled_skill_load("codebase_admin", "ability", "codebase_admin",
            &json!({"visibility":{"codebase_admin":"visible"}}), &json!({}), |_| true, |_| true).is_err());
    }

    /// Keep focused sibling tests on the same installed-tool policy fixture.
    pub(super) fn policy() -> Value {
        json!({"rules":{"file_list":"allow","file_read":"allow","file_search":"allow",
            "state_inspect":"allow","prompt_slot_read":"allow","prompt_slot_write":"require_approval","file_write":"require_approval","file_edit":"require_approval","workspace_seed":"require_approval","source_review":"allow","source_patch":"require_approval","promotion_propose":"allow",
            "shell":"require_approval","python":"deny"}})
    }

    #[test]
    fn model_index_advertises_discoverable_abilities_without_hidden_tools() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let state = json!({"active":[],"suppressed":[]});
        let entries = catalog.catalog_with(&policy, &Value::Null, &state);
        let schemas = catalog.schemas_with(&policy, &json!([]), &Value::Null, &state);
        let index = Catalog::model_index(&entries, &schemas);
        assert!(index.contains("# [ABILITIES]"));
        assert!(index.contains("codebase_admin — Codebase Admin"));
        assert!(index.contains("An empty loaded list does not mean no abilities"));
        assert!(!index.contains("- file_read"));
        assert!(index.contains("codebase_admin"));
        assert!(!index.contains("Use scoped workspace tools."));
        let crowded = Catalog::model_index(&vec![entries[0].clone(); 512], &schemas);
        assert_eq!(crowded.matches("codebase_admin —").count(),512);
        assert!(!crowded.contains("More entries are available through list_abilities."));

        let visible = json!({"visibility":{"codebase_admin":"visible"}});
        let entries = catalog.catalog_with(&policy, &visible, &state);
        let schemas = catalog.schemas_with(&policy, &json!([]), &visible, &state);
        let index = Catalog::model_index(&entries, &schemas);
        assert!(!index.contains("# [ABILITIES]"));
        assert!(index.contains("- file_read"));
        assert!(!index.contains("- python"));

        let tool_discoverable = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"discoverable"}});
        let entries = catalog.catalog_with(&policy, &tool_discoverable, &state);
        let schemas = catalog.schemas_with(&policy, &json!([]), &tool_discoverable, &state);
        let index = Catalog::model_index(&entries, &schemas);
        assert!(index.contains("## Load on demand"));
        assert!(index.contains("- file_read — discoverable tool in ability codebase_admin"));

        let suppressed = json!({"active":[],"suppressed":["codebase_admin"]});
        assert!(!Catalog::model_index(&catalog.catalog_with(&policy, &visible, &suppressed),
            &catalog.schemas_with(&policy, &json!([]), &visible, &suppressed))
            .contains("codebase_admin"));
        assert!(!Catalog::model_index(&[], &[]).contains("codebase_admin"));
    }

    #[test]
    fn manifest_and_skill_are_loaded_without_exposing_denied_tools() {
        let (_root, catalog) = fixture();
        let policy = policy();
        assert_eq!(catalog.schemas(&policy, &json!([])).len(), 2);
        let mut run = json!({"loaded_abilities":[]});
        let result = catalog.discover(
            "load_ability",
            &json!({"ability_id":"codebase_admin"}),
            &mut run,
            &policy,
        );
        assert_eq!(result["status"], "ok");
        assert_eq!(result["ability_id"], "codebase_admin");
        assert!(result.get("ability").is_none());
        assert!(result.get("skill").is_none());
        assert!(result.get("tool_skills").is_none());
        assert!(!result.to_string().contains("Use scoped workspace tools."));
        assert!(!result.to_string().contains("Match one exact span."));
        assert!(catalog.tool_loaded("file_edit", &run["loaded_abilities"]));
        let names = catalog
            .schemas(&policy, &run["loaded_abilities"])
            .into_iter()
            .map(|schema| schema["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&"file_edit".to_owned()));
        assert!(names.contains(&"state_inspect".to_owned()));
        assert!(names.contains(&"prompt_slot_read".to_owned()));
        assert!(names.contains(&"prompt_slot_write".to_owned()));
        assert!(!names.contains(&"python".to_owned()));
    }

    #[test]
    /// Reject unknown identities while adopting current bytes from the registered source.
    fn configuration_skill_rejects_unknown_and_reads_current_assets() {
        let (root, catalog) = fixture();
        assert_eq!(catalog.configuration_skill("codebase_admin"), Some(Cow::Borrowed("Use scoped workspace tools.")));
        assert_eq!(catalog.configuration_skill("../codebase_admin"), None);
        assert_eq!(catalog.configuration_skill("missing"), None);
        fs::write(root.path().join("Administrator/codebase_admin/codebase_admin.skill.md"), "changed").unwrap();
        assert_eq!(catalog.configuration_skill("codebase_admin").as_deref(),Some("changed"));
    }

    #[test]
    fn duplicate_tools_fail_closed() {
        let (root, _) = fixture();
        let path = root
            .path()
            .join("Administrator/codebase_admin/codebase_admin.json");
        fs::write(
            &path,
            r#"{"display_name":"Codebase Admin","kind":"ability",
            "description":"Run tools","tools":["shell","shell","python"]}"#,
        )
        .unwrap();
        assert!(Catalog::load(root.path()).is_err());
    }

    #[test]
    fn missing_catalog_or_ability_removes_tools_from_runtime() {
        let (root, catalog) = fixture();
        let missing = root.path().join("removed");
        let empty = Catalog::load(&missing).unwrap();
        let visibility = json!({"visibility":{"codebase_admin":"visible"}});
        let state = json!({"active":["codebase_admin"]});
        assert!(empty.configuration_catalog(&policy(), &visibility).is_empty());
        assert!(!empty.tool_available("file_read", &visibility, &state, &Value::Null));
        assert_ne!(empty.version(), catalog.version());
        fs::remove_dir_all(root.path().join("Administrator/codebase_admin")).unwrap();
        assert!(!catalog.tool_available("file_read", &visibility, &state, &Value::Null));
        assert!(catalog.configuration_catalog(&policy(), &visibility).is_empty());
        let partial = Catalog::load(root.path()).unwrap();
        assert!(!partial.tool_available("file_read", &visibility, &state, &Value::Null));
    }

    #[test]
    fn partial_readiness_exposes_only_compiled_tools_and_withdraws_on_edit() {
        let (root, _) = fixture();
        let path = root.path().join("Core/base");
        fs::create_dir_all(&path).unwrap();
        let manifest = path.join("base.json");
        fs::write(&manifest, r#"{"display_name":"Base","kind":"virtual_core",
            "description":"Partial core","readiness":"partial",
            "tools":["calculate","register_webhook"]}"#).unwrap();
        let catalog = Catalog::load(root.path()).unwrap();
        let policy = json!({"rules":{"calculate":"allow","register_webhook":"allow"}});
        let visibility = json!({"visibility":{"base":"visible"}});
        let state = json!({"active":["base"]});
        let item = catalog.configuration_catalog(&policy, &visibility)
            .into_iter().find(|entry| entry["id"] == "base").unwrap();
        assert_eq!(item["readiness"], "partial");
        assert_eq!(item["available"], true);
        assert!(catalog.tool_available("calculate", &visibility, &state, &Value::Null));
        assert!(!catalog.tool_available("register_webhook", &visibility, &state, &Value::Null));
        fs::write(&manifest, r#"{"display_name":"Base","kind":"virtual_core",
            "description":"Disabled","readiness":"pending","tools":["calculate"]}"#).unwrap();
        assert!(!catalog.tool_available("calculate", &visibility, &state, &Value::Null));
        let disabled = Catalog::load(root.path()).unwrap();
        assert!(!disabled.tool_available("calculate", &visibility, &state, &Value::Null));
    }

    #[test]
    fn catalog_only_v2_tool_is_listed_but_never_executable() {
        let (root, _) = fixture();
        let path = root.path().join("Administrator/computer_control");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("computer_control.json"),
            r#"{"display_name":"Computer Control","kind":"ability",
                "description":"Unsupported Computer Control","tools":["computer_screenshot"]}"#).unwrap();
        fs::write(path.join("computer_control.skill.md"), "Control the excluded attached computer.").unwrap();
        let catalog = Catalog::load(root.path()).unwrap();
        let policy = policy();
        let visibility = json!({"visibility":{"computer_control":"visible"}});
        let item = catalog.configuration_catalog(&policy, &visibility)
            .into_iter().find(|item| item["id"] == "computer_control").unwrap();
        assert_eq!(item["available"], false);
        assert_eq!(item["tools"][0]["supported"], false);
        assert!(!catalog.catalog_with(&policy, &visibility, &Value::Null)
            .iter().any(|item| item["id"] == "computer_control"));
        assert!(!catalog.visible_guidance(&policy, &visibility, &Value::Null)
            .contains("Control the excluded attached computer."));
        let mut run = json!({"loaded_abilities":[]});
        assert_eq!(catalog.discover_with("load_ability", &json!({"ability_id":"computer_control"}),
            &mut run, &policy, &visibility, &Value::Null)["status"], "denied");
        assert!(!catalog.tool_available("computer_screenshot", &visibility,
            &json!({"active":["computer_control"],"suppressed":[]}), &Value::Null));
    }

    #[test]
    fn tool_gate_distinguishes_missing_tools_from_hidden_tools() {
        let (_root, catalog) = fixture();
        let state = json!({"active":[],"suppressed":[]});
        let hidden = json!({"visibility":{"codebase_admin":"off"}});
        assert_eq!(catalog.tool_gate("file_read",&hidden,&state,&Value::Null),
            (true,false));
        assert_eq!(catalog.tool_gate("missing",&hidden,&state,&Value::Null),
            (false,false));
        let visible = json!({"visibility":{"codebase_admin":"visible"}});
        assert_eq!(catalog.tool_gate("file_read",&visible,&state,&Value::Null),
            (true,true));
        assert_eq!(catalog.tool_abilities("file_read",&visible,&state,&Value::Null),
            vec!["codebase_admin".to_owned()]);
        assert_eq!(catalog.tool_abilities("file_read",&hidden,&state,&Value::Null),
            vec!["codebase_admin".to_owned()]);
        let disabled_tool = json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"off"}});
        assert_eq!(catalog.tool_gate("file_read",&disabled_tool,&state,&Value::Null),
            (true,false));
    }

    #[test]
    fn catalog_version_changes_with_skill() {
        let (root, catalog) = fixture();
        fs::write(
            root.path()
                .join("Administrator/codebase_admin/codebase_admin.skill.md"),
            "New skill guidance",
        )
        .unwrap();
        assert_ne!(
            catalog.version(),
            Catalog::load(root.path()).unwrap().version()
        );
    }

    #[test]
    fn visible_skills_do_not_depend_on_tool_schema_exposure() {
        let (root,_)=fixture();
        let path=root.path().join("Administrator/codebase_admin/skills");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("ui_theme.skill.md"),"---\nid: ui_theme\ntitle: Theme\nwhen_to_use: Edit UI\ndefault_mode: visible\n---\nUI THEME").unwrap();
        let catalog=Catalog::load(root.path()).unwrap();
        let p=json!({"rules":{"file_read":"allow","file_edit":"allow"}});
        let v=json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_read":"discoverable","file_edit":"discoverable"}});
        let state=json!({"active":[],"suppressed":[]});
        let guidance=catalog.visible_guidance_authorized(&p,&v,&state,|_|true,|_|true,|_|false);
        assert!(guidance.contains("Use scoped workspace tools."));
        assert!(guidance.contains("UI THEME"));
        assert!(!guidance.contains("Match one exact span."));
        let entries=catalog.catalog_with(&p,&v,&state);
        let schemas=catalog.schemas_with(&p,&json!([]),&v,&state);
        let report=catalog.context_inclusion(&entries,&schemas,&v,&state,&json!([]),&json!([]));
        let entry=&report[0];
        assert!(entry.schemas.is_empty());
        assert!(entry.skills.iter().any(|s|s.kind=="ability"&&s.included));
        assert!(entry.skills.iter().any(|s|s.kind=="selectable"&&s.included));
        assert!(entry.skills.iter().filter(|s|s.kind=="tool").all(|s|!s.included));
    }

    #[test]
    fn visible_guidance_and_schema_respect_suppression() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let visibility = json!({"revision":1,"visibility":{"codebase_admin":"visible"}});
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
        let mut run = json!({"loaded_abilities":[]});
        let result = catalog.discover_with(
            "load_ability",
            &json!({"ability_id":"codebase_admin"}),
            &mut run,
            &policy,
            &visibility,
            &suppressed,
        );
        assert_eq!(result["status"], "denied");
        assert!(run["loaded_abilities"].as_array().unwrap().is_empty());
    }

    #[test]
    fn visible_guidance_filters_denied_ability_and_tool_text() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let visibility = json!({"visibility":{"codebase_admin":"visible"}});
        let state = json!({"active":[],"suppressed":[]});
        let normal = catalog.visible_guidance(&policy, &visibility, &state);
        assert!(normal.contains("Use scoped workspace tools."));
        assert!(normal.contains("Match one exact span."));

        let denied_ability = catalog.visible_guidance_filtered(
            &policy, &visibility, &state, |_| false, |_| true);
        assert!(denied_ability.is_empty());

        let denied_tool = catalog.visible_guidance_filtered(
            &policy, &visibility, &state, |_| true, |tool| tool != "file_edit");
        assert!(denied_tool.contains("Use scoped workspace tools."));
        assert!(!denied_tool.contains("Match one exact span."));

        let all_tools_denied = catalog.visible_guidance_filtered(
            &policy, &visibility, &state, |_| true, |_| false);
        assert!(all_tools_denied.is_empty());
    }

    #[test]
    fn persisted_active_discoverable_ability_exposes_schemas() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let state = json!({"revision":1,"active":["codebase_admin"],"suppressed":[]});
        let names = catalog
            .schemas_with(&policy, &json!([]), &Value::Null, &state)
            .into_iter()
            .map(|v| v["name"].as_str().unwrap().to_owned())
            .collect::<Vec<_>>();
        assert!(names.contains(&"file_edit".to_owned()));
        assert!(catalog
            .visible_guidance(&policy, &Value::Null, &state)
            .contains("Use scoped workspace tools."));
        let cleared = json!({"revision":2,"active":[],"suppressed":[]});
        // A retained conversation selection survives an old activation-list reset.
        let selected=json!(["codebase_admin"]);
        assert!(catalog.tool_available("file_edit",&Value::Null,&cleared,&selected));
        assert!(catalog.schemas_with(&policy,&selected,&Value::Null,&cleared)
            .iter().any(|schema|schema["name"]=="file_edit"));
        // Unloading the selection removes its exposure. Explicit suppression also
        // wins while a selected identity remains in the conversation ledger.
        assert!(!catalog.tool_available("file_edit",&Value::Null,&cleared,&json!([])));
        assert!(!catalog.schemas_with(&policy,&json!([]),&Value::Null,&cleared)
            .iter().any(|schema|schema["name"]=="file_edit"));
        let suppressed=json!({"revision":3,"active":[],"suppressed":["codebase_admin"]});
        assert!(!catalog.tool_available("file_edit",&Value::Null,&suppressed,&selected));
        assert!(!catalog.schemas_with(&policy,&selected,&Value::Null,&suppressed)
            .iter().any(|schema|schema["name"]=="file_edit"));
        assert!(catalog.known("codebase_admin"));
        assert!(!catalog.known("unknown"));
    }

    #[test]
    fn agent_off_revokes_loaded_ability_but_keeps_configuration_row() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let visibility = json!({"revision":1,"visibility":{"codebase_admin":"off"}});
        let state = json!({"revision":1,"active":["codebase_admin"],"suppressed":[]});
        assert_eq!(catalog.configuration_catalog(&policy, &visibility)[0]["visibility"], "off");
        assert!(catalog.catalog_with(&policy, &visibility, &state).is_empty());
        assert!(!catalog.tool_available("file_edit", &visibility, &state, &json!(["codebase_admin"])));
        assert!(!catalog.schemas_with(&policy, &json!(["codebase_admin"]), &visibility, &state)
            .iter().any(|schema| schema["name"] == "file_edit"));
        assert!(catalog.visible_guidance(&policy, &visibility, &state).is_empty());
        let mut run = json!({"loaded_abilities":["codebase_admin"]});
        assert_eq!(catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
            &mut run, &policy, &visibility, &state)["status"], "denied");
    }

    #[test]
    fn bundled_skill_mode_is_independent_of_ability_mode() {
        let (_root, catalog) = fixture();
        let policy = policy();
        let state = json!({"active":[],"suppressed":[]});
        let mut visibility = json!({"visibility":{"codebase_admin":"visible"},
            "skill_visibility":{"codebase_admin":"off"}});
        assert!(!catalog.visible_guidance(&policy, &visibility, &state)
            .contains("Use scoped workspace tools."));
        let mut run = json!({"loaded_abilities":[]});
        assert!(catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
            &mut run, &policy, &visibility, &state)["skill"].is_null());
        visibility["skill_visibility"]["codebase_admin"] = json!("discoverable");
        assert!(!catalog.visible_guidance(&policy, &visibility, &state)
            .contains("Use scoped workspace tools."));
        assert_eq!(catalog.discover_with("load_ability", &json!({"ability_id":"codebase_admin"}),
            &mut run, &policy, &visibility, &state)["status"], "already_available");
    }

    // SKILL-LIFETIME-REGRESSION: activation survives transcript folding but not unload.
    #[test]
    fn ability_activation_preserves_independent_skill_visibility() {
        let (_root,catalog)=fixture();let p=policy();
        let v=json!({"visibility":{"codebase_admin":"discoverable"},"skill_visibility":{"codebase_admin":"discoverable"}});
        let active=json!({"active":["codebase_admin"],"suppressed":[]});
        assert!(!catalog.visible_guidance(&p,&v,&active).contains("Use scoped workspace tools."));
        let visible=json!({"visibility":{"codebase_admin":"discoverable"},"skill_visibility":{"codebase_admin":"visible"}});
        assert!(catalog.visible_guidance(&p,&visible,&active).contains("Use scoped workspace tools."));
        let suppressed=json!({"active":[],"suppressed":["codebase_admin"]});
        assert!(!catalog.visible_guidance(&p,&v,&suppressed).contains("Use scoped workspace tools."));
    }

    #[test]
    fn typed_pending_entries_never_expose_tools_or_guidance() {
        let (root, _) = fixture();
        for (id, kind) in [("core_entry", "virtual_core"), ("connection", "oauth"),
            ("chat_channel", "channel"), ("background", "service"), ("future", "placeholder")] {
            let path = root.path().join("Other").join(id);
            fs::create_dir_all(&path).unwrap();
            fs::write(path.join(format!("{id}.json")), serde_json::to_vec(&json!({
                "display_name":id,"description":"Catalog only","kind":kind,
                "readiness":if kind == "placeholder" {"placeholder"} else {"pending"},
                "tools":[], "config":{"settings":[]}
            })).unwrap()).unwrap();
            fs::write(path.join(format!("{id}.skill.md")), "Unsafe v2 instructions").unwrap();
        }
        let catalog = Catalog::load(root.path()).unwrap();
        assert_eq!(catalog.configuration_catalog(&policy(), &Value::Null).len(), 6);
        for id in ["core_entry", "connection", "chat_channel", "background", "future"] {
            let vis = json!({"visibility":{id:"visible"}});
            let mut run = json!({"loaded_abilities":[]});
            assert_eq!(catalog.discover_with("load_ability", &json!({"ability_id":id}),
                &mut run, &policy(), &vis, &Value::Null)["status"], "denied");
            assert_eq!(run["loaded_abilities"], json!([]));
            assert!(!catalog.visible_guidance(&policy(), &vis, &Value::Null).contains("Unsafe v2"));
        }
    }

    #[test]
    fn installed_tool_ownership_and_identifiers_fail_closed() {
        let (root, _) = fixture();
        let path = root.path().join("Other/duplicate");
        fs::create_dir_all(&path).unwrap();
        for tool in ["file_read", "../../escape"] {
            fs::write(path.join("duplicate.json"), serde_json::to_vec(&json!({
                "display_name":"Duplicate","description":"Invalid owner","kind":"ability",
                "tools":[tool]
            })).unwrap()).unwrap();
            assert!(Catalog::load(root.path()).is_err());
        }
    }

    #[test]
    fn unsupported_allow_rule_does_not_expose_catalog_or_skill() {
        let (root, _) = fixture();
        let path = root.path().join("Other/legacy");
        fs::create_dir_all(path.join("tools")).unwrap();
        fs::write(path.join("legacy.json"), r#"{"display_name":"Legacy","description":"Pending","kind":"ability","tools":["v2_only"]}"#).unwrap();
        fs::write(path.join("legacy.skill.md"), "Unsafe guidance").unwrap();
        let catalog = Catalog::load(root.path()).unwrap();
        let p = json!({"rules":{"v2_only":"allow"}});
        let v = json!({"visibility":{"legacy":"visible"}});
        assert!(catalog.catalog_with(&p, &v, &Value::Null).is_empty());
        assert!(catalog.visible_guidance(&p, &v, &Value::Null).is_empty());
    }

    #[test]
    fn malformed_and_oversized_assets_fail_closed() {
        let (root, _) = fixture();
        let path = root.path().join("Administrator/codebase_admin/codebase_admin.json");
        let original = fs::read(&path).unwrap();
        for bytes in [b"{".to_vec(), vec![b' '; MAX_MANIFEST_BYTES as usize + 1]] {
            fs::write(&path, bytes).unwrap();
            assert!(Catalog::load(root.path()).is_err());
        }
        fs::write(&path, original).unwrap();
        let skill = root.path().join("Administrator/codebase_admin/tools/file_edit.skill.md");
        fs::write(&skill, vec![0xff]).unwrap();
        assert!(Catalog::load(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_assets_fail_closed() {
        use std::os::unix::fs::symlink;
        let (root, _) = fixture();
        let skill = root.path().join("Administrator/codebase_admin/tools/file_edit.skill.md");
        fs::remove_file(&skill).unwrap();
        symlink(root.path().join("missing"), &skill).unwrap();
        assert!(Catalog::load(root.path()).is_err());
    }

    #[test]
    fn current_catalog_withdrawal_preserves_strict_snapshot_validation() {
        use std::os::unix::fs::symlink;
        let (root,_) = fixture();
        let path=root.path().join("Administrator/codebase_admin/codebase_admin.json");
        let original=fs::read(&path).unwrap();
        fs::remove_file(&path).unwrap();
        assert!(Catalog::load(root.path()).is_err());
        let current=Catalog::load_current(root.path()).unwrap();
        assert!(!current.abilities.iter().any(|entry|entry.id=="codebase_admin"));
        fs::write(&path,&original).unwrap();
        assert!(Catalog::load_current(root.path()).unwrap().abilities.iter().any(|entry|entry.id=="codebase_admin"));
        fs::write(&path,b"invalid declaration").unwrap();
        assert!(Catalog::load_current(root.path()).is_err());
        fs::remove_file(&path).unwrap();
        symlink(root.path().join("missing"),&path).unwrap();
        assert!(Catalog::load_current(root.path()).is_err());
    }

    #[test]
    fn selected_tool_markers_preserve_off_and_suppressed_controls() {
        let catalog=crate::runtime_tests::native_catalog();
        let loaded=json!(["tool:file_read"]);
        let mut visibility=json!({"visibility":{"codebase_admin":"discoverable"},
            "tool_modes":{"file_read":"discoverable"}});
        let state=json!({"active":[],"suppressed":[]});
        assert!(catalog.tool_available("file_read",&visibility,&state,&loaded));
        visibility["visibility"]["codebase_admin"]=json!("off");
        assert!(!catalog.tool_available("file_read",&visibility,&state,&loaded));
        visibility["visibility"]["codebase_admin"]=json!("discoverable");
        assert!(!catalog.tool_available("file_read",&visibility,&json!({"active":[],"suppressed":["codebase_admin"]}),&loaded));
        assert_eq!(state,json!({"active":[],"suppressed":[]}));
    }

    #[test]
    fn code_index_comment_maintenance_skill_is_discoverable() {
        let catalog = crate::runtime_tests::native_catalog();
        let visibility = json!({"visibility":{"code_index":"visible"}});
        let active = json!({"active":["code_index"],"suppressed":[]});
        let listed = catalog.bundled_skill_list("code_index", &visibility, &active,
            |_| true, |_| true).unwrap();
        assert!(listed.iter().any(|skill| skill["kind"] == "selectable"
            && skill["skill_id"] == "comment_maintenance"));
        let loaded = catalog.bundled_skill_load("code_index", "selectable", "comment_maintenance",
            &visibility, &active, |_| true, |_| true).unwrap();
        assert_eq!(loaded["source_kind"], "untrusted_bundled_guidance");
        assert_eq!(loaded["activates"], false);
        assert!(loaded["text"].as_str().unwrap().contains("chat footer"));
    }

    /// Verify native seed inventory against the tools actually registered by the current host.
    #[test]
    fn production_seed_manifest_loads() {
        let catalog = crate::runtime_tests::native_catalog();
        let ordinary = catalog.abilities.iter()
            .filter(|entry| !matches!(entry.id.as_str(),"optimization"|"mcp")).collect::<Vec<_>>();
        assert_eq!(ordinary.len(), 81);
        assert_eq!(catalog.abilities.iter().filter(|entry|entry.id=="mcp").count(),1);
        assert!(!catalog.known("mcp"));
        assert_eq!(catalog.abilities.iter().filter(|entry| entry.id == "optimization").count(), 1);
        assert!(!catalog.known("optimization"));
        assert_eq!(catalog.abilities.iter().filter(|entry| Catalog::executable(entry)).count(), 9);
        assert_eq!(ordinary.iter().filter(|entry| entry.readiness == Readiness::Ready).count(), 8);
        assert_eq!(catalog.abilities.iter().filter(|entry| entry.readiness == Readiness::Partial).count(), 2);
        let automation=catalog.abilities.iter().find(|entry|entry.id=="automation").unwrap();
        assert_eq!(automation.required_module.as_deref(),Some("automation"));
        assert!(!Catalog::executable(automation));
        assert!(catalog.abilities.iter().any(|entry| entry.id == "decision" && entry.tools == ["decide_with_jev", "score_directory_with_jev"]));
        let base = catalog.abilities.iter().find(|entry| entry.id == "base").unwrap();
        let mut installed: Vec<_> = base.tools.iter().filter(|name| Catalog::supported(name)).map(String::as_str).collect();
        installed.sort();
        assert_eq!(installed, ["calculate", "compact_context", "context_assets", "edit_own_prompt", "get_date", "get_task_checkpoint", "get_time", "list_skills", "list_tools", "load_skill", "load_tool", "present_chat_component", "present_plan", "read_attachment", "read_own_prompt", "recall_compacted", "remove_own_skill", "save_own_skill", "search_this_session", "search_tools", "session_search", "set_execution_mode", "similar_sessions", "task_cancel", "task_status", "task_wait", "update_task_checkpoint"]);
        assert!(!catalog.abilities.iter().any(|ability| matches!(ability.id.as_str(), "v3_build"|"v3_workspace")));
        let coding=catalog.abilities.iter().find(|ability| ability.id=="codebase_admin").unwrap();
        assert_eq!(coding.tools, ["file_list","file_read","file_search","file_edit","file_write",
            "build_work","state_inspect","prompt_slot_read","prompt_slot_write","shell","python"]);
        assert!(catalog.abilities.iter().any(|ability| ability.id == "diagnostics"));
        assert!(catalog.abilities.iter().any(|ability| ability.id == "terminal_control"));
    }
    #[test]
    fn web_access_loading_policy_and_visibility_gates() {
        let catalog=crate::runtime_tests::native_catalog();
        let mut p=crate::policy_default();
        let mut v=json!({"visibility":{"web_access":"discoverable"}});
        let mut run=json!({"loaded_abilities":[]});
        let state=json!({"active":[],"suppressed":[]});
        assert!(!catalog.tool_available("maps_geocode",&v,&state,&json!([])));
        let result=catalog.discover_with("load_ability",&json!({"ability_id":"web_access"}),&mut run,&p,&v,&state);
        assert_eq!(result["status"],"ok");
        assert_eq!(result["ability_id"],"web_access");
        assert!(result.get("tool_skills").is_none());
        assert!(!result.to_string().contains("great-circle"));
        let active=json!({"active":["web_access"],"suppressed":[]});
        assert!(catalog.tool_available("maps_geocode",&v,&active,&run["loaded_abilities"]));
        assert_eq!(catalog.tool_abilities("maps_geocode",&v,&active,&run["loaded_abilities"]),vec!["web_access"]);
        p["rules"]["web_search"]=json!("deny");
        assert!(!catalog.schemas_with(&p,&run["loaded_abilities"],&v,&active).iter().any(|s|s["name"]=="web_search"));
        v["visibility"]["web_access"]=json!("off");
        assert!(!catalog.tool_available("maps_geocode",&v,&active,&run["loaded_abilities"]));
    }

}


#[cfg(test)]
mod optional_module_tests {
    use super::*;
    #[test]
    fn absent_optional_module_loads_without_exposing_execution() {
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
        let catalog = Catalog::load(root.path()).unwrap();
        let entry = catalog
            .abilities
            .iter()
            .find(|a| a.id == "automation")
            .unwrap();
        assert!(!crate::modules::available("automation"));
        assert!(!Catalog::executable(entry));
        assert!(!catalog.tool_available(
            "schedule_task",
            &json!({"visibility":{"automation":"visible"}}),
            &json!({"active":["automation"]}),
            &Value::Null
        ));
    }
}

#[cfg(test)]
mod optional_settings_schema_tests {
    use super::*;
    use super::tests::{fixture,policy};
    fn catalog() -> Catalog {
        crate::runtime_tests::native_catalog()
    }
    fn defaults(catalog: &Catalog) -> Value {
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
    #[test]
    fn optional_schema_enforces_full_declared_types_keys_and_numeric_bounds() {
        let catalog = catalog();
        let settings = defaults(&catalog);
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
            None
        );
    }
    #[test]
    fn malformed_optional_declarations_fail_closed() {
        for mutation in [
            "duplicate",
            "unknown-type",
            "invalid-default",
            "invalid-bound",
            "reversed-bounds",
            "missing-default",
        ] {
            let mut catalog = catalog();
            let settings = defaults(&catalog);
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

    #[test]
    /// Adopt selected current text while preserving source, mode, permission and schema boundaries.
    fn capability_guidance_reads_selected_live_sources_and_withdraws_invalid_or_denied_assets() {
        let (root,_)=fixture();
        let folder=root.path().join("Administrator/codebase_admin");
        fs::create_dir_all(folder.join("skills")).unwrap();
        let skill=|body:&str|format!("---\nid: verification\ntitle: Verification\nwhen_to_use: Review implementation\ndefault_mode: discoverable\n---\n{body}");
        let path=folder.join("skills/verification.skill.md");
        fs::write(&path,skill("FIRST BODY")).unwrap();
        let catalog=Catalog::load_snapshot(root.path()).unwrap();
        let visible=json!({"visibility":{"codebase_admin":"visible"},
            "settings":{"codebase_admin":{"terminal_caution":"cautious"}}});
        let state=json!({"active":[],"suppressed":[]});
        let entries=catalog.catalog_with(&policy(),&visible,&state);
        let schemas=vec![json!({"name":"load_skill"}),json!({"name":"file_edit"})];
        let selected=json!([{"ability_id":"codebase_admin","kind":"selectable","skill_id":"verification","sha256":"obsolete"}]);
        let read=||catalog.capability_guidance(&entries,&schemas,&visible,&state,&Value::Null,&selected);
        let first=read();
        assert!(first.iter().any(|item|item.id=="ability:codebase_admin" && item.natural
            && item.body=="Use scoped workspace tools."));
        assert!(first.iter().any(|item|item.id=="ability:codebase_admin:settings" && item.natural
            && item.body.contains("Configured terminal_caution: cautious")));
        let mut hidden_core=visible.clone();hidden_core["skill_visibility"]=json!({"codebase_admin":"discoverable"});
        let hidden=catalog.capability_guidance(&entries,&schemas,&hidden_core,&state,&Value::Null,&selected);
        assert!(hidden.iter().any(|item|item.id=="ability:codebase_admin" && !item.natural && !item.selected));
        assert!(hidden.iter().any(|item|item.id=="ability:codebase_admin:settings" && item.natural));
        hidden_core["skill_visibility"]["codebase_admin"]=json!("off");
        let off_core=catalog.capability_guidance(&entries,&schemas,&hidden_core,&state,&Value::Null,&selected);
        assert!(!off_core.iter().any(|item|item.id=="ability:codebase_admin"));
        assert!(off_core.iter().any(|item|item.id=="ability:codebase_admin:settings" && item.natural));
        assert!(first.iter().any(|item|item.id=="tool:codebase_admin:file_edit" && item.natural));
        assert!(first.iter().any(|item|item.id=="selectable:codebase_admin:verification"
            && item.selected && !item.natural && item.body==skill("FIRST BODY")));
        let version=catalog.live_version();
        fs::write(&path,skill("CURRENT BODY")).unwrap();
        assert_ne!(catalog.live_version(),version);
        let current=read();
        assert!(current.iter().any(|item|item.selected && item.body==skill("CURRENT BODY")));
        assert!(!current.iter().any(|item|item.body.contains("FIRST BODY")));
        fs::write(&path,"Missing required selectable metadata").unwrap();
        assert!(!read().iter().any(|item|item.id.starts_with("selectable:")));
        fs::remove_file(&path).unwrap();
        assert!(!read().iter().any(|item|item.id.starts_with("selectable:")));
        fs::write(&path,skill("REAPPEARED BODY")).unwrap();
        assert!(read().iter().any(|item|item.selected && item.body==skill("REAPPEARED BODY")));
        let mut off=visible.clone();off["skill_visibility"]=json!({"codebase_admin:verification":"off"});
        assert!(!catalog.capability_guidance(&entries,&schemas,&off,&state,&Value::Null,&selected)
            .iter().any(|item|item.id.starts_with("selectable:")));
        assert!(catalog.capability_guidance(&entries,&schemas,&visible,
            &json!({"suppressed":["codebase_admin"]}),&Value::Null,&selected).is_empty());
        assert!(catalog.capability_guidance(&[],&schemas,&visible,&state,&Value::Null,&selected).is_empty());
        let without_load=catalog.capability_guidance(&entries,&[json!({"name":"file_edit"})],
            &visible,&state,&Value::Null,&selected);
        assert!(without_load.iter().filter(|item|item.id.starts_with("selectable:"))
            .all(|item|!item.selected && !item.natural));
        let no_tools=catalog.capability_guidance(&entries,&[],&visible,&state,&Value::Null,&selected);
        assert!(no_tools.iter().any(|item|item.id=="ability:codebase_admin" && item.natural));
        assert!(no_tools.iter().filter(|item|item.id.starts_with("tool:")).all(|item|!item.natural));
    }

    #[test]
    /// Prevent retained identities from reviving denied or Off tool guidance.
    fn capability_guidance_filters_current_authorized_tool_names_and_modes() {
        let (_root,catalog)=fixture();
        let visible=json!({"visibility":{"codebase_admin":"visible"},
            "tool_modes":{"file_edit":"discoverable"}});
        let state=json!({});
        let mut entries=catalog.catalog_with(&policy(),&visible,&state);
        let schemas=vec![json!({"name":"file_edit"})];
        let selected=json!(["tool:file_edit"]);
        let body=catalog.capability_guidance(&entries,&schemas,&visible,&state,&selected,&Value::Null);
        assert!(body.iter().any(|item|item.id=="tool:codebase_admin:file_edit" && item.natural));
        entries[0]["tools"]=json!([{"name":"file_read"}]);
        assert!(!catalog.capability_guidance(&entries,&schemas,&visible,&state,&selected,&Value::Null)
            .iter().any(|item|item.id=="tool:codebase_admin:file_edit"));
        let mut off=visible;off["tool_modes"]["file_edit"]=json!("off");
        let entries=catalog.catalog_with(&policy(),&off,&state);
        assert!(!catalog.capability_guidance(&entries,&schemas,&off,&state,&selected,&Value::Null)
            .iter().any(|item|item.id=="tool:codebase_admin:file_edit"));
    }


    #[test]
    /// Project identities without changing saved state or overriding restrictions.
    fn selected_state_is_call_local_and_preserves_off_and_suppression() {
        let (_root,catalog)=fixture();
        let visibility=json!({"visibility":{"codebase_admin":"discoverable"}});
        let state=json!({"active":["another"],"suppressed":[],"revision":7});
        let original=state.clone();
        let loaded=json!(["codebase_admin","codebase_admin","unknown","tool:file_edit"]);
        let projected=catalog.selected_state(&visibility,&state,&loaded);
        assert_eq!(state,original);
        assert_eq!(projected["active"],json!(["another","codebase_admin"]));
        assert_eq!(projected["revision"],7);
        assert_eq!(catalog.tool_gate("file_edit",&visibility,&state,&loaded),(true,true));
        let tool_discoverable=json!({"visibility":{"codebase_admin":"discoverable"},"tool_modes":{"file_edit":"discoverable"}});
        assert_eq!(catalog.tool_gate("file_edit",&tool_discoverable,&state,&json!(["codebase_admin"])),(true,false));
        assert_eq!(catalog.selected_state(&visibility,&projected,&loaded),projected);
        let off=json!({"visibility":{"codebase_admin":"off"}});
        assert_eq!(catalog.selected_state(&off,&state,&loaded),state);
        assert_eq!(catalog.tool_gate("file_edit",&off,&state,&loaded),(true,false));
        let suppressed=json!({"active":[],"suppressed":["codebase_admin"]});
        assert_eq!(catalog.selected_state(&visibility,&suppressed,&loaded),suppressed);
        assert_eq!(catalog.tool_gate("file_edit",&visibility,&suppressed,&loaded),(true,false));
        assert_eq!(catalog.selected_state(&visibility,&state,&json!(["tool:file_edit"])),state);
        assert_eq!(catalog.selected_state(&visibility,&state,&json!(["unknown"])),state);
    }

}
