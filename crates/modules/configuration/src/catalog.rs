//! Setting descriptors for the configuration composer. Each configuration owner keeps a
//! `*.settings.json` beside its configuration; [`INDEX`] lists them, and [`load`] merges them
//! into one validated catalog. Descriptors hold no current values: those are read live.
use anyhow::{anyhow, bail, ensure, Context, Result};
use serde_json::{Map, Value};
use std::collections::BTreeSet;
use std::path::Path;

/// The catalog index, relative to the checkout.
pub const INDEX: &str = "crates/modules/configuration/settings-catalog.json";
const LIVE_FILES: &str = "live-files.json";
const LAYERS: [&str; 3] = ["app", "agent", "user"];
const STORES: [&str; 2] = ["agent_metadata", "user_profile"];
const KINDS: [&str; 8] = [
    "boolean", "integer", "number", "enum", "string", "length", "object", "agent_id",
];
const FIELDS: [&str; 19] = [
    "id",
    "description",
    "type",
    "pointer",
    "layers",
    "examples",
    "areas",
    "unit",
    "min",
    "max",
    "recommended_max",
    "max_length",
    "enum",
    "parameters",
    "agent_pointer",
    "value_shape",
    "bounds",
    "notes",
    "nullable",
];

/// One validated descriptor, the descriptor file it came from and the live configuration file
/// its app-layer value lives in (`None` for stores).
#[derive(Debug, Clone)]
pub struct Setting {
    pub owner: String,
    pub config: Option<String>,
    pub descriptor: Value,
}

/// Load and validate every descriptor listed in the index under `root` (a checkout).
pub fn load(root: &Path) -> Result<Vec<Setting>> {
    let mut ids = BTreeSet::new();
    let mut settings = Vec::new();
    for owner in owners(root)? {
        for descriptor in array(&owner.descriptors, "settings")? {
            let id =
                validate(descriptor, owner.config.as_ref()).with_context(|| owner.file.clone())?;
            ensure!(
                ids.insert(id.to_owned()),
                "{}: duplicate setting {id}",
                owner.file
            );
            settings.push(Setting {
                owner: owner.file.clone(),
                config: owner.config_path.clone(),
                descriptor: descriptor.clone(),
            });
        }
    }
    Ok(settings)
}

/// Live files and config leaves that are neither described nor excluded with a reason.
pub fn uncovered(root: &Path) -> Result<Vec<String>> {
    let index = crate::read(&root.join(INDEX))?;
    let excluded_files = object(&index, "excluded_files")?;
    let owners = owners(root)?;
    let mut missing = Vec::new();
    for live in array(&crate::read(&root.join(LIVE_FILES))?, "files")? {
        let live = live
            .as_str()
            .ok_or_else(|| anyhow!("Invalid live file entry"))?;
        let owned = owners
            .iter()
            .any(|o| o.config_path.as_deref() == Some(live));
        if !owned && !reason(excluded_files.get(live)) {
            missing.push(live.to_owned());
        }
    }
    for owner in &owners {
        let Some(config) = &owner.config else {
            continue;
        };
        let described = array(&owner.descriptors, "settings")?
            .iter()
            .map(|d| expand(text(d, "pointer")?, d.get("parameters")))
            .collect::<Result<Vec<_>>>()?
            .concat();
        let excluded = object(&owner.descriptors, "excluded")?;
        ensure!(
            excluded.values().all(|r| reason(Some(r))),
            "{}: every exclusion needs a reason",
            owner.file
        );
        let mut leaves = Vec::new();
        collect_leaves(config, String::new(), &mut leaves);
        for leaf in leaves {
            let is_described = described
                .iter()
                .any(|p| leaf == *p || leaf.starts_with(&format!("{p}/")));
            if !is_described && !excluded.keys().any(|pattern| matches(pattern, &leaf)) {
                missing.push(format!(
                    "{}{leaf}",
                    owner.config_path.as_deref().unwrap_or("")
                ));
            }
        }
    }
    Ok(missing)
}

struct Owner {
    file: String,
    config_path: Option<String>,
    config: Option<Value>,
    descriptors: Value,
}

