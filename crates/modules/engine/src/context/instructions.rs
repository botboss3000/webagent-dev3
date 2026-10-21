//! Engine behavioral prompt policy; Models receives prepared instructions.
use anyhow::{anyhow,Result};
use serde_json::Value;
use std::{sync::{Mutex,OnceLock},time::UNIX_EPOCH};
const LEGACY_DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. When asked what abilities or tools are available, check list_abilities; an empty loaded list does not mean the catalog is empty. When a task needs workspace access, search list_abilities using a few task words (or no query for the full permitted catalog), then load_ability to reveal its tools. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
pub const DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. Visible and activated abilities already supply their configured guidance and tools; use them directly without loading again. list_abilities shows inactive discoverable abilities by default; include_available=true shows the full permitted catalog with availability states. Load only a discoverable ability needed for the task. Tool and skill visibility are independent: use list_tools/load_tool for exact discoverable tools or search_tools for ranked discovery and list_skills/load_skill for discoverable guidance. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
pub fn instructions_for_turn(pinned: &str) -> String {
        let slots = pinned.strip_prefix(DEFAULT_INSTRUCTIONS)
            .or_else(||pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS)).unwrap_or(pinned);
        format!("{}{}", configured_base_instructions(), slots)
    }
pub fn instructions_for_snapshot(pinned:&str,root:&std::path::Path,source:Option<&std::path::Path>)->Result<String> {
        let value=crate::defaults::resolved_prompts_at(root,source)?;
        let mut parts=Vec::new();
        for key in ["base_instructions","closing_reporting","jev_guidance"] {
            let text=value[key].as_str().ok_or_else(||anyhow!("Invalid live base prompt"))?.trim();
            if !text.is_empty(){parts.push(text.to_owned());}
        }
        let slots=pinned.strip_prefix(DEFAULT_INSTRUCTIONS).or_else(||pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS)).unwrap_or(pinned);
        Ok(format!("{}{}",parts.join("\n\n"),slots))
    }
pub fn configured_base_instructions() -> String {
        static CACHE: OnceLock<Mutex<(Option<(u128, u128)>, String)>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new((None, DEFAULT_INSTRUCTIONS.to_string())));
        let mut cache = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = std::path::Path::new("config/prompts.json");
        let loaded = (|| -> Result<((u128, u128), String)> {
            let metadata = std::fs::metadata(path)?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
            let bytes = serde_json::to_vec(&crate::defaults::resolved_prompts(std::path::Path::new("."))?)?;
            let hash = bytes.iter().fold(0u128, |state, byte| state.wrapping_mul(31).wrapping_add(u128::from(*byte)));
            let freshness = (modified, hash);
            if cache.0 == Some(freshness) {
                return Ok((freshness, cache.1.clone()));
            }
            let value: Value = serde_json::from_slice(&bytes)?;
            let mut parts = Vec::new();
            for key in ["base_instructions", "closing_reporting", "jev_guidance"] {
                let text = value.get(key).and_then(Value::as_str)
                    .ok_or_else(|| anyhow!("Invalid prompt field: {key}"))?.trim();
                if !text.is_empty() { parts.push(text.to_owned()); }
            }
            Ok((freshness, parts.join("\n\n")))
        })();
        match loaded {
            Ok((freshness, composed)) => {
                cache.0 = Some(freshness);
                cache.1 = composed;
            }
            Err(error) => eprintln!("Warning: could not load config/prompts.json: {error}"),
        }
        cache.1.clone()
    }
