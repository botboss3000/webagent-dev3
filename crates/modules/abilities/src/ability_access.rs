//! What each ability declares about access: its manifest's `default_access` by capability
//! level, as tools declare theirs in `tool.json`, and `tier_access`, the app tiers whose
//! owners' agents may have it. Read once per process from the same abilities folder as the
//! tool definitions. The default is shown, not applied: an ability stays closed until a
//! level grants it. A manifest without `tier_access` grants no tier and is reported.
use crate::tool_definitions::ROLES;
use serde_json::Value;
use std::{collections::BTreeMap, fs, path::Path, sync::OnceLock};
use webagent_access::{Permission, ADMINISTRATOR_TIER};

/// The group whose abilities only an administrator owner and caller may use.
pub const ADMINISTRATOR_GROUP: &str = "Administrator";

/// The tiers built-in Base works for, as a manifest's `tier_access` would list them.
/// Tiers whose owners' agents get the built-in Base ability, which has no manifest.
pub const BUILT_IN_TIERS: [&str; 2] = [webagent_access::FREE_TIER, ADMINISTRATOR_TIER];

/// One ability's declared access.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AbilityAccess {
    pub default_access: BTreeMap<String, Permission>,
    /// Tiers whose owners' agents may have this ability; empty grants none.
    pub tier_access: Vec<String>,
    /// In the Administrator group: hard-coded to administrators, whatever `tier_access` says.
    pub administrator: bool,
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

    /// Whether an agent owned on `owner_tier` may use this ability for a caller on
    /// `caller_tier`. Administrator abilities need both on the administrator tier.
    #[must_use]
    pub fn allows(&self, owner_tier: &str, caller_tier: &str) -> bool {
        if self.administrator {
            owner_tier == ADMINISTRATOR_TIER && caller_tier == ADMINISTRATOR_TIER
        } else {
            self.tier_access.iter().any(|tier| tier == owner_tier)
        }
    }
}

/// Declared access by ability, and manifests that are missing or wrong.
#[derive(Debug, Default)]
pub struct Registry {
    pub abilities: BTreeMap<String, AbilityAccess>,
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
        let administrator = group.file_name() == ADMINISTRATOR_GROUP;
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
                        tiers(&manifest["tier_access"])?,
                    ))
                });
            let (default_access, tier_access) = match declared {
                Ok(declared) => declared,
                Err(reason) => {
                    registry.problems.push(format!("{id}: {reason}"));
                    continue;
                }
            };
            if administrator && tier_access != [ADMINISTRATOR_TIER] {
                registry.problems.push(format!(
                    "{id}: tier_access must be [\"{ADMINISTRATOR_TIER}\"]"
                ));
            }
            registry.abilities.insert(
                id,
                AbilityAccess {
                    default_access,
                    tier_access,
                    administrator,
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

/// `tier_access`: a required list of tier IDs.
pub fn tiers(value: &Value) -> Result<Vec<String>, String> {
    let tiers = value
        .as_array()
        .ok_or("tier_access must list the tiers that may have it")?;
    tiers
        .iter()
        .map(|tier| {
            tier.as_str()
                .filter(|tier| webagent_access::valid_id(tier))
                .map(str::to_owned)
                .ok_or_else(|| format!("tier_access names an invalid tier: {tier}"))
        })
        .collect()
}

static LOADED: OnceLock<Registry> = OnceLock::new();

/// Process-wide registry, read once on first use.
pub fn registry() -> &'static Registry {
    LOADED.get_or_init(|| {
        crate::tool_definitions::root()
            .map(|root| load_all(&root))
            .unwrap_or_default()
    })
}

/// An ability's declared default for a level's rungs, shown on the Abilities page.
pub fn default_for(id: &str, rungs: &[String]) -> Option<Permission> {
    registry().abilities.get(id)?.default_for(rungs)
}

/// Whether ability `id` works on an agent owned on `owner_tier` for a caller on
/// `caller_tier` (installed into the access layer). Unknown abilities never do.
pub fn ability_allows(id: &str, owner_tier: &str, caller_tier: &str) -> bool {
    // Built-in Base has no manifest; it is a normal ability for every signed-in tier.
    if id == webagent_access::BUILT_IN_ABILITY {
        return BUILT_IN_TIERS.contains(&owner_tier);
    }
    registry()
        .abilities
        .get(id)
        .is_some_and(|ability| ability.allows(owner_tier, caller_tier))
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
    fn manifests_declare_defaults_and_tiers() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write(
            root,
            "Web/web_search/web_search.json",
            r#"{"default_access":{"operator":"auto","member":"ask"},"tier_access":["free"]}"#,
        );
        write(root, "Core/plain/plain.json", "{}");
        write(
            root,
            "Core/broken/broken.json",
            r#"{"default_access":{"admin":"auto"},"tier_access":[]}"#,
        );
        write(
            root,
            "Administrator/shell/shell.json",
            r#"{"tier_access":["free","administrator"]}"#,
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
        assert!(search.allows("free", "anonymous"));
        assert!(!search.allows("administrator", "administrator"));
        // Missing and wrong lists are reported; the abilities stay closed.
        assert!(!registry.abilities.contains_key("plain"));
        assert!(!registry.abilities.contains_key("broken"));
        let shell = &registry.abilities["shell"];
        assert!(shell.allows("administrator", "administrator"));
        assert!(!shell.allows("administrator", "free"));
        assert!(!shell.allows("free", "administrator"));
        assert_eq!(registry.problems.len(), 3, "{:?}", registry.problems);
    }

    /// Release check: every shipped ability declares its tiers, and Administrator
    /// abilities exactly `["administrator"]`.
    #[test]
    fn shipped_manifests_declare_tier_access() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../abilities");
        let registry = load_all(&root);
        assert!(registry.abilities.len() > 10);
        assert!(registry.problems.is_empty(), "{:?}", registry.problems);
        // A tool's own list may only narrow its ability's.
        for (id, ability) in &registry.abilities {
            for group in fs::read_dir(&root).unwrap().flatten() {
                let tools = group.path().join(id).join("tools");
                for tool in fs::read_dir(tools).into_iter().flatten().flatten() {
                    let Ok(bytes) = fs::read(tool.path().join("tool.json")) else {
                        continue;
                    };
                    let manifest: Value = serde_json::from_slice(&bytes).unwrap();
                    if manifest["tier_access"].is_null() {
                        continue;
                    }
                    let own = tiers(&manifest["tier_access"]).unwrap();
                    assert!(
                        own.iter().all(|tier| ability.tier_access.contains(tier)),
                        "{id}/{:?} widens tier_access",
                        tool.file_name()
                    );
                }
            }
        }
    }
}
