//! Content-free call inclusion reports and a static boundary to the authoritative Abilities catalog.
use anyhow::Result;
use serde_json::Value;
use std::path::Path;
pub use webagent_context_contracts::abilities::{AbilityContext, AbilityInclusion, SkillInclusion};

/// Abilities retains source selection, classification, availability and native/plugin declarations.
/// The Engine borrows already-authorized views; implementations do not receive App or Engine state bundles.
pub trait Catalog: Clone + Send + Sync + 'static {
    fn is_discovery(name: &str) -> bool;
    fn model_index(entries: &[Value], schemas: &[Value]) -> String;
    fn all_schemas(&self) -> Vec<Value>;
    fn capability_guidance(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<webagent_capability_contracts::Guidance>;
    fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value>;
    fn context_inclusion(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<AbilityInclusion>;
    fn discover_with(
        &self,
        name: &str,
        args: &Value,
        run: &mut Value,
        policy: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Value;
    fn live_version(&self) -> String;
    fn plugin_binding(&self, name: &str) -> Option<Value>;
    fn published(&self) -> bool;
    fn schemas_with(
        &self,
        policy: &Value,
        loaded: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Vec<Value>;
    fn selected_state(&self, visibility: &Value, state: &Value, loaded: &Value) -> Value;
    fn snapshot_root(&self) -> &Path;
    fn source_root(&self) -> Option<&Path>;
    fn version(&self) -> &str;
    fn tool_gate(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> (bool, bool);
    fn tool_abilities(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> Vec<String>;
    fn tool_off(&self, name: &str, visibility: &Value) -> bool;
    fn native_snapshot_current(&self) -> Result<bool>;
    fn current(&self) -> Result<Self>;
}

impl<H: webagent_abilities::Host> Catalog for webagent_abilities::Catalog<H> {
    fn is_discovery(name: &str) -> bool {
        webagent_abilities::is_discovery(name)
    }
    fn model_index(entries: &[Value], schemas: &[Value]) -> String {
        webagent_abilities::Catalog::<H>::model_index(entries, schemas)
    }
    fn all_schemas(&self) -> Vec<Value> {
        webagent_abilities::Catalog::<H>::all_schemas(self)
    }
    fn capability_guidance(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<webagent_capability_contracts::Guidance> {
        webagent_abilities::Catalog::<H>::capability_guidance(
            self,
            entries,
            schemas,
            visibility,
            state,
            loaded,
            loaded_skills,
        )
    }
    fn catalog_with(&self, policy: &Value, visibility: &Value, state: &Value) -> Vec<Value> {
        webagent_abilities::Catalog::<H>::catalog_with(self, policy, visibility, state)
    }
    fn context_inclusion(
        &self,
        entries: &[Value],
        schemas: &[Value],
        visibility: &Value,
        state: &Value,
        loaded: &Value,
        loaded_skills: &Value,
    ) -> Vec<AbilityInclusion> {
        webagent_abilities::Catalog::<H>::context_inclusion(
            self,
            entries,
            schemas,
            visibility,
            state,
            loaded,
            loaded_skills,
        )
    }
    fn discover_with(
        &self,
        name: &str,
        args: &Value,
        run: &mut Value,
        policy: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Value {
        webagent_abilities::Catalog::<H>::discover_with(
            self, name, args, run, policy, visibility, state,
        )
    }
    fn live_version(&self) -> String {
        webagent_abilities::Catalog::<H>::live_version(self)
    }
    fn plugin_binding(&self, name: &str) -> Option<Value> {
        webagent_abilities::Catalog::<H>::plugin_binding(self, name)
    }
    fn published(&self) -> bool {
        webagent_abilities::Catalog::<H>::published(self)
    }
    fn schemas_with(
        &self,
        policy: &Value,
        loaded: &Value,
        visibility: &Value,
        state: &Value,
    ) -> Vec<Value> {
        webagent_abilities::Catalog::<H>::schemas_with(self, policy, loaded, visibility, state)
    }
    fn selected_state(&self, visibility: &Value, state: &Value, loaded: &Value) -> Value {
        webagent_abilities::Catalog::<H>::selected_state(self, visibility, state, loaded)
    }
    fn snapshot_root(&self) -> &Path {
        webagent_abilities::Catalog::<H>::snapshot_root(self)
    }
    fn source_root(&self) -> Option<&Path> {
        webagent_abilities::Catalog::<H>::source_root(self)
    }
    fn version(&self) -> &str {
        webagent_abilities::Catalog::<H>::version(self)
    }
    fn tool_gate(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> (bool, bool) {
        webagent_abilities::Catalog::<H>::tool_gate(self, name, visibility, state, loaded)
    }
    fn tool_abilities(
        &self,
        name: &str,
        visibility: &Value,
        state: &Value,
        loaded: &Value,
    ) -> Vec<String> {
        webagent_abilities::Catalog::<H>::tool_abilities(self, name, visibility, state, loaded)
    }
    fn tool_off(&self, name: &str, visibility: &Value) -> bool {
        webagent_abilities::Catalog::<H>::tool_off(self, name, visibility)
    }
    fn native_snapshot_current(&self) -> Result<bool> {
        webagent_abilities::Catalog::<H>::native_snapshot_current(self)
    }
    fn current(&self) -> Result<Self> {
        if let Some(root) = H::selected_source()? {
            webagent_abilities::Catalog::<H>::load_snapshot(&root.join("plugins/abilities"))
        } else {
            webagent_abilities::Catalog::<H>::load_current(self.snapshot_root())
        }
    }
}
