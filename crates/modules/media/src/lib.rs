//! Media: application voice and image workflows. Owns dictation PCM sequencing, speech
//! formatting, the agent voice output catalog/setting and the Image Out receipt rules. Chat owns
//! voice/media preferences, Agents stores each agent's voice output setting and Models owns role
//! selections and provider wire formats; no `media.json` exists.
//! Root-bound adapters (App/Store, HTTP routes, model calls, the `generate_image` tool) live in
//! `host/` and are compiled by the root binary.
pub mod capture;
pub mod image;
pub mod speech;

#[cfg(test)]
mod tests;
