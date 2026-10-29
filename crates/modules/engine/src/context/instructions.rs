//! Engine behavioral prompt policy; Models receives prepared instructions.
use anyhow::{anyhow, Result};
const LEGACY_DEFAULT_INSTRUCTIONS: &str = "You are WebAgent, a helpful assistant. Use only the tools made available to you. When asked what abilities or tools are available, check list_abilities; an empty loaded list does not mean the catalog is empty. When a task needs workspace access, search list_abilities using a few task words (or no query for the full permitted catalog), then load_ability to reveal its tools. For development source work, file tools, approved shell and Python commands start in /live-checkout, the independent V3 source root. Use Codebase Admin file tools without task_id for explicit direct/live file edits and runtime configuration. For application source changes intended for a build or release, supply one stable task_id to file tools; they automatically create an isolated broker task. Inspect that task before editing, use its latest input digest for writes, and use build_work review/check/preflight. Only explicit authorized build_work finish deploys; task edits alone never deploy. V2 source is outside this container mount. Their reach is determined by container OS permissions; an empty session workspace does not mean source is unavailable. In read-only mode explain that edits require a write-capable mode. When ranking many candidate tools, files, or abilities, prefer search_tools or score_directory_with_jev when a Decision model is assigned, instead of loading full catalogs or listings into context.";
pub use webagent_context_contracts::compatibility::ENGINE_INSTRUCTION_SEED as DEFAULT_INSTRUCTIONS;
pub fn instructions_for_turn(pinned: &str) -> String {
    let slots = pinned
        .strip_prefix(DEFAULT_INSTRUCTIONS)
        .or_else(|| pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS))
        .unwrap_or(pinned);
    format!("{}{}", configured_base_instructions(), slots)
}
/// Prompt files are live: the checkout's edits apply on the next call, ahead of the
/// release snapshot `source`. `_root` (the state root) holds no prompt overrides.
pub fn instructions_for_snapshot(
    pinned: &str,
    _root: &std::path::Path,
    source: Option<&std::path::Path>,
) -> Result<String> {
    let value = crate::defaults::runtime_prompts_at(source)?;
    let mut parts = Vec::new();
    for key in ["base_instructions", "closing_reporting"] {
        let text = value[key]
            .as_str()
            .ok_or_else(|| anyhow!("Invalid live base prompt"))?
            .trim();
        if !text.is_empty() {
            parts.push(text.to_owned());
        }
    }
    let slots = pinned
        .strip_prefix(DEFAULT_INSTRUCTIONS)
        .or_else(|| pinned.strip_prefix(LEGACY_DEFAULT_INSTRUCTIONS))
        .unwrap_or(pinned);
    Ok(format!("{}{}", parts.join("\n\n"), slots))
}
/// The live base prompt files composed for a turn; the compiled seed if they cannot be read.
pub fn configured_base_instructions() -> String {
    instructions_for_snapshot("", std::path::Path::new("."), None).unwrap_or_else(|error| {
        eprintln!("Warning: could not load the live base prompts: {error}");
        DEFAULT_INSTRUCTIONS.to_string()
    })
}
