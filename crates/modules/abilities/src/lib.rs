//! Abilities infrastructure: validated catalog/package IO, activation projections and encrypted controls.
pub mod user_files;
mod catalog;
mod cache;
pub mod package;
pub mod selection;
mod ports;
pub mod controls;
pub use ports::Host;
pub use cache::CatalogCache;
pub use catalog::{Catalog,is_discovery,public_context,search_words,near_word,AbilityContext,AbilityInclusion,SkillInclusion};

#[cfg(feature="test-support")]
pub trait TestHost:Host {
 fn native_fixture()->Catalog<Self>;
 fn policy_fixture()->serde_json::Value;
}
#[cfg(feature="test-support")] pub use catalog::registered_tests as catalog_testing;
#[cfg(feature="test-support")] pub use package::registered_tests as plugins_testing;

#[cfg(test)]mod io_tests;
