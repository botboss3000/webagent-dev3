//! What each ability declares about access: its manifest's `default_access` by capability
//! level, as tools declare theirs in `tool.json`, and its group's `entitlement_group` from
//! `_group.json`, which account plans grant. Read once per process from the same abilities
//! folder as the tool definitions. An ability that declares no default stays closed until
//! a level grants it; an ungrouped ability needs a named plan grant.
use crate::tool_definitions::{self, ROLES};
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};
use webagent_access::Permission;

/// One ability's declared access.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AbilityAccess {
    pub default_access: BTreeMap<String, Permission>,
    /// Account-tier template by tier ID (see `webagent_access::Template`).
    pub account_access: BTreeMap<String, Permission>,
    pub entitlement: Option<String>,
}

impl AbilityAccess {
    /// The default for the first of `rungs` (a level, then the levels below it) this
    /// ability names; `None` when it names none.
    #[must_use]
    pub fn default_for(&self, rungs: &[String]) -> Option<Permission> {
        rungs
            .iter()
            .find_map(|rung| self.default_access.get(rung))
            .copied()
    }
}

/// Declared access by ability, each tool's entitlement group, and folders that could
/// not be used.
#[derive(Debug, Default)]
pub struct Registry {
    pub abilities: BTreeMap<String, AbilityAccess>,
    pub tools: BTreeMap<String, String>,
    pub problems: Vec<String>,
}

/// Read every ability under `root` (`<Group>/<ability>/<ability>.json`).
#[must_use]
pub fn load_all(root: &Path) -> Registry {
    let mut registry = Registry::default();
    let mut groups = fs::read_dir(root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .collect::<Vec<_>>();
    groups.sort_by_key(fs::DirEntry::file_name);
    for group in groups {
        let entitlement = fs::read(group.path().join("_group.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .and_then(|meta| meta["entitlement_group"].as_str().map(str::to_owned))
            .filter(|group| webagent_access::valid_id(group));
        for ability in fs::read_dir(group.path()).into_iter().flatten().flatten() {
            let folder = ability.path();
            let id = ability.file_name().to_string_lossy().into_owned();
            let Ok(bytes) = fs::read(folder.join(format!("{id}.json"))) else {
                continue;
            };
            let declared = serde_json::from_slice::<Value>(&bytes)
                .map_err(|_| "unreadable manifest".to_owned())
                .and_then(|manifest| {
                    Ok((
                        defaults(&manifest["default_access"])?,
                        templates(&manifest["account_access"])?,
                    ))
                });
            let (default_access, account_access) = match declared {
                Ok(declared) => declared,
                Err(reason) => {
                    registry.problems.push(format!("{id}: {reason}"));
                    continue;
                }
            };
            if let Some(group) = &entitlement {
                for tool in tool_definitions::folder_names(&folder).unwrap_or_default() {
                    registry.tools.entry(tool).or_insert_with(|| group.clone());
                }
            }
            registry.abilities.insert(
                id,
                AbilityAccess {
                    default_access,
                    account_access,
                    entitlement: entitlement.clone(),
                },
            );
        }
    }
    registry
}

/// `default_access`: absent, or an object of known roles to permissions.
fn defaults(value: &Value) -> Result<BTreeMap<String, Permission>, String> {
    if value.is_null() {
        return Ok(BTreeMap::new());
    }
    let roles = value
        .as_object()
        .ok_or("default_access must be an object")?;
    let mut defaults = BTreeMap::new();
    for (role, permission) in roles {
        if !ROLES.contains(&role.as_str()) {
            return Err(format!("default_access names an unknown role: {role}"));
        }
        let permission = permission
            .as_str()
            .and_then(|permission| Permission::from_policy(permission).ok())
            .ok_or("default_access permissions are auto, ask or deny")?;
        defaults.insert(role.clone(), permission);
    }
    Ok(defaults)
}

/// `account_access`: absent, or an object of tier IDs to permissions.
fn templates(value: &Value) -> Result<BTreeMap<String, Permission>, String> {
    if value.is_null() {
        return Ok(BTreeMap::new());
    }
    let tiers = value
        .as_object()
        .ok_or("account_access must be an object")?;
    let mut templates = BTreeMap::new();
    for (tier, permission) in tiers {
        if !webagent_access::valid_id(tier) {
            return Err(format!("account_access names an invalid tier: {tier}"));
        }
        let permission = permission
            .as_str()
            .and_then(|permission| Permission::from_policy(permission).ok())
            .ok_or("account_access permissions are auto, ask or deny")?;
        templates.insert(tier.clone(), permission);
    }
    Ok(templates)
}

static LOADED: OnceLock<Registry> = OnceLock::new();

/// Process-wide registry, read once on first use.
pub fn registry() -> &'static Registry {
    LOADED.get_or_init(|| {
        tool_definitions::root()
            .map(|root| load_all(&root))
            .unwrap_or_default()
    })
}

/// An ability's declared default for a level's rungs (installed into the access layer).
pub fn default_for(id: &str, rungs: &[String]) -> Option<Permission> {
    registry().abilities.get(id)?.default_for(rungs)
}

/// An ability's manifest template for account tier `tier` (installed into the access layer).
pub fn ability_template(id: &str, tier: &str) -> Option<Permission> {
    registry()
        .abilities
        .get(id)?
        .account_access
        .get(tier)
        .copied()
}

/// An ability's entitlement group (installed into the access layer).
pub fn ability_entitlement(id: &str) -> Option<&'static str> {
    registry().abilities.get(id)?.entitlement.as_deref()
}

/// The entitlement group of the ability a tool belongs to (installed into the access layer).
pub fn tool_entitlement(name: &str) -> Option<&'static str> {
    registry().tools.get(name).map(String::as_str)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn write(root: &Path, path: &str, text: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn manifests_declare_defaults_and_groups_grant_their_tools() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "Web/_group.json",
            r#"{"name":"Web","entitlement_group":"web_read"}"#,
        );
        write(
            root,
            "Web/web_search/web_search.json",
            r#"{"default_access":{"operator":"auto","member":"ask"},"account_access":{"basic":"ask"}}"#,
        );
        write(root, "Web/web_search/tools/web_read/tool.json", "{}");
        write(root, "Core/_group.json", r#"{"name":"Core"}"#);
        write(root, "Core/plain/plain.json", "{}");
        write(
            root,
            "Core/broken/broken.json",
            r#"{"default_access":{"admin":"auto"}}"#,
        );
        let registry = load_all(root);
        let search = &registry.abilities["web_search"];
        let rungs = |names: &[&str]| {
            names
                .iter()
                .map(|name| (*name).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            search.default_for(&rungs(&["member", "visitor"])),
            Some(Permission::Ask)
        );
        assert_eq!(search.default_for(&rungs(&["visitor"])), None);
        assert_eq!(search.entitlement.as_deref(), Some("web_read"));
        assert_eq!(search.account_access.get("basic"), Some(&Permission::Ask));
        assert!(search.account_access.get("anonymous").is_none());
        assert_eq!(
            registry.tools.get("web_read").map(String::as_str),
            Some("web_read")
        );
        assert_eq!(registry.abilities["plain"], AbilityAccess::default());
        assert!(!registry.abilities.contains_key("broken"));
        assert!(
            registry.problems[0].contains("unknown role"),
            "{:?}",
            registry.problems
        );
    }
}
