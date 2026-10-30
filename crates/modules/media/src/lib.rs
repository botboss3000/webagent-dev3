//! Media: application voice and image workflows. Owns the transient voice lanes (generation,
//! expiry, capacity and capture state), PCM framing, spoken-utterance routing rules, speech
//! formatting and the Image Out receipt rules. Chat owns voice/media preferences and Models owns
//! role selections and provider wire formats; no `media.json` exists.
//! Root-bound adapters (App/Store, HTTP routes, model calls, the `generate_image` tool) live in
//! `host/` and are compiled by the root binary.
pub mod capture;
pub mod image;
pub mod lanes;
pub mod speech;
pub mod utterance;

#[cfg(test)]
mod tests;
