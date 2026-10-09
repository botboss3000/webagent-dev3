//! Agent-owned settings, independent of the legacy V2 manager.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IncomingMessage {
    pub enabled: bool,
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
pub async fn incoming(store: &crate::store::Store, agent: &str) -> Result<IncomingMessage> {
    Ok(snapshot(store, agent).await?.0)
}
pub async fn snapshot(store: &crate::store::Store, agent: &str) -> Result<(IncomingMessage, u64)> {
    let view = store.for_agent(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(&view.agent_pool)
        .await?;
    let metadata = view.unseal(&blob)?;
    let value = &metadata["manager_incoming_message"];
    let settings = if value.is_null() {
        IncomingMessage::default()
    } else {
        serde_json::from_value(value.clone())?
    };
    settings.validate()?;
    Ok((
        settings,
        metadata["manager_incoming_message_revision"]
            .as_u64()
            .unwrap_or(0),
    ))
}
