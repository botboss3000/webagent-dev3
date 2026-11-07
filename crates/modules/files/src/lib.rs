//! Files: upload/attachment validation, provenance, encrypted attachment and link records, quota
//! accounting and content delivery policy. Chat owns the `chat.json` attachment limits consumed here;
//! Storage keeps pools, keys and the record codec. Per-tier storage limits live in `storage-policy.json`.
//! Root-bound adapters (App/Store, HTTP routes, the File Manager admission route) live in `host/`
//! and are compiled by the root binary. The isolated worker keeps File Manager checkout confinement.
pub mod attachments;
pub mod repository;
pub mod storage_policy;
pub mod uploads;

pub use webagent_chat::RecordCodec;

/// Encrypted attachment record, created by upload or inline admission.
pub fn attachment_key(id: &str) -> String {
    format!("attachment:{id}")
}

/// Public attachment metadata linked to the user message of one run.
pub fn message_files_key(run_id: &str) -> String {
    format!("message_attachments:{run_id}")
}

/// Files a tool produced for the agent reply of one run, such as browser screenshots.
/// The record carries its `session_id`, so deleting the session deletes it.
pub fn reply_files_key(run_id: &str) -> String {
    format!("reply_attachments:{run_id}")
}

/// Running byte total of stored uploads in this user plane.
pub const QUOTA_KEY: &str = "attachment_quota";

/// Running byte total of this user's uploads in sessions of one agent.
pub fn agent_quota_key(agent_id: &str) -> String {
    format!("{QUOTA_KEY}:{agent_id}")
}

#[cfg(test)]
mod tests;
