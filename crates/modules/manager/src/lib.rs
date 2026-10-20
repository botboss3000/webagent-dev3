//! Pure manager settings and incoming-message admission protocol; the host retains authority and effects.
pub mod configuration;
pub mod incoming_message;
pub mod verdict;
pub use verdict::Verdict;
