//! Shared host layer for module host crates.
//!
//! The root binary's `Store` wraps [`StoreCore`] and dereferences to it, so root code keeps
//! its field and method paths while module host crates add storage behaviour through
//! extension traits on `StoreCore`. Everything links into the one program.
mod error;
mod store;

pub use error::{bad, conflict, Api, ApiError};
pub use store::StoreCore;
pub use webagent_configuration::digest;

use std::time::{SystemTime, UNIX_EPOCH};

/// Current Unix time in whole seconds.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is after the Unix epoch")
        .as_secs()
}

/// A fresh random identifier.
pub fn uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Namespaced document key `prefix:id`.
pub fn key(prefix: &str, id: &str) -> String {
    format!("{prefix}:{id}")
}

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(test)]
mod tests;
