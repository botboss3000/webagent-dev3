//! Content-free call inclusion reports and a static boundary to the authoritative Abilities catalog.
use anyhow::Result;
use serde::{Deserialize,Serialize};
use serde_json::Value;
use std::path::Path;
/// Content-free evidence generated from the same authorized model-call assets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbilityContext {
    pub turn: u64,
    pub call: u64,
    pub abilities: Vec<AbilityInclusion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbilityInclusion {
    pub id: String,
    pub visibility: String,
    pub availability: String,
    pub guidance_bytes: usize,
    pub schemas: Vec<String>,
    pub skills: Vec<SkillInclusion>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillInclusion {
    pub kind: String,
    pub id: String,
    pub mode: String,
    pub included: bool,
    pub content_bytes: usize,
    pub reason: String,
}

/// Abilities retains source selection, classification, availability and native/plugin declarations.
/// The Engine borrows already-authorized views; implementations do not receive App or Engine state bundles.
pub trait Catalog:Clone+Send+Sync+'static {
 fn is_discovery(name:&str)->bool;
 fn model_index(entries:&[Value],schemas:&[Value])->String;
 fn all_schemas(&self)->Vec<Value>;
 fn capability_guidance(&self,entries:&[Value],schemas:&[Value],visibility:&Value,state:&Value,loaded:&Value,loaded_skills:&Value)->Vec<webagent_capability_contracts::Guidance>;
 fn catalog_with(&self,policy:&Value,visibility:&Value,state:&Value)->Vec<Value>;
 fn context_inclusion(&self,entries:&[Value],schemas:&[Value],visibility:&Value,state:&Value,loaded:&Value,loaded_skills:&Value)->Vec<AbilityInclusion>;
 fn discover_with(&self,name:&str,args:&Value,run:&mut Value,policy:&Value,visibility:&Value,state:&Value)->Value;
 fn live_version(&self)->String;
 fn plugin_binding(&self,name:&str)->Option<Value>;
 fn published(&self)->bool;
 fn schemas_with(&self,policy:&Value,loaded:&Value,visibility:&Value,state:&Value)->Vec<Value>;
 fn selected_state(&self,visibility:&Value,state:&Value,loaded:&Value)->Value;
 fn snapshot_root(&self)->&Path;
 fn source_root(&self)->Option<&Path>;
 fn version(&self)->&str;
 fn tool_gate(&self,name:&str,visibility:&Value,state:&Value,loaded:&Value)->(bool,bool);
 fn tool_abilities(&self,name:&str,visibility:&Value,state:&Value,loaded:&Value)->Vec<String>;
 fn native_snapshot_current(&self)->Result<bool>;
 fn current(&self)->Result<Self>;
}
