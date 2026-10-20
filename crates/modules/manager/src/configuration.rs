//! Agent-owned incoming-message settings; filesystem and storage remain in the host.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingMessage {
    pub enabled: bool,
    #[serde(default)]
    pub criteria: String,
}
impl IncomingMessage {
    pub fn validate(&self) -> Result<()> {
        if self.criteria.len() > 8192 || (self.enabled && self.criteria.trim().is_empty()) {
            bail!("Acceptance criteria are required when enabled and must be at most 8192 bytes");
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn criteria_bounds_and_enabled_state_are_preserved() {
        IncomingMessage::default().validate().unwrap();
        for criteria in ["".to_owned(),"  ".to_owned(),"x".repeat(8193)] {
            assert!(IncomingMessage {enabled:true,criteria}.validate().is_err());
        }
        IncomingMessage {enabled:true,criteria:"x".repeat(8192)}.validate().unwrap();
        assert!(IncomingMessage {enabled:false,criteria:"x".repeat(8193)}.validate().is_err());
        assert!(serde_json::from_str::<IncomingMessage>(r#"{"enabled":true,"criteria":"allowed","other":true}"#).is_err());
    }
}
