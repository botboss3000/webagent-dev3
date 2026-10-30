//! Abilities infrastructure: validated catalog/package IO, activation projections and encrypted controls.
pub mod ability_access;
pub mod base;
mod cache;
mod catalog;
pub mod controls;
pub mod drift;
pub mod package;
mod ports;
pub mod selection;
pub mod tool_definitions;
pub mod user_files;
pub use cache::CatalogCache;
pub use catalog::{
    is_discovery, near_word, public_context, search_words, AbilityContext, AbilityInclusion,
    Catalog, SkillInclusion,
};
pub use ports::Host;

#[cfg(feature = "test-support")]
pub trait TestHost: Host {
    fn native_fixture() -> Catalog<Self>;
    fn policy_fixture() -> serde_json::Value;
}
#[cfg(feature = "test-support")]
pub use catalog::registered_tests as catalog_testing;
#[cfg(feature = "test-support")]
pub use package::registered_tests as plugins_testing;

#[cfg(test)]
mod io_tests;
