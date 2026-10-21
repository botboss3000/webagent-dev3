//! Engine-context prompt and projection composition; all provider IO is owned by Models.
use anyhow::{anyhow,Result};
use serde_json::{json,Value};
use std::sync::{Mutex,OnceLock};
use std::time::UNIX_EPOCH;
use tokio_util::sync::CancellationToken;
pub use webagent_models::provider::{ProviderId,ProviderSelection};
pub use webagent_models::transport::{ProviderFailure,typed_failure,projection_id};
const LEGACY_DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. When asked what abilities or tools are available, check list_abilities; an empty loaded list does not mean the catalog is empty. When a task needs workspace access, search list_abilities using a few task words (or no query for the full permitted catalog), then load_ability to reveal its tools. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
pub const DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. Visible and activated abilities already supply their configured guidance and tools; use them directly without loading again. list_abilities shows inactive discoverable abilities by default; include_available=true shows the full permitted catalog with availability states. Load only a discoverable ability needed for the task. Tool and skill visibility are independent: use list_tools/load_tool for exact discoverable tools or search_tools for ranked discovery and list_skills/load_skill for discoverable guidance. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
#[derive(Clone)]
pub struct Provider(pub(crate) webagent_models::transport::Provider);
impl std::ops::Deref for Provider {
    type Target=webagent_models::transport::Provider;
    fn deref(&self)->&Self::Target {&self.0}
}
impl Default for Provider {fn default()->Self {Self::new()}}
impl Provider {
    pub fn new()->Self {Self(webagent_models::transport::Provider::new())}
    #[cfg(test)]
    pub fn for_test(endpoint:String)->Self {Self(webagent_models::transport::Provider::for_test(endpoint))}
    pub(crate) fn instructions_for_turn(pinned: &str) -> String {
        let slots = pinned.strip_prefix(DEFAULT_INSTRUCTIONS)
            .or_else(||pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS)).unwrap_or(pinned);
        format!("{}{}", Self::configured_base_instructions(), slots)
    }
    pub(crate) fn instructions_for_snapshot(pinned:&str,root:&std::path::Path,source:Option<&std::path::Path>)->Result<String> {
        let value=crate::live_assets::resolved_prompts_at(root,source)?;
        let mut parts=Vec::new();
        for key in ["base_instructions","closing_reporting","jev_guidance"] {
            let text=value[key].as_str().ok_or_else(||anyhow!("Invalid live base prompt"))?.trim();
            if !text.is_empty(){parts.push(text.to_owned());}
        }
        let slots=pinned.strip_prefix(DEFAULT_INSTRUCTIONS).or_else(||pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS)).unwrap_or(pinned);
        Ok(format!("{}{}",parts.join("\n\n"),slots))
    }
    pub(crate) fn configured_base_instructions() -> String {
        static CACHE: OnceLock<Mutex<(Option<(u128, u128)>, String)>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new((None, DEFAULT_INSTRUCTIONS.to_string())));
        let mut cache = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let path = std::path::Path::new("config/prompts.json");
        let loaded = (|| -> Result<((u128, u128), String)> {
            let metadata = std::fs::metadata(path)?;
            let modified = metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos();
            let bytes = serde_json::to_vec(&crate::live_assets::resolved_prompts(std::path::Path::new("."))?)?;
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
}
struct RootProjection<'a>(&'a std::path::Path);
impl webagent_models::transport::Projection for RootProjection<'_> {
    fn project<'a>(&'a self,input:&'a webagent_provider_contracts::RequestV1,cancel:&'a CancellationToken)
        ->impl std::future::Future<Output=Result<webagent_provider_contracts::ResultV1>>+Send+'a {
        crate::engine_components::provider_project(self.0,input,cancel)
    }
}
pub(crate) struct CallDraft<'a>(webagent_models::transport::CallDraft<'a>);
impl<'a> CallDraft<'a> {
    pub(crate) fn new(selection:&'a ProviderSelection,input:Vec<Value>,tools:Vec<Value>,session:&'a str,instructions:&'a str)->Self {
        Self(webagent_models::transport::CallDraft::new(selection,input,tools,session,instructions))
    }
    pub(crate) async fn prepare(self,root:&std::path::Path,cancel:&CancellationToken)->Result<webagent_models::transport::call::ValidatedCall<'a>> {
        self.0.prepare(&RootProjection(root),cancel).await
    }
    #[cfg(any(feature="linked-context",test))]
    pub(crate) fn validate(self)->Result<webagent_models::transport::call::ValidatedCall<'a>> {
        self.0.validate_with(webagent_provider_projection::project)
    }
}
#[cfg(test)]
mod tests {use super::*;
    #[test]
    fn configured_base_instructions_loads_and_recovers_from_file_changes() {
        let path = "config/prompts.json";
        let original = std::fs::read(path).expect("read original prompts file");
        let original_metadata = std::fs::metadata(path).expect("read prompt metadata");
        let original_permissions = original_metadata.permissions();
        let original_modified = original_metadata.modified().expect("read prompt modification time");
        struct RestorePrompts(Vec<u8>, std::fs::Permissions, std::time::SystemTime);
        impl Drop for RestorePrompts {
            /// Restore the original prompt fixture even if the prompt-cache assertion unwinds.
            fn drop(&mut self) {
                let _ = std::fs::write("config/prompts.json", &self.0);
                let _ = std::fs::set_permissions("config/prompts.json", self.1.clone());
                // This compile input must keep its original freshness after the fixture.
                if let Ok(file) = std::fs::File::options().write(true).open("config/prompts.json") {
                    let _ = file.set_times(std::fs::FileTimes::new().set_modified(self.2));
                }
            }
        }
        let restore = RestorePrompts(original.clone(), original_permissions.clone(), original_modified);
        let good = json!({
            "base_instructions": "BASE_MARKER",
            "closing_reporting": "CLOSING_MARKER",
            "jev_guidance": "JEV_MARKER"
        });
        std::fs::write(path, good.to_string()).expect("write valid prompts file");
        let last_good = Provider::configured_base_instructions();
        assert!(last_good.contains("BASE_MARKER"));
        assert!(last_good.contains("CLOSING_MARKER"));
        assert!(last_good.contains("JEV_MARKER"));

        std::fs::write(path, "{ malformed json").expect("write malformed prompts file");
        assert_eq!(Provider::configured_base_instructions(), last_good);
        std::fs::remove_file(path).expect("remove prompts file");
        assert_eq!(Provider::configured_base_instructions(), last_good);

        std::fs::write(path, &original).expect("restore original prompts file");
        assert_eq!(std::fs::read(path).expect("re-read restored prompts file"), original);
        drop(restore);
        Provider::configured_base_instructions();
        assert_eq!(std::fs::read(path).expect("verify restored prompts file"), original);
        assert_eq!(std::fs::metadata(path).expect("verify restored prompt permissions").permissions(), original_permissions);
        assert_eq!(std::fs::metadata(path).expect("verify restored prompt timestamp").modified().unwrap(), original_modified);
    }
}
