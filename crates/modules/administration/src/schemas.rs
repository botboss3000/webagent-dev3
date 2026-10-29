//! Administrator tool schemas, read from the tool folders under an abilities root.
//!
//! A catalog snapshot reads them from its own root, so a published description edit
//! reaches the model with that snapshot and needs no Rust build. The schema only
//! describes a tool: execution identities and argument checks stay compiled here and in
//! the host crate.
use crate::tools::{supported, TOOLS};
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use webagent_abilities::tool_definitions::{read_folder, ToolDefinition};

/// The ability group holding every Administrator tool folder.
pub const GROUP: &str = "Administrator";

/// Administrator schemas found under one abilities root, and why any were left out.
#[derive(Debug, Default)]
pub struct Loaded {
    /// Function schemas in [`TOOLS`] order.
    pub schemas: Vec<Value>,
    /// One message per tool that was left out.
    pub problems: Vec<String>,
}

/// Read the Administrator tool schemas under `abilities` (a catalog root).
///
/// A tool whose folder is missing, invalid or duplicated is left out, so the model never
/// sees it; tool folders that are not Administrator native tools are ignored. Schemas
/// come back in [`TOOLS`] order, so a catalog digest over them is stable.
pub fn load(abilities: &Path) -> Result<Loaded> {
    let group = abilities.join(GROUP);
    let entries = match fs::read_dir(&group) {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(Loaded {
                schemas: Vec::new(),
                problems: vec![format!(
                    "No Administrator tool folders under {}",
                    abilities.display()
                )],
            });
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("Cannot read Administrator tools: {}", group.display()))
        }
    };
    let mut problems = Vec::new();
    let mut found: BTreeMap<String, Option<ToolDefinition>> = BTreeMap::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let tools = match read_folder(&entry.path()) {
            Ok(Some(tools)) => tools,
            Ok(None) => continue,
            Err(error) => {
                problems.push(format!("{error:#}"));
                continue;
            }
        };
        for tool in tools {
            if !supported(&tool.name) {
                continue;
            }
            match tool.definition {
                Ok(definition) => {
                    if found.insert(tool.name.clone(), Some(definition)).is_some() {
                        // Two folders for one tool: neither is trusted.
                        problems.push(format!(
                            "Duplicate Administrator tool folder: {}",
                            tool.name
                        ));
                        found.insert(tool.name, None);
                    }
                }
                Err(error) => problems.push(format!("{error:#}")),
            }
        }
    }
    let mut schemas = Vec::with_capacity(TOOLS.len());
    for name in TOOLS {
        match found.get(name) {
            Some(Some(definition)) => schemas.push(definition.schema()),
            Some(None) => {}
            None => problems.push(format!(
                "Administrator tool {name} has no valid tool folder; it is disabled"
            )),
        }
    }
    Ok(Loaded { schemas, problems })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    fn write_tool(root: &Path, ability: &str, name: &str, description: &str) {
        let folder = root.join(GROUP).join(ability).join("tools").join(name);
        fs::create_dir_all(&folder).expect("tool folder");
        let definition = json!({"name":name,"description":description,"effect":"read",
            "default_access":{"operator":"auto"},
            "parameters":{"type":"object","properties":{},"required":[],"additionalProperties":false}});
        fs::write(folder.join("tool.json"), definition.to_string()).expect("tool.json");
    }

    #[test]
    fn checkout_folders_hold_every_administrator_tool() {
        let abilities =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../plugins/abilities");
        let loaded = load(&abilities).expect("checkout tool folders");
        assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
        let names: Vec<_> = loaded
            .schemas
            .iter()
            .map(|schema| schema["name"].clone())
            .collect();
        assert_eq!(names, TOOLS.map(Value::from).to_vec());
        let maintenance = &loaded.schemas[5];
        assert_eq!(maintenance["name"], "code_index_maintenance");
        assert_eq!(
            maintenance["strict"], false,
            "the folder keeps the schema's strict flag"
        );
    }

    #[test]
    fn missing_invalid_and_duplicate_folders_disable_only_their_tool() {
        let root = tempfile::tempdir().expect("root");
        write_tool(root.path(), "a", "git_tool", "Inspect a repository");
        write_tool(root.path(), "a", "read_diagnostics", "Read run diagnostics");
        write_tool(root.path(), "b", "read_diagnostics", "Second copy");
        write_tool(root.path(), "a", "not_administrator", "Ignored");
        write_tool(root.path(), "b", "v3_source_read", "");
        let loaded = load(root.path()).expect("loaded");
        let names: Vec<_> = loaded
            .schemas
            .iter()
            .map(|schema| schema["name"].clone())
            .collect();
        assert_eq!(names, vec![json!("git_tool")]);
        assert!(loaded
            .problems
            .iter()
            .any(|p| p.contains("Duplicate Administrator tool folder")));
        assert!(loaded.problems.iter().any(|p| p.contains("v3_source_read")));
        assert!(loaded
            .problems
            .iter()
            .any(|p| p.contains("build_work has no valid tool folder")));
    }

    #[test]
    fn a_root_without_the_group_has_no_schemas() {
        let root = tempfile::tempdir().expect("root");
        let loaded = load(root.path()).expect("loaded");
        assert!(loaded.schemas.is_empty());
        assert_eq!(loaded.problems.len(), 1);
    }
}
