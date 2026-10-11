use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
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
    group: String,
    group_name: String,
    group_order: i32,
    name: String,
    description: String,
    skill_summary: Option<String>,
    migration_note: Option<String>,
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
    abilities: Vec<Ability>,
    version: String,
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
    pub fn load(root: &Path) -> Result<Self> {
        let mut abilities = Vec::new();
        let mut ids = HashSet::new();
        let mut owners = BTreeMap::new();
        if root.exists() && fs::symlink_metadata(root)?.file_type().is_symlink() { bail!("Symlink catalog root"); }
        let groups = match fs::read_dir(root) {
            Ok(groups) => groups,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let version = hex::encode(Sha256::digest(b"[]"));
                return Ok(Self { abilities, version });
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
                let ability_path = folder.path();
                // Pin exactly the bytes parsed here; live edits withdraw cached authority
                // until a freshly validated catalog reload adopts the new declaration.
                // Retired coding abilities keep their backend files for operator
                // commands and durable recovery, but are no longer catalog entries.
                if group_id == "Administrator" && matches!(id.as_str(), "v3_build"|"v3_workspace")
                    && !ability_path.join(format!("{id}.json")).exists() { continue; }
                let manifest_bytes = read_bytes(&ability_path.join(format!("{id}.json")), MAX_MANIFEST_BYTES)?;
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
                let readiness = manifest.readiness.clone().unwrap_or_else(|| {
                    if manifest.placeholder || manifest.kind == EntryKind::Placeholder { Readiness::Placeholder }
                    else if manifest.tools.iter().any(|tool| Self::supported(tool)) { Readiness::Ready }
                    else { Readiness::Pending }
                });
                if (manifest.placeholder || manifest.kind == EntryKind::Placeholder)
                    && (readiness != Readiness::Placeholder || !manifest.tools.is_empty()) {
                    bail!("Invalid placeholder: {id}");
                }
                if matches!(readiness, Readiness::Ready | Readiness::Partial) &&
                    (!matches!(manifest.kind, EntryKind::Ability | EntryKind::VirtualCore) ||
                     !manifest.tools.iter().any(|tool| Self::supported(tool))) {
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
                    if Self::supported(tool) && owners.insert(tool.clone(), id.clone()).is_some() {
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
                    group: group_id.clone(),
                    group_name: group_name.clone(),
                    group_order: group_meta.order,
                    name: manifest.display_name,
                    description: manifest.description,
                    skill_summary: manifest.skill_summary,
                    migration_note: manifest.migration_note,
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
        abilities.sort_by(|a, b| {
            (a.group_order, &a.group_name, a.order, &a.name, &a.id).cmp(&(
                b.group_order,
                &b.group_name,
                b.order,
                &b.name,
                &b.id,
            ))
        });
        let version = hex::encode(Sha256::digest(serde_json::to_vec(&abilities)?));
        Ok(Self { abilities, version })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    fn present(ability: &Ability) -> bool {
        // Presence alone cannot retain an old executable declaration after an
        // operator disables or edits its manifest. Unreadable/changed bytes fail closed.
        [ability.path.as_path(), ability.path.parent().unwrap_or(ability.path.as_path()),
            ability.path.parent().and_then(Path::parent).unwrap_or(ability.path.as_path())]
            .into_iter().all(|path|fs::symlink_metadata(path).is_ok_and(|m|m.file_type().is_dir()))
            && read_bytes(&ability.path.join(format!("{}.json", ability.id)), MAX_MANIFEST_BYTES)
                .is_ok_and(|bytes| hex::encode(Sha256::digest(&bytes)) == ability.manifest_digest)
    }

    // Only cached, validated bundle IDs can select an asset. Live edits withdraw
    // old guidance instead of importing instructions without a catalog reload.
    fn skill_live(ability: &Ability, kind: &str, name: &str, text: &str) -> bool {
        let path = match kind {
            "ability" if name == ability.id => ability.path.join(format!("{name}.skill.md")),
            "tool" if valid_id(name) => ability.path.join("tools").join(format!("{name}.skill.md")),
            "selectable" if valid_id(name) => ability.path.join("skills").join(format!("{name}.skill.md")),
            _ => return false,
        };
        path.parent().is_some_and(|parent|
            fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_dir()))
            && read_bytes(&path, text.len() as u64).is_ok_and(|bytes| bytes == text.as_bytes())
    }

    /// Bounded validity fingerprint of cached manifests and skill assets only.
    /// A changed file cannot become trusted guidance; it merely invalidates the
    /// run-local schema/guidance cache until a validated reload adopts its bytes.
    pub fn live_version(&self) -> String {
        let validity = self.abilities.iter().map(|ability| {
            let present = Self::present(ability);
            let mut assets = Vec::new();
            if let Some(text) = &ability.skill {
                assets.push(json!(["ability", ability.id,
                    present && Self::skill_live(ability, "ability", &ability.id, text)]));
            }
            for (name, text) in &ability.tool_skills {
                assets.push(json!(["tool", name, present && Self::skill_live(ability, "tool", name, text)]));
            }
            for (name, text) in &ability.selectable_skills {
                assets.push(json!(["selectable", name, present && Self::skill_live(ability, "selectable", name, text)]));
            }
            json!([ability.id, present, assets])
        }).collect::<Vec<_>>();
        hex::encode(Sha256::digest(serde_json::to_vec(&json!([self.version, validity]))
            .expect("Cached catalog validity is serializable")))
    }

    fn bundled_candidates<'a>(
        &'a self, ability_id: &str, visibility: &Value, state: &Value,
        allow_ability: &impl Fn(&str) -> bool, allow_tool: &impl Fn(&str) -> bool,
    ) -> Result<(&'a Ability, Vec<(&'static str, &'a str, &'a String)>)> {
        if !valid_id(ability_id) { bail!("Invalid bundled ability ID"); }
        let ability = self.abilities.iter().find(|a| a.id == ability_id)
            .filter(|a| Self::available(a, visibility, state)
                && allow_ability(ability_id)
                && a.tools.iter().any(|tool| Self::supported(tool)
                    && Self::tool_mode(visibility, tool) != "off" && allow_tool(tool)))
            .context("Bundled skills unavailable under current access")?;
        let mut assets = Vec::new();
        if Self::skill_mode(visibility, ability_id) != "off" {
            if let Some(text) = &ability.skill { assets.push(("ability", ability.id.as_str(), text)); }
        }
        for (name, text) in &ability.tool_skills {
            if Self::supported(name) && Self::tool_mode(visibility, name) != "off" && allow_tool(name) {
                assets.push(("tool", name.as_str(), text));
            }
        }
        for (name, text) in &ability.selectable_skills {
            if Self::selectable_metadata(name,text).is_some()
                && Self::selectable_mode(visibility, ability_id, name, text) != "off" {
                assets.push(("selectable", name.as_str(), text));
            }
        }
        assets.retain(|(kind, name, text)| Self::skill_live(ability, kind, name, text));
        Ok((ability, assets))
    }

    pub(crate) fn bundled_skill_list(
        &self, ability_id: &str, visibility: &Value, state: &Value,
        allow_ability: impl Fn(&str) -> bool, allow_tool: impl Fn(&str) -> bool,
    ) -> Result<Vec<Value>> {
        let (ability, assets) = self.bundled_candidates(ability_id, visibility, state, &allow_ability, &allow_tool)?;
        Ok(assets.into_iter().map(|(kind, name, text)| {
            let mode = Self::asset_mode(visibility, ability, kind, name, text);
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

    fn asset_mode<'a>(visibility: &'a Value, ability: &Ability, kind: &str, name: &str, text: &str) -> &'a str {
        match kind {
            "ability" => Self::skill_mode(visibility,&ability.id),
            "tool" => Self::tool_mode(visibility,name),
            _ => Self::selectable_mode(visibility,&ability.id,name,text),
        }
    }

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
            "mode":Self::asset_mode(visibility,ability,kind,skill_id,text),
            "availability":if Self::asset_mode(visibility,ability,kind,skill_id,text)=="visible" {"visible"} else {"discoverable"},
            "requires_activation":Self::asset_mode(visibility,ability,kind,skill_id,text)!="visible",
            "bytes":text.len(),"sha256":hex::encode(Sha256::digest(text.as_bytes())),
            "text":text,"source_kind":"untrusted_bundled_guidance","activates":false}))
    }

    fn executable(ability: &Ability) -> bool {
        Self::present(ability) && matches!(ability.readiness, Readiness::Ready | Readiness::Partial)
            && matches!(ability.kind, EntryKind::Ability | EntryKind::VirtualCore)
    }

    pub fn ids(&self) -> Vec<String> {
        self.abilities.iter().filter(|ability| Self::present(ability))
            .map(|ability| ability.id.clone()).collect()
    }

    pub fn known(&self, id: &str) -> bool {
        self.abilities.iter().any(|ability| ability.id == id && Self::present(ability))
    }

    fn supported(name: &str) -> bool {
        crate::tools::schemas().iter().any(|schema| schema["name"] == name)
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

    fn describe(ability: &Ability, policy: &Value, visibility: &Value, state: &Value) -> Value {
        let tools = ability
            .tools
            .iter()
            .filter(|name| Self::executable(ability) && Self::supported(name) && Self::allowed(policy, name) && Self::tool_mode(visibility, name) != "off")
            .map(|name| json!({"name":name,"policy":policy["rules"][name],
                        "mode":Self::tool_mode(visibility,name),"supported":Self::supported(name)}))
            .collect::<Vec<_>>();
        let visible = Self::is_visible(visibility, &ability.id);
        let off = Self::is_off(visibility, &ability.id);
        let mut general_skills=Vec::new();
        if ability.skill.as_ref().is_some_and(|text| Self::skill_live(ability,"ability",&ability.id,text)) {
            general_skills.push(json!({"skill_id":"core","title":"Skill","when_to_use":ability.skill_summary,
                "mode":Self::skill_mode(visibility,&ability.id)}));
        }
        for (name,text) in &ability.selectable_skills {
            if !Self::skill_live(ability,"selectable",name,text) {continue;}
            if let Some((title,when,_))=Self::selectable_metadata(name,text) {
                general_skills.push(json!({"skill_id":name,"title":title,"when_to_use":when,
                    "mode":Self::selectable_mode(visibility,&ability.id,name,text)}));
            }
        }
        json!({"id":ability.id,"group":ability.group,"group_name":ability.group_name,
            "name":ability.name,"description":ability.description,
            "kind":ability.kind,"readiness":ability.readiness,"config":ability.config,
            "skill_summary":ability.skill_summary,"migration_note":ability.migration_note,
            "icon":ability.icon,"color":ability.color,"tools":tools,
            "has_skill":ability.skill.as_ref().is_some_and(|text| Self::skill_live(ability, "ability", &ability.id, text)),
            "selectable_skills":ability.selectable_skills.iter().filter(|(name, text)| Self::skill_live(ability, "selectable", name, text) && Self::selectable_metadata(name,text).is_some() && Self::selectable_mode(visibility,&ability.id,name,text)!="off").map(|(name, _)| name).collect::<Vec<_>>(),
            "general_skills":general_skills,
            "skill_visibility":Self::skill_mode(visibility, &ability.id),
            "visibility":if off {"off"} else if visible {"visible"} else {"discoverable"},
            "active":Self::active(state, &ability.id),
            "availability":if visible {"visible"} else if Self::active(state, &ability.id) {"activated"} else {"discoverable"},
            "requires_activation":!visible && !Self::active(state, &ability.id)})
    }

    pub fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value> {
        self.abilities
            .iter()
            .filter(|ability| {
                Self::executable(ability)
                    && !Self::is_off(visibility, &ability.id)
                    && !Self::suppressed(state, &ability.id)
                    && ability.tools.iter().any(|name| Self::supported(name) && Self::allowed(policy, name) && Self::tool_mode(visibility,name) != "off")
            })
            .map(|ability| Self::describe(ability, policy, visibility, state))
            .collect()
    }

    pub fn configuration_catalog(&self, policy: &Value, visibility: &Value) -> Vec<Value> {
        self.abilities
            .iter()
            .filter(|ability| Self::present(ability))
            .map(|ability| {
                let mut item = Self::describe(ability, policy, visibility, &Value::Null);
                item["available"] = json!(Self::executable(ability) && ability.tools.iter().any(|name| Self::supported(name)));
                item["tools"] = json!(ability.tools.iter().map(|name| {
                    let description=crate::tools::schemas().iter().find(|schema|schema["name"]==*name)
                        .and_then(|schema|schema["description"].as_str()).unwrap_or("").to_owned();
                    json!({"name":name,"policy":policy["rules"][name],
                        "mode":Self::tool_mode(visibility,name),"supported":Self::supported(name),
                        "description":description,
                        "has_tool_skill":ability.tool_skills.get(name)
                            .is_some_and(|text|Self::skill_live(ability,"tool",name,text))})
                }).collect::<Vec<_>>());
                item
            })
            .collect()
    }

    /// Administrator viewer projection: only validated, unchanged bundle guidance.
    /// This does not activate the ability or alter model-facing visibility.
    pub fn configuration_skill(&self, id: &str) -> Option<&str> {
        let ability = self.abilities.iter().find(|ability| ability.id == id && Self::present(ability))?;
        ability.skill.as_deref().filter(|text| Self::skill_live(ability, "ability", id, text))
    }

    pub fn configuration_skill_asset(&self, id: &str, kind: &str, name: &str) -> Option<&str> {
        let ability = self.abilities.iter().find(|a| a.id == id && Self::present(a))?;
        let text = match kind {
            "skill" if valid_id(name) => ability.selectable_skills.get(name)
                .filter(|text| Self::selectable_metadata(name,text).is_some()),
            "tool" if valid_id(name) && ability.tools.iter().any(|tool| tool == name) => ability.tool_skills.get(name),
            _ => None,
        }?;
        Self::skill_live(ability, if kind == "skill" {"selectable"} else {"tool"}, name, text)
            .then_some(text.as_str())
    }

    /// Installed owners of a tool, independent of visibility or activation.
    /// A shared tool name must meet every owning ability permission.
    pub fn tool_abilities(&self,name:&str,_visibility:&Value,_state:&Value,
        _loaded:&Value) -> Vec<String> {
        if !Self::supported(name) {return Vec::new();}
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
        let supported = Self::supported(name);
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
                && (Self::available(ability, visibility, state)
                    || (selected && !Self::is_off(visibility,&ability.id)
                        && !Self::suppressed(state,&ability.id)));
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
        schemas.extend(crate::tools::schemas().into_iter().filter(|tool| {
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
                    Self::supported(tool) && Self::allowed(policy, tool) && eligible_tool(tool)
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
                if let Some(skill) = ability.skill.as_ref().filter(|text| Self::skill_live(ability, "ability", &ability.id, text)) {
                    part.push_str("\n");
                    part.push_str(skill);
                }
            }
            for (tool, skill) in &ability.tool_skills {
                if Self::supported(tool) && Self::allowed(policy, tool)
                    && supplied_tool(tool)
                    && Self::skill_live(ability, "tool", tool, skill)
                    && Self::tool_mode(visibility, tool) == "visible" {
                    part.push_str("\n\nTool ");
                    part.push_str(tool);
                    part.push_str(":\n");
                    part.push_str(skill);
                }
            }
            for (name, skill) in &ability.selectable_skills {
                if Self::selectable_mode(visibility, &ability.id, name, skill) == "visible"
                    && Self::skill_live(ability, "selectable", name, skill) {
                    part.push_str("\n\nSkill ");
                    part.push_str(name);
                    part.push_str(":\n");
                    part.push_str(skill);
                }
            }
            // Administrator-selected guidance is mandatory; assembly budgets the complete request.
            text.push_str(&part);
        }
        text
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
                    item["ability_id"]==id && item["kind"]==kind && item["skill_id"]==name
                        && item["sha256"]==hex::encode(Sha256::digest(body.as_bytes()))));
                let included = natural || explicit;
                let reason = if included {"included"} else if mode=="off" {"off"}
                    else if mode=="discoverable" || entry["availability"]=="discoverable" {"discoverable"}
                    else {"unavailable"};
                skills.push(SkillInclusion {kind:kind.into(),id:name.into(),mode:mode.into(),included,
                    content_bytes:if included {body.len()} else {0},reason:reason.into()});
            };
            if let Some(body)=ability.skill.as_ref().filter(|body|Self::skill_live(ability,"ability",id,body)) {
                let mode=Self::skill_mode(visibility,id);
                add("ability",id,mode,body,mode=="visible"&&(normal||extra));
            }
            for (name,body) in &ability.tool_skills {
                if names.contains(name.as_str()) && Self::skill_live(ability,"tool",name,body) {
                    let mode=Self::tool_mode(visibility,name);
                    add("tool",name,mode,body,sent.contains(name.as_str())
                        && ((normal&&mode=="visible") || (mode!="off"&&selected(name))));
                }
            }
            for (name,body) in &ability.selectable_skills {
                if Self::selectable_metadata(name,body).is_some() && Self::skill_live(ability,"selectable",name,body) {
                    let mode=Self::selectable_mode(visibility,id,name,body);
                    add("selectable",name,mode,body,normal&&mode=="visible");
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
                let ability = Self::describe(entry, policy, visibility, state);
                if ability["tools"].as_array().is_none_or(Vec::is_empty) {
                    return json!({"status":"denied","output":"Ability unavailable under current policy"});
                }
                if Self::is_visible(visibility, id) || Self::active(state, id) {
                    return json!({"status":"already_available","ability":ability,
                        "message":"This ability is already available. Call its visible tools directly; use list_tools/load_tool or search_tools for discoverable tools and list_skills/load_skill for discoverable guidance."});
                }
                if !run["loaded_abilities"].is_array() {
                    run["loaded_abilities"] = json!([]);
                }
                let loaded = run["loaded_abilities"].as_array_mut().unwrap();
                if !loaded.iter().any(|item| item.as_str() == Some(id)) {
                    loaded.push(json!(id));
                }
                let permitted_skills = entry
                    .tool_skills
                    .iter()
                    .filter(|(tool, text)| Self::supported(tool) && Self::allowed(policy, tool) && Self::tool_mode(visibility, tool) == "visible"
                        && Self::skill_live(entry, "tool", tool, text))
                    .collect::<BTreeMap<_, _>>();
                json!({"status":"ok","ability":ability,
                    "skill":if Self::skill_mode(visibility, id) != "visible" {None} else {entry.skill.as_ref().filter(|text| Self::skill_live(entry, "ability", &entry.id, text))},
                    "tool_skills":permitted_skills,
                    "selectable_skills":entry.selectable_skills.iter().filter(|(name, text)| Self::skill_live(entry, "selectable", name, text) && Self::selectable_metadata(name,text).is_some() && Self::selectable_mode(visibility,id,name,text)!="off").map(|(name, _)| name).collect::<Vec<_>>(),
                    "catalog_version":self.version})
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

    fn fixture() -> (TempDir, Catalog) {
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

    fn policy() -> Value {
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
        assert_eq!(result["ability"]["group_name"], "Administrator");
        assert_eq!(result["skill"], "Use scoped workspace tools.");
        assert_eq!(result["tool_skills"]["file_edit"], "Match one exact span.");
        assert!(result["tool_skills"]["python"].is_null());
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
    fn configuration_skill_rejects_unknown_and_changed_assets() {
        let (root, catalog) = fixture();
        assert_eq!(catalog.configuration_skill("codebase_admin"), Some("Use scoped workspace tools."));
        assert_eq!(catalog.configuration_skill("../codebase_admin"), None);
        assert_eq!(catalog.configuration_skill("missing"), None);
        fs::write(root.path().join("Administrator/codebase_admin/codebase_admin.skill.md"), "changed").unwrap();
        assert_eq!(catalog.configuration_skill("codebase_admin"), None);
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
        assert!(!catalog.tool_available(
            "file_edit",
            &Value::Null,
            &cleared,
            &json!(["codebase_admin"])
        ));
        let revoked =
            catalog.schemas_with(&policy, &json!(["codebase_admin"]), &Value::Null, &cleared);
        assert!(!revoked.iter().any(|schema| schema["name"] == "file_edit"));
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
    fn selected_tool_markers_preserve_off_and_suppressed_controls() {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities");
        let catalog=Catalog::load(&root).unwrap();
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
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities");
        let catalog = Catalog::load(&root).unwrap();
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

    #[test]
    fn production_seed_manifest_loads() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities");
        let catalog = Catalog::load(&root).unwrap();
        assert_eq!(catalog.abilities.len(), 81);
        assert_eq!(catalog.abilities.iter().filter(|entry| Catalog::executable(entry)).count(), 9);
        assert_eq!(catalog.abilities.iter().filter(|entry| entry.readiness == Readiness::Ready).count(), 8);
        assert_eq!(catalog.abilities.iter().filter(|entry| entry.readiness == Readiness::Partial).count(), 1);
        assert!(catalog.abilities.iter().any(|entry| entry.id == "decision" && entry.tools == ["decide_with_jev", "score_directory_with_jev"]));
        let base = catalog.abilities.iter().find(|entry| entry.id == "base").unwrap();
        let mut installed: Vec<_> = base.tools.iter().filter(|name| Catalog::supported(name)).map(String::as_str).collect();
        installed.sort();
        assert_eq!(installed, ["calculate", "compact_context", "edit_own_prompt", "get_date", "get_task_checkpoint", "get_time", "list_skills", "list_tools", "load_skill", "load_tool", "present_chat_component", "present_plan", "read_attachment", "read_own_prompt", "recall_compacted", "remove_own_skill", "save_own_skill", "search_this_session", "search_tools", "session_search", "set_execution_mode", "similar_sessions", "task_cancel", "task_status", "task_wait", "update_task_checkpoint"]);
        assert!(!catalog.abilities.iter().any(|ability| matches!(ability.id.as_str(), "v3_build"|"v3_workspace")));
        let coding=catalog.abilities.iter().find(|ability| ability.id=="codebase_admin").unwrap();
        assert_eq!(coding.tools, ["file_list","file_read","file_search","file_edit","file_write",
            "build_work","state_inspect","prompt_slot_read","prompt_slot_write","shell","python"]);
        assert!(catalog.abilities.iter().any(|ability| ability.id == "diagnostics"));
        assert!(catalog.abilities.iter().any(|ability| ability.id == "terminal_control"));
    }
    #[test]
    fn web_access_loading_policy_and_visibility_gates() {
        let catalog=Catalog::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("plugins/abilities")).unwrap();
        let mut p=crate::policy_default();
        let mut v=json!({"visibility":{"web_access":"discoverable"}});
        let mut run=json!({"loaded_abilities":[]});
        let state=json!({"active":[],"suppressed":[]});
        assert!(!catalog.tool_available("maps_geocode",&v,&state,&json!([])));
        let result=catalog.discover_with("load_ability",&json!({"ability_id":"web_access"}),&mut run,&p,&v,&state);
        assert_eq!(result["status"],"ok");
        assert!(result["tool_skills"]["maps_geocode"].as_str().unwrap().contains("great-circle"));
        let active=json!({"active":["web_access"],"suppressed":[]});
        assert!(catalog.tool_available("maps_geocode",&v,&active,&run["loaded_abilities"]));
        assert_eq!(catalog.tool_abilities("maps_geocode",&v,&active,&run["loaded_abilities"]),vec!["web_access"]);
        p["rules"]["web_search"]=json!("deny");
        assert!(!catalog.schemas_with(&p,&run["loaded_abilities"],&v,&active).iter().any(|s|s["name"]=="web_search"));
        v["visibility"]["web_access"]=json!("off");
        assert!(!catalog.tool_available("maps_geocode",&v,&active,&run["loaded_abilities"]));
    }

}
