//! Replaceable context algorithm; stable host contracts are a separate compilation boundary.
pub use webagent_context_contracts::{context_input,context_accounting,context_compaction};
#[path="assembly.rs"] pub mod context_assembly;
pub mod protocol;

/// Build-owned identity of the algorithm linked into this executable; live service metadata cannot replace it.
pub const IMPLEMENTATION_VERSION: &str = env!("CARGO_PKG_VERSION");

pub mod tool_results;
