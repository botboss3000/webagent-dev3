//! Prompts are live Markdown files under `crates/modules/configuration/prompts/`
//! (listed in `live-files.json`), assembled into the JSON shape owners consume:
//! `name.md` is a text value, `name.json` an object of settings and a folder a
//! nested object. Adding a prompt means adding a file; there is no saved copy.
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::PathBuf;
/// Checkout folder of the prompt files.
pub const PROMPTS: &str = "crates/modules/configuration/prompts";
const FILE_LIMIT: u64 = 64 * 1024;
pub fn published_root_at(ui: &std::path::Path) -> Result<PathBuf> {
    if !ui.ends_with("current/v3/ui") {
        anyhow::bail!("Invalid published configuration root");
    }
    let assets = ui
        .parent()
        .and_then(std::path::Path::parent)
        .and_then(std::path::Path::parent)
        .ok_or_else(|| anyhow!("Invalid published configuration root"))?;
    let selected = assets.join("current").canonicalize()?;
    if selected.parent() != Some(assets.join("releases").as_path())
        || selected
            .file_name()
            .and_then(|s| s.to_str())
            .is_none_or(|s| uuid::Uuid::parse_str(s).is_err())
    {
        anyhow::bail!("Invalid published configuration revision");
    }
    Ok(selected.join("v3"))
}
/// Explicit call snapshots and configured publication are authoritative; only an unconfigured installation uses packaged defaults.
pub fn default_source(source: Option<&std::path::Path>) -> Result<Option<PathBuf>> {
    if let Some(source) = source {
        return Ok(Some(source.to_owned()));
    }
    std::env::var_os("V3_UI_ROOT")
        .map(|ui| published_root_at(&PathBuf::from(ui)))
        .transpose()
}
fn roots(source: Option<&std::path::Path>) -> Result<Vec<PathBuf>> {
    Ok(match default_source(source)? {
        // An explicit candidate (asset validation, tests) is the only source; never mix in the checkout.
        Some(root) if source.is_some() => vec![root],
        _ => crate::live::roots(),
    })
}
fn assemble(roots: &[PathBuf], relative: &str, depth: usize) -> Result<Value> {
    if depth > 4 {
        bail!("Prompt folders are nested too deeply");
    }
    let mut object = serde_json::Map::new();
    for (name, folder) in crate::live::entries_from(roots, relative)? {
        let path = format!("{relative}/{name}");
        let (key, value) = if folder {
            (name.clone(), assemble(roots, &path, depth + 1)?)
        } else if let Some(key) = name.strip_suffix(".md") {
            let bytes = crate::live::read_from(roots, &path, FILE_LIMIT, |bytes| {
                std::str::from_utf8(bytes)
                    .map(|_| ())
                    .map_err(|_| anyhow!("Prompt file is not UTF-8"))
            })?;
            let text = String::from_utf8(bytes)?;
            // Files end with one newline by convention; the prompt text does not.
            (
                key.to_owned(),
                Value::String(text.strip_suffix('\n').unwrap_or(&text).to_owned()),
            )
        } else if let Some(key) = name.strip_suffix(".json") {
            let bytes = crate::live::read_from(roots, &path, FILE_LIMIT, |bytes| {
                match serde_json::from_slice::<Value>(bytes)? {
                    Value::Object(_) => Ok(()),
                    _ => bail!("Prompt settings must be an object"),
                }
            })?;
            (key.to_owned(), serde_json::from_slice(&bytes)?)
        } else {
            continue;
        };
        // `name.json` settings and a `name/` folder of texts merge into one object.
        match (object.get_mut(&key), value) {
            (Some(Value::Object(existing)), Value::Object(more)) => existing.extend(more),
            (Some(_), _) => bail!("Prompt {key} is defined twice"),
            (None, value) => {
                object.insert(key, value);
            }
        }
    }
    Ok(Value::Object(object))
}
/// The assembled prompts as JSON bytes, from a pinned candidate `source` or the live files.
pub fn prompt_defaults_at(
    source: Option<&std::path::Path>,
    _fallback_directory: &std::path::Path,
) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&assemble(&roots(source)?, PROMPTS, 0)?)?)
}
/// The assembled prompts; there are no saved overrides, so `root` (the state root) is unused.
pub fn resolved_prompts_at(
    _root: &std::path::Path,
    source: Option<&std::path::Path>,
    _fallback_directory: &std::path::Path,
) -> Result<Value> {
    assemble(&roots(source)?, PROMPTS, 0)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_tree_assembles_the_owner_json_shape() {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().join(PROMPTS);
        std::fs::create_dir_all(base.join("naming")).unwrap();
        std::fs::create_dir_all(base.join("skills")).unwrap();
        std::fs::write(base.join("base.md"), "Base text\n").unwrap();
        std::fs::write(base.join("naming.json"), r#"{"attempts":3}"#).unwrap();
        std::fs::write(base.join("naming/prompt.md"), "Name it").unwrap();
        std::fs::write(base.join("skills/review.md"), "Review\n\n").unwrap();
        std::fs::write(base.join("notes.txt"), "ignored").unwrap();
        let value = assemble(&[root.path().to_owned()], PROMPTS, 0).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"base":"Base text","naming":{"attempts":3,"prompt":"Name it"},"skills":{"review":"Review\n"}})
        );
        std::fs::write(base.join("skills.md"), "clash").unwrap();
        assert!(assemble(&[root.path().to_owned()], PROMPTS, 0).is_err());
    }
    #[test]
    fn shipped_prompts_assemble_with_required_sections() {
        let value = resolved_prompts_at(
            std::path::Path::new("/nonexistent"),
            None,
            std::path::Path::new("/nonexistent"),
        )
        .unwrap();
        for key in ["base_instructions", "closing_reporting"] {
            assert!(
                value[key].as_str().is_some_and(|text| !text.is_empty()),
                "{key}"
            );
        }
        assert!(value["session_naming"]["prompt"].is_string());
        assert!(value["tool_result_deduplication"]["placeholder"].is_string());
    }
}
