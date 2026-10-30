//! Image Out receipt rules for the `generate_image` tool. A durable started receipt is written
//! before a potentially billable request; its key also fixes the attachment identity so recovery
//! publishes the same attachment without another request.
use serde_json::{json, Value};

/// The tool this module executes.
pub const TOOL: &str = "generate_image";
/// Largest accepted prompt in bytes.
pub const MAX_PROMPT_BYTES: usize = 16384;
/// Largest saved base64 image accepted when recovering a publication.
pub const MAX_RECOVERY_DATA_BYTES: usize = 24 * 1024 * 1024;

/// True when `prompt` has visible text and fits [`MAX_PROMPT_BYTES`].
pub fn prompt_is_valid(prompt: &str) -> bool {
    !prompt.trim().is_empty() && prompt.len() <= MAX_PROMPT_BYTES
}

/// Receipt key for one tool invocation (run `rid`, pending call `pending`) in session `sid`.
pub fn receipt_key(sid: &str, rid: &str, pending: &str) -> String {
    format!(
        "receipt:core_image:{sid}:{}",
        webagent_configuration::digest(&json!([rid, pending]))
    )
}

/// Deterministic attachment UUID derived from a receipt key.
pub fn attachment_id(receipt: &str) -> String {
    let digest = webagent_configuration::digest(&json!(receipt));
    let bytes = hex::decode(&digest[..32]).expect("a digest is lowercase hex");
    uuid::Uuid::from_slice(&bytes)
        .expect("32 hex characters are 16 bytes")
        .to_string()
}

/// A failure before any provider request: report it plainly instead of as an uncertain effect.
pub fn not_sent(message: &str) -> Value {
    json!({"status": "error", "output": message})
}

/// An outcome that may have reached the provider; it must be reconciled, never replayed.
pub fn uncertain(receipt: &str, message: &str) -> Value {
    json!({
        "status": "uncertain",
        "uncertain": true,
        "output": {"receipt": receipt, "message": message},
    })
}

/// Attachment file name for a generated image type; `None` for unsupported types.
pub fn generated_filename(media_type: &str) -> Option<&'static str> {
    match media_type {
        "image/jpeg" => Some("generated.jpg"),
        "image/webp" => Some("generated.webp"),
        "image/gif" => Some("generated.gif"),
        "image/png" => Some("generated.png"),
        _ => None,
    }
}
