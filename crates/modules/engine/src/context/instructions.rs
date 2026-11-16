//! Engine behavioral prompt policy; Models receives prepared instructions.
//! An agent's instructions are its prompt slots; general rules live in the core Base skill.
use anyhow::Result;
const LEGACY_DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. When asked what abilities or tools are available, check list_abilities; an empty loaded list does not mean the catalog is empty. When a task needs workspace access, search list_abilities using a few task words (or no query for the full permitted catalog), then load_ability to reveal its tools. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
/// The seed before the checkout moved to /code (2026-11-06); stored prompts may still start with it.
const PREVIOUS_DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. Visible and activated abilities already supply their configured guidance and tools; use them directly without loading again. list_abilities shows inactive discoverable abilities by default; include_available=true shows the full permitted catalog with availability states. Load only a discoverable ability needed for the task. Tool and skill visibility are independent: use list_tools/load_tool for exact discoverable tools or search_tools for ranked discovery and list_skills/load_skill for discoverable guidance. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
pub use webagent_context_contracts::compatibility::ENGINE_INSTRUCTION_SEED as DEFAULT_INSTRUCTIONS;
/// The run-specific slots after whichever compiled seed a stored prompt starts with.
fn without_seed(pinned: &str) -> &str {
    [
        DEFAULT_INSTRUCTIONS,
        PREVIOUS_DEFAULT_INSTRUCTIONS,
        LEGACY_DEFAULT_INSTRUCTIONS,
    ]
    .iter()
    .find_map(|seed| pinned.strip_prefix(seed))
    .unwrap_or(pinned)
}
/// The agent's prompt slots; empty for an agent with none.
pub fn instructions_for_turn(pinned: &str) -> String {
    without_seed(pinned).trim_start().to_owned()
}
/// The agent's prompt slots, without any compiled seed a stored prompt starts with.
/// `_root` and `_source` no longer hold prompt files; callers keep passing them.
pub fn instructions_for_snapshot(
    pinned: &str,
    _root: &std::path::Path,
    _source: Option<&std::path::Path>,
) -> Result<String> {
    Ok(instructions_for_turn(pinned))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_compiled_seed_is_replaced_by_the_live_prompt() {
        for seed in [
            DEFAULT_INSTRUCTIONS,
            PREVIOUS_DEFAULT_INSTRUCTIONS,
            LEGACY_DEFAULT_INSTRUCTIONS,
        ] {
            assert_eq!(without_seed(&format!("{seed}\n\nslot")), "\n\nslot");
        }
        assert!(DEFAULT_INSTRUCTIONS.contains("start in /code,"));
        assert_eq!(without_seed("custom"), "custom");
        assert_eq!(
            instructions_for_turn(&format!("{DEFAULT_INSTRUCTIONS}\n\nslot")),
            "slot"
        );
        assert_eq!(instructions_for_turn(""), "");
    }
}
