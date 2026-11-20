//! What each ability declares about access, as tools declare theirs in `<tool>.tool.json`: its
//! manifest's `default_policy` by capability level, `owner_tier` (the app tiers whose
//! owners' agents may have it) and `role_access` (the built-in roles that may ever have
//! it). Read once per process from the same abilities folder as the tool definitions. The
//! default is shown, not applied: an ability stays closed until a level grants it. A
//! manifest without `owner_tier` grants no tier and is reported.
use crate::tool_definitions::ROLES;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};
use webagent_access::{Permission, ADMINISTRATOR_TIER};

/// The group whose abilities only an administrator owner and caller may use.
pub const ADMINISTRATOR_GROUP: &str = "Administrator";

/// Tiers whose owners' agents get the built-in Base ability, which has no manifest.
pub const BUILT_IN_TIERS: [&str; 2] = [webagent_access::FREE_TIER, ADMINISTRATOR_TIER];

/// One ability's declared access.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AbilityAccess {
    pub default_policy: BTreeMap<String, Permission>,
    /// Tiers whose owners' agents may have this ability; empty grants none.
    pub owner_tier: Vec<String>,
    /// Built-in roles that may ever have this ability; `None` means every role. A hard
    /// limit: no level setting widens it.
    pub role_access: Option<Vec<String>>,
    /// In the Administrator group: hard-coded to administrators, whatever `owner_tier` says.
    pub administrator: bool,
}

impl AbilityAccess {
    /// The default for the first of `rungs` (a level, then the levels below it) this
    /// ability names; `None` when it names none.
    #[must_use]
    pub fn default_for(&self, rungs: &[String]) -> Option<Permission> {
        rungs
            .iter()
            .find_map(|rung| self.default_policy.get(rung))
            .copied()
    }

    /// Whether an agent owned on `owner_tier` may use this ability for a caller on
    /// `caller_tier`. Administrator abilities need both on the administrator tier.
    #[must_use]
    pub fn allows(&self, owner_tier: &str, caller_tier: &str) -> bool {
        if self.administrator {
            owner_tier == ADMINISTRATOR_TIER && caller_tier == ADMINISTRATOR_TIER
        } else {
            self.owner_tier.iter().any(|tier| tier == owner_tier)
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
                        defaults(&manifest["default_policy"])?,
                        tiers(&manifest["owner_tier"])?,
                        roles(&manifest["role_access"])?,
                    ))
                });
            let (default_policy, owner_tier, role_access) = match declared {
                Ok(declared) => declared,
                Err(reason) => {
                    registry.problems.push(format!("{id}: {reason}"));
                    continue;
                }
            };
            if administrator && owner_tier != [ADMINISTRATOR_TIER] {
                registry.problems.push(format!(
                    "{id}: owner_tier must be [\"{ADMINISTRATOR_TIER}\"]"
                ));
            }
            registry.abilities.insert(
                id,
                AbilityAccess {
                    default_policy,
                    owner_tier,
                    role_access,
                    administrator,
                },
            );
        }
    }
    registry
}

/// `default_policy`: absent, or an object of known roles to permissions.
fn defaults(value: &Value) -> Result<BTreeMap<String, Permission>, String> {
    if value.is_null() {
        return Ok(BTreeMap::new());
    }
    let roles = value
        .as_object()
        .ok_or("default_policy must be an object")?;
    let mut defaults = BTreeMap::new();
    for (role, permission) in roles {
        if !ROLES.contains(&role.as_str()) {
            return Err(format!("default_policy names an unknown role: {role}"));
        }
        let permission = permission
            .as_str()
            .and_then(|permission| Permission::from_policy(permission).ok())
            .ok_or("default_policy values are allow, require_approval or deny")?;
        defaults.insert(role.clone(), permission);
    }
    Ok(defaults)
}

/// `owner_tier`: a required list of tier IDs.
pub fn tiers(value: &Value) -> Result<Vec<String>, String> {
    let tiers = value
        .as_array()
        .ok_or("owner_tier must list the tiers that may have it")?;
    tiers
        .iter()
        .map(|tier| {
            tier.as_str()
                .filter(|tier| webagent_access::valid_id(tier))
                .map(str::to_owned)
                .ok_or_else(|| format!("owner_tier names an invalid tier: {tier}"))
        })
        .collect()
}

