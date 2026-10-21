//! Content-free inclusion report identity shared by Abilities and Engine.
use serde::{Deserialize,Serialize};
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