fn owners(root: &Path) -> Result<Vec<Owner>> {
    let index = crate::read(&root.join(INDEX))?;
    array(&index, "owners")?
        .iter()
        .map(|owner| {
            let file = text(owner, "descriptors")?.to_owned();
            let config_path = match (owner.get("config"), owner.get("store")) {
                (Some(Value::String(path)), None) => Some(path.clone()),
                (None, Some(Value::String(store))) if STORES.contains(&store.as_str()) => None,
                _ => bail!("{file}: an owner needs one config path or a known store"),
            };
            let config = match &config_path {
                Some(path) => Some(crate::read(&root.join(path))?),
                None => None,
            };
            let descriptors = crate::read(&root.join(&file))?;
            Ok(Owner {
                file,
                config_path,
                config,
                descriptors,
            })
        })
        .collect()
}

/// Check one descriptor; when its owner is a file, check each pointer resolves to its type.
fn validate<'a>(descriptor: &'a Value, config: Option<&Value>) -> Result<&'a str> {
    let id = text(descriptor, "id")?;
    let fields = descriptor
        .as_object()
        .ok_or_else(|| anyhow!("Invalid descriptor"))?;
    if let Some(field) = fields.keys().find(|k| !FIELDS.contains(&k.as_str())) {
        bail!("{id}: unknown field {field}");
    }
    ensure!(
        !text(descriptor, "description")?.trim().is_empty(),
        "{id}: empty description"
    );
    let kind = text(descriptor, "type")?;
    ensure!(KINDS.contains(&kind), "{id}: unknown type {kind}");
    let layers = strings(descriptor, "layers")?;
    ensure!(
        !layers.is_empty() && layers.iter().all(|l| LAYERS.contains(l)),
        "{id}: invalid layers"
    );
    ensure!(
        !strings(descriptor, "examples")?.is_empty(),
        "{id}: needs example phrases"
    );
    ensure!(
        (kind == "enum") != strings(descriptor, "enum")?.is_empty(),
        "{id}: enum values belong to enum settings"
    );
    if let (Some(min), Some(max)) = (descriptor["min"].as_f64(), descriptor["max"].as_f64()) {
        ensure!(min <= max, "{id}: min exceeds max");
    }
    let pointers = expand(text(descriptor, "pointer")?, descriptor.get("parameters"))?;
    if let Some(config) = config {
        ensure!(
            layers.contains(&"app"),
            "{id}: a file setting needs the app layer"
        );
        for pointer in pointers {
            let value = config
                .pointer(&pointer)
                .ok_or_else(|| anyhow!("{id}: {pointer} is missing"))?;
            // `nullable` settings store null for "no limit".
            let null_ok = value.is_null() && descriptor["nullable"] == true;
            ensure!(
                null_ok || fits(kind, value)?,
                "{id}: {pointer} is not {kind}"
            );
        }
    }
    Ok(id)
}

fn fits(kind: &str, value: &Value) -> Result<bool> {
    Ok(match kind {
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "enum" | "string" | "agent_id" => value.is_string(),
        "length" | "object" => value.is_object(),
        _ => bail!("Unknown setting type {kind}"),
    })
}

/// Expand `{name}` placeholders over each listed parameter value.
fn expand(pointer: &str, parameters: Option<&Value>) -> Result<Vec<String>> {
    ensure!(pointer.starts_with('/'), "Invalid pointer {pointer}");
    let mut pointers = vec![pointer.to_owned()];
    if let Some(parameters) = parameters {
        let parameters = parameters
            .as_object()
            .ok_or_else(|| anyhow!("Invalid parameters for {pointer}"))?;
        for (name, values) in parameters {
            let values = values
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| anyhow!("Parameter {name} needs values"))?;
            let placeholder = format!("{{{name}}}");
            let mut expanded = Vec::new();
            for pointer in &pointers {
                for value in values {
                    let value = value
                        .as_str()
                        .ok_or_else(|| anyhow!("Invalid value for parameter {name}"))?;
                    expanded.push(pointer.replace(&placeholder, value));
                }
            }
            pointers = expanded;
        }
    }
    ensure!(
        pointers.iter().all(|p| !p.contains('{')),
        "Unbound parameter in {pointer}"
    );
    Ok(pointers)
}

/// Leaf pointers of `value`; arrays are leaves and instruction text is skipped.
fn collect_leaves(value: &Value, pointer: String, leaves: &mut Vec<String>) {
    match value.as_object() {
        Some(object) => {
            for (key, child) in object {
                if key != "_instructions" && key != "description" {
                    collect_leaves(child, format!("{pointer}/{key}"), leaves);
                }
            }
        }
        None => leaves.push(pointer),
    }
}

