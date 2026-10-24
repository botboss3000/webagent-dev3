//! Chat-owned execution-mode selection and confirmation preferences.
//! Named mode definitions belong to the Agents module.
use anyhow::{bail, Result};
use serde_json::Value;
use std::path::Path;

pub struct ChatModePreferences {
    pub default_mode: Option<String>,
    pub confirm_auto_mode: bool,
}

pub async fn chat_preferences(path: &Path) -> Result<ChatModePreferences> {
    let fallback = ChatModePreferences {
        default_mode: None,
        confirm_auto_mode: true,
    };
    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(fallback),
        Err(error) => return Err(error.into()),
    };
    let document: Value = serde_json::from_slice(&bytes)?;
    let settings = &document["chat_common"]["execution_mode"];
    if settings.is_null() {
        return Ok(fallback);
    }
    let object = settings
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("Invalid chat execution mode configuration"))?;
    let default_mode = match object.get("default_mode") {
        None => None,
        Some(Value::String(mode)) if !mode.is_empty() && mode.len() <= 32 => Some(mode.clone()),
        _ => bail!("Invalid chat default execution mode"),
    };
    let confirm_auto_mode = match object.get("confirm_auto_mode") {
        None => true,
        Some(Value::Bool(value)) => *value,
        _ => bail!("Invalid Auto mode confirmation setting"),
    };
    Ok(ChatModePreferences {
        default_mode,
        confirm_auto_mode,
    })
}
