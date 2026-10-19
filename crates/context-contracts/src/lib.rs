//! Stable pure engine contracts and shared compaction policy. No runtime/provider/store dependencies.
#[path="input.rs"] pub mod context_input;
#[path="assembly.rs"] pub mod context_assembly;
#[path="accounting.rs"] pub mod context_accounting;
#[path="compaction.rs"] pub mod context_compaction;
pub mod protocol;

pub mod tool_results;
pub mod execution;
