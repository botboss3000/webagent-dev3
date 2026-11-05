//! Media: application voice and image workflows. Owns dictation PCM sequencing, speech
//! formatting, the voice service (dictation recognizer and reply speaker) and its live settings,
//! the agent voice output catalog/setting and the Image Out receipt rules. Chat owns
//! voice/media preferences, Agents stores each agent's voice output setting and Models owns role
//! selections and provider wire formats; no `media.json` exists.
//! Root-bound adapters (App/Store, HTTP routes, model calls, the `generate_image` tool) live in
//! `host/` and are compiled by the root binary.
pub mod capture;
pub mod image;
pub mod recognizer;
pub mod settings;
pub mod speaker;
pub mod speech;

/// Run this process as the voice service for `role` (`dictation` or `speech`) with the
/// settings passed as JSON by the host, until its input closes.
pub fn voice_service(role: &str, settings: &str) -> Result<(), String> {
    let settings: settings::VoiceSettings =
        serde_json::from_str(settings).map_err(|error| error.to_string())?;
    let root = std::path::Path::new(recognizer::VOICE_ROOT);
    match role {
        "dictation" => recognizer::run(root, &settings.dictation),
        "speech" => speaker::run(root, &settings.speech),
        _ => Err(format!("unknown voice service role {role}")),
    }
}

#[cfg(test)]
mod tests;