/// `role_access`: absent (every role), or a non-empty list of built-in roles.
pub fn roles(value: &Value) -> Result<Option<Vec<String>>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let roles = value
        .as_array()
        .filter(|roles| !roles.is_empty())
        .ok_or("role_access must list at least one role")?;
    roles
        .iter()
        .map(|role| {
            role.as_str()
                .filter(|role| ROLES.contains(role))
                .map(str::to_owned)
                .ok_or_else(|| format!("role_access names an unknown role: {role}"))
        })
        .collect::<Result<_, _>>()
        .map(Some)
}

/// The registry of the abilities root it was read from. Each root is read once and kept, as
/// callers hold `'static` references; publishes and live edits are rare.
static LOADED: Mutex<Option<(Option<PathBuf>, &'static Registry)>> = Mutex::new(None);

/// Process-wide registry from the current abilities root (`tool_definitions::root`), read
/// again when that root changes, so a publish or live ability edit applies without a restart.
pub fn registry() -> &'static Registry {
    let root = crate::tool_definitions::root();
    let mut held = LOADED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((read_from, loaded)) = held.as_ref() {
        if *read_from == root {
            return loaded;
        }
    }
    let loaded: &'static Registry =
        Box::leak(Box::new(root.as_deref().map(load_all).unwrap_or_default()));
    *held = Some((root, loaded));
    loaded
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

/// The built-in role a capability level counts as: the first built-in role among its
/// rungs (it, then the levels below it); a level below every built-in role is a visitor.
#[must_use]
pub fn built_in_role(rungs: &[String]) -> &'static str {
    rungs
        .iter()
        .find_map(|rung| ROLES.iter().find(|role| **role == rung.as_str()))
        .copied()
        .unwrap_or("visitor")
}

/// Whether ability `id`'s `role_access` admits a level with these rungs. Abilities
/// without a manifest (built-in Base) and without a list admit every role.
pub fn ability_role_allows(id: &str, rungs: &[String]) -> bool {
    let role = built_in_role(rungs);
    registry()
        .abilities
        .get(id)
        .and_then(|ability| ability.role_access.as_ref())
        .is_none_or(|roles| roles.iter().any(|allowed| allowed == role))
}

/// Whether ability `id` is in the Administrator group (installed into the access layer).
pub fn administrator_ability(id: &str) -> bool {
    registry()
        .abilities
        .get(id)
        .is_some_and(|ability| ability.administrator)
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
            r#"{"default_policy":{"operator":"allow","member":"require_approval"},"owner_tier":["free"]}"#,
        );
        write(root, "Core/plain/plain.json", "{}");
        write(
            root,
            "Core/broken/broken.json",
            r#"{"default_policy":{"admin":"allow"},"owner_tier":[]}"#,
        );
        write(
            root,
            "Administrator/shell/shell.json",
            r#"{"owner_tier":["free","administrator"]}"#,
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
    fn shipped_manifests_declare_owner_tier() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../abilities");
        let registry = load_all(&root);
        assert!(registry.abilities.len() > 10);
        assert!(registry.problems.is_empty(), "{:?}", registry.problems);
        // A tool's own list may only narrow its ability's.
        for (id, ability) in &registry.abilities {
            for group in fs::read_dir(&root).unwrap().flatten() {
                let tools = group.path().join(id).join("tools");
                for tool in fs::read_dir(tools).into_iter().flatten().flatten() {
                    let Ok(bytes) =
                        fs::read(tool.path().join(crate::tool_definitions::definition_file(
                            &tool.file_name().to_string_lossy(),
                        )))
                    else {
                        continue;
                    };
                    let manifest: Value = serde_json::from_slice(&bytes).unwrap();
                    if manifest["owner_tier"].is_null() {
                        continue;
                    }
                    let own = tiers(&manifest["owner_tier"]).unwrap();
                    assert!(
                        own.iter().all(|tier| ability.owner_tier.contains(tier)),
                        "{id}/{:?} widens owner_tier",
                        tool.file_name()
                    );
                }
            }
        }
    }
}
