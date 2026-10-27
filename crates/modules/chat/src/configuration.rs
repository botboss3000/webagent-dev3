//! Chat-owned execution-mode preferences, attachment limits and Main Chat skill prompt loading.
//! Presentation fields of `config/chat.json` belong to Webapp; Named mode definitions belong to Agents.
use anyhow::{anyhow, bail, Result};
use serde_json::Value;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatModePreferences {
    pub default_mode: Option<String>,
    pub confirm_auto_mode: bool,
}

pub fn parse_chat_preferences(document: &Value) -> Result<ChatModePreferences> {
    let fallback = ChatModePreferences {
        default_mode: None,
        confirm_auto_mode: true,
    };
    let settings = &document["chat_common"]["execution_mode"];
    if settings.is_null() {
        return Ok(fallback);
    }
    let object = settings
        .as_object()
        .ok_or_else(|| anyhow!("Invalid chat execution mode configuration"))?;
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

/// Read the installation chat.json on every use; per-agent overlays merge over this base.
/// A missing file means defaults; an unreadable or invalid one fails closed rather than
/// falling back to a compiled default.
/// Chat's live file (`live-files.json`), relative to the checkout.
pub const LIVE_CHAT: &str = "ui/chat/chat.json";

pub async fn read_chat_document(path: &Path) -> Result<Value> {
    let valid = |bytes: &[u8]| serde_json::from_slice::<Value>(bytes).map(|_| ()).map_err(Into::into);
    match webagent_configuration::live::read_path(path, 4 * 1024 * 1024, valid) {
        Ok(bytes) => {
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("Chat configuration invalid"))
        }
        Err(error) if webagent_configuration::live::missing(&error) => {
            Ok(Value::Object(Default::default()))
        }
        Err(_) => Err(anyhow!("Chat configuration unavailable")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AttachmentLimits {
    pub max_file_size_mb: Option<usize>,
    pub max_per_message: Option<usize>,
}

impl AttachmentLimits {
    pub fn bytes(self) -> Option<usize> {
        self.max_file_size_mb.map(|mb| mb * 1024 * 1024)
    }
}

pub fn parse_attachment_limits(config: &Value) -> Result<AttachmentLimits> {
    if !config["chat_limits"].is_null() && !config["chat_limits"].is_object() {
        bail!("Chat limits configuration invalid");
    }
    let settings = &config["chat_limits"]["attachments"];
    if !settings.is_null() && !settings.is_object() {
        bail!("Attachment configuration invalid");
    }
    let number = |name: &str| -> Result<Option<usize>> {
        if settings[name].is_null() {
            return Ok(None);
        }
        let value = settings[name]
            .as_u64()
            .filter(|n| *n > 0)
            .and_then(|n| usize::try_from(n).ok())
            .ok_or_else(|| anyhow!("Attachment {name} must be a positive integer or null"))?;
        if name == "max_file_size_mb" && value.checked_mul(1024 * 1024).is_none() {
            bail!("Attachment size exceeds machine address range");
        }
        Ok(Some(value))
    };
    Ok(AttachmentLimits {
        max_file_size_mb: number("max_file_size_mb")?,
        max_per_message: number("max_per_message")?,
    })
}

/// Validate Chat-owned sections of `config/chat.json` while preserving Webapp presentation siblings.
pub fn validate_chat_config(value: &Value) -> Result<()> {
    if !value.is_object() {
        bail!("Configuration must be an object");
    }
    parse_attachment_limits(value).map_err(|_| anyhow!("Invalid attachment limit"))?;
    if !value["chat_common"].is_null() && !value["chat_common"].is_object() {
        bail!("Invalid chat_common configuration");
    }
    if !value["chat_common"]["execution_mode"].is_null() {
        parse_chat_preferences(value)?;
    }
    Ok(())
}

/// Main Chat guidance from the live prompt files (`prompts/main_chat_skills/*.md`),
/// or from a pinned candidate `source`.
pub fn main_chat_prompts_at(
    root: &Path,
    source: Option<&Path>,
    fallback_directory: &Path,
) -> Result<Value> {
    let prompts = webagent_configuration::prompts::resolved_prompts_at(root, source, fallback_directory)?;
    Ok(prompts.get("main_chat_skills").cloned().unwrap_or(Value::Null))
}
