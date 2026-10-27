//! Manager-owned encrypted settings: each agent's incoming-message gate and criteria live in its record.
use anyhow::{bail, Result};
use crate::storage::{Repository,RecordEffects};
use serde_json::Value;
pub use crate::configuration::IncomingMessage;
pub async fn incoming(store: &Repository<'_,impl RecordEffects>, agent: &str) -> Result<IncomingMessage> {
    Ok(snapshot(store, agent).await?.0)
}
pub async fn snapshot(store: &Repository<'_,impl RecordEffects>, agent: &str) -> Result<(IncomingMessage, u64)> {
    let pool = store.effects.agent_pool(agent).await?;
    let blob: Vec<u8> = sqlx::query_scalar("SELECT metadata FROM agents WHERE id=?")
        .bind(agent)
        .fetch_one(&pool)
        .await?;
    let metadata = store.effects.unseal_agent(agent,&blob)?;
    let value = &metadata["manager_incoming_message"];
    let settings: IncomingMessage = if value.is_null() {
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

