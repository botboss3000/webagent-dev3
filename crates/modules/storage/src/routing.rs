//! Namespace routing: each document key belongs to exactly one authority plane.
use anyhow::{anyhow, Result};
use sqlx::SqlitePool;

/// The three storage authorities.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Plane {
    Application,
    User,
    Agent,
}

/// Route each known namespace to its user, agent or application authority; conversation assets stay user-owned.
pub fn plane_for(key: &str) -> Plane {
    if key.starts_with("agent:") || key.starts_with("agent_abilities:") {
        Plane::Agent
    } else if key == "migration:conversation_privacy_v1"
        || key.starts_with("module_state:")
        || key.starts_with("core_source_control:")
        || key.starts_with("core_vault:")
        || key.starts_with("receipt:core_vault:")
        || key.starts_with("core_visual:")
        || key.starts_with("core_tool:")
        || key.starts_with("core_profile:")
        || key.starts_with("core_rating:")
        || key.starts_with("core_webhook:")
        || key.starts_with("core_webhook_log:")
        || key.starts_with("core_spawn_origin:")
        || key.starts_with("base_checkpoint:")
        || key.starts_with("base_component:")
        || key.starts_with("receipt:base:")
        || key.starts_with("receipt:core_image:")
        || key.starts_with("receipt:module:")
        || key.starts_with("session:")
        || key.starts_with("manager_review:")
        || key.starts_with("manager_pending:")
        || key.starts_with("followup_queue:")
        || key.starts_with("messaging_route:")
        || key.starts_with("receipt:agent_message:")
        || key.starts_with("receipt:messaging_route:")
        || key.starts_with("receipt:queue:")
        || key.starts_with("user_agent_page_preferences:")
        || key.starts_with("user_app_page_preferences:")
        || key.starts_with("message_meta:")
        || key.starts_with("shared_help:")
        || key.starts_with("receipt:shared_help:")
        || key.starts_with("receipt:participation:")
        || key.starts_with("session_mode:")
        || key.starts_with("session_chat_controls:")
        || key.starts_with("session_naming:")
        || key.starts_with("receipt:chat_controls:")
        || key.starts_with("compaction_segment:")
        || key.starts_with("session_ability_state:")
        || key.starts_with("session_context_assets:")
        || key.starts_with("action:")
        || key.starts_with("background_task:")
        || key.starts_with("run:")
        || key.starts_with("run_view:")
        || key.starts_with("administrator_receipt:")
        || key.starts_with("administrator_browser:")
        || key.starts_with("administrator_host:")
        || key.starts_with("administrator_notice:")
        || key.starts_with("attachment:")
        || key.starts_with("session_usage:")
        || key == "attachment_quota"
        || key.starts_with("message_attachments:")
        || key.starts_with("memory:")
        || key.starts_with("promotion:")
        || key.starts_with("receipt:session:")
        || key.starts_with("receipt:direct_message:")
        || key.starts_with("receipt:message:")
        || key.starts_with("receipt:run:")
        || key.starts_with("receipt:session_mode:")
        || key.starts_with("receipt:approval:")
    {
        Plane::User
    } else {
        Plane::Application
    }
}

/// User-plane namespaces that stay AEAD-encrypted even when the administrator
/// plaintext policy is enabled.
pub fn always_sealed(key: &str) -> bool {
    key.starts_with("core_source_control:")
        || key.starts_with("module_state:")
        || key.starts_with("receipt:module:")
        || key.starts_with("session_context_assets:")
}

/// Reject document access naming an agent other than the handle's bound agent.
pub fn validate_agent_doc_key(agent_id: &str, key: &str) -> Result<()> {
    if let Some(rest) = key.strip_prefix("agent:") {
        if rest != agent_id && !rest.starts_with(&format!("{agent_id}:")) {
            return Err(anyhow!("Agent view mismatch"));
        }
    }
    if let Some(rest) = key.strip_prefix("agent_abilities:") {
        if rest != agent_id {
            return Err(anyhow!("Agent view mismatch"));
        }
    }
    Ok(())
}

/// Borrowed pools of one bound storage view.
#[derive(Clone, Copy)]
pub struct Planes<'a> {
    pub application: &'a SqlitePool,
    pub user: &'a SqlitePool,
    pub agent: &'a SqlitePool,
}

impl<'a> Planes<'a> {
    /// Select the pool that owns `key`.
    pub fn for_key(&self, key: &str) -> &'a SqlitePool {
        match plane_for(key) {
            Plane::Agent => self.agent,
            Plane::User => self.user,
            Plane::Application => self.application,
        }
    }
}