/// `*` matches one pointer segment; a final `**` matches any remainder.
fn matches(pattern: &str, pointer: &str) -> bool {
    let mut segments = pointer.split('/');
    for part in pattern.split('/') {
        if part == "**" {
            return true;
        }
        match segments.next() {
            Some(segment) if part == "*" || part == segment => {}
            _ => return false,
        }
    }
    segments.next().is_none()
}

fn reason(value: Option<&Value>) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|r| !r.trim().is_empty())
}

fn text<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    value[key]
        .as_str()
        .ok_or_else(|| anyhow!("Missing text field {key}"))
}

fn array<'a>(value: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    value[key]
        .as_array()
        .ok_or_else(|| anyhow!("Missing list {key}"))
}

fn object<'a>(value: &'a Value, key: &str) -> Result<&'a Map<String, Value>> {
    value[key]
        .as_object()
        .ok_or_else(|| anyhow!("Missing object {key}"))
}

fn strings<'a>(value: &'a Value, key: &str) -> Result<Vec<&'a str>> {
    match value.get(key) {
        None => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| item.as_str().ok_or_else(|| anyhow!("{key} must hold text")))
            .collect(),
        Some(_) => bail!("{key} must be a list"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The whole checkout, or `None` where only build inputs were copied (the image's
    /// test run); the broker's focused run of this crate has the checkout.
    fn checkout() -> Option<std::path::PathBuf> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        if root.join(LIVE_FILES).is_file() {
            return Some(root);
        }
        eprintln!("skipped: no checkout {LIVE_FILES} beside this build");
        None
    }

    #[test]
    fn checkout_catalog_loads_with_unique_valid_settings() {
        let Some(root) = checkout() else { return };
        let settings = load(&root).unwrap();
        assert!(settings.len() >= 30, "catalog shrank to {}", settings.len());
    }

    #[test]
    fn every_live_setting_is_described_or_excluded_with_a_reason() {
        let Some(root) = checkout() else { return };
        let missing = uncovered(&root).unwrap();
        assert!(
            missing.is_empty(),
            "Describe these in the owner's *.settings.json or exclude them with a reason: {missing:?}"
        );
    }

    #[test]
    fn patterns_match_whole_segments() {
        assert!(matches("/tiers/*/tools/**", "/tiers/basic/tools/shell"));
        assert!(matches("/a/**", "/a/b/c"));
        assert!(matches("/a/*", "/a/b"));
        assert!(!matches("/a/*", "/a/b/c"));
        assert!(!matches("/a/b", "/a/bc"));
        assert!(!matches("/tiers/*/tools/**", "/tier_names/basic"));
    }

    #[test]
    fn descriptors_reject_wrong_types_unknown_fields_and_unbound_parameters() {
        let config = json!({"chat":{"size":17,"mode":"auto"},"tiers":{"basic":{"cap":2}}});
        let base = json!({"id":"x","description":"Size.","type":"number","pointer":"/chat/size",
            "layers":["app"],"examples":["bigger"]});
        assert_eq!(validate(&base, Some(&config)).unwrap(), "x");
        let mut wrong = base.clone();
        wrong["type"] = json!("boolean");
        assert!(validate(&wrong, Some(&config)).is_err());
        let mut unknown = base.clone();
        unknown["default"] = json!(17);
        assert!(validate(&unknown, Some(&config)).is_err());
        let mut missing = base.clone();
        missing["pointer"] = json!("/chat/absent");
        assert!(validate(&missing, Some(&config)).is_err());
        let mut no_app = base.clone();
        no_app["layers"] = json!(["user"]);
        assert!(validate(&no_app, Some(&config)).is_err());
        assert!(validate(&no_app, None).is_ok());
        let tier = json!({"id":"t","description":"Cap.","type":"integer","pointer":"/tiers/{tier}/cap",
            "parameters":{"tier":["basic"]},"layers":["app"],"examples":["more"]});
        assert!(validate(&tier, Some(&config)).is_ok());
        let mut unbound = tier.clone();
        unbound.as_object_mut().unwrap().remove("parameters");
        assert!(validate(&unbound, Some(&config)).is_err());
        let mut enum_without_values = base.clone();
        enum_without_values["type"] = json!("enum");
        enum_without_values["pointer"] = json!("/chat/mode");
        assert!(validate(&enum_without_values, Some(&config)).is_err());
    }
}
