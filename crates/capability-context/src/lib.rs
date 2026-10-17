//! Deterministic composition of already authorized, freshly resolved capabilities.
//! This implementation has no filesystem, runtime, policy or credential dependency.
// @code-index {"version":1,"feature":"capability-composition","aliases":["live context","source correspondence"],"purpose":"Compose host-authorized current prompts and selected guidance deterministically, with temporary menus separate. Resolved configuration is input data; composition safety has no mutable settings.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"This pure implementation combines host-resolved inputs. Prompt and visibility settings remain at the host; composition must preserve mandatory text and cannot offer a configurable correspondence bypass."}}
use anyhow::Result;
use std::collections::BTreeSet;
pub use webagent_capability_contracts::{Definition, Guidance, GuidanceKind, Inclusion,
    InclusionReason, PrepareRequest, PreparedCapability};
use webagent_capability_contracts::{inclusion_reason, validate_request, validate_result, INTERFACE_VERSION};

pub const IMPLEMENTATION_VERSION: &str = "0.1.1";

/// Compose exact current strings and retain stable definition/schema ordering.
/// Missing/Off/denied assets must already be omitted by the host. A remembered
/// schema name never supplies a definition absent from permitted_schemas.
pub fn prepare(request: &PrepareRequest) -> Result<PreparedCapability> {
    validate_request(request)?;
    let mut main = String::with_capacity(request.base_prompt.len() + request.live_slots.len());
    main.push_str(&request.base_prompt);
    main.push_str(&request.live_slots);
    let mut definitions = Vec::new();
    let mut inclusion = Vec::with_capacity(request.guidance.len());
    for item in &request.guidance {
        let included = item.selected || item.natural;
        if included {
            definitions.push(Definition { id: item.id.clone(), kind: item.kind, body: item.body.clone() });
        }
        inclusion.push(Inclusion { id: item.id.clone(), kind: item.kind, included,
            reason: inclusion_reason(item), content_bytes: if included { item.body.len() } else { 0 } });
    }
    let permitted = request.permitted_schemas.iter().map(String::as_str).collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut schema_order = Vec::with_capacity(permitted.len());
    for name in request.prior_schema_order.iter().chain(&request.permitted_schemas) {
        if permitted.contains(name.as_str()) && seen.insert(name.as_str()) {
            schema_order.push(name.clone());
        }
    }
    let result = PreparedCapability { version: INTERFACE_VERSION,
        source_fingerprint: request.source_fingerprint.clone(), main, definitions,
        schema_order, temporary_menu: request.menu.clone(), inclusion };
    validate_result(request, &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Named source data keeps expected results independent from the algorithm.
    fn fixture() -> PrepareRequest {
        PrepareRequest { version: 1, source_fingerprint: "a".repeat(64),
            base_prompt: "BASE".into(), live_slots: "\nSLOT".into(), menu: "CURRENT MENU".into(),
            guidance: vec![
                Guidance { id: "ability:base".into(), kind: GuidanceKind::Ability,
                    body: "VISIBLE".into(), selected: false, natural: true },
                Guidance { id: "skill:base:own:guide".into(), kind: GuidanceKind::Own,
                    body: "SELECTED".into(), selected: true, natural: false },
                Guidance { id: "skill:base:selectable:later".into(), kind: GuidanceKind::Selectable,
                    body: "DISCOVERABLE".into(), selected: false, natural: false }],
            permitted_schemas: vec!["calculate".into(), "file_read".into(), "file_list".into()],
            prior_schema_order: vec!["revoked_tool".into(), "file_read".into()] }
    }

    /// Menus remain temporary; every natural/selected body is permanent and exact.
    #[test]
    fn separates_definitions_from_temporary_menu() {
        let result = prepare(&fixture()).unwrap();
        assert_eq!(result.main, "BASE\nSLOT");
        assert_eq!(result.definitions.iter().map(|item|item.body.as_str()).collect::<Vec<_>>(), vec!["VISIBLE", "SELECTED"]);
        assert_eq!(result.schema_order, vec!["file_read", "calculate", "file_list"]);
        assert_eq!(result.temporary_menu, "CURRENT MENU");
        assert!(!result.main.contains("CURRENT MENU"));
        assert!(result.definitions.iter().all(|item| !item.body.contains("CURRENT MENU")));
        assert_eq!(result.inclusion[2].reason, InclusionReason::Discoverable);
        assert_eq!(result.inclusion[2].content_bytes, 0);
    }

    /// Re-reading unchanged source yields byte-identical serialized output.
    #[test]
    fn unchanged_inputs_are_cache_stable() {
        let request = fixture();
        assert_eq!(serde_json::to_vec(&prepare(&request).unwrap()).unwrap(),
            serde_json::to_vec(&prepare(&request).unwrap()).unwrap());
    }

    /// Identity persists through an edit while the current complete body changes.
    #[test]
    fn current_body_and_main_prompt_replace_old_text() {
        let mut request = fixture();
        let old = prepare(&request).unwrap();
        request.guidance[1].body = "CURRENT π🙂".into();
        request.base_prompt = "NEW BASE".into();
        request.live_slots = "\nCURRENT SLOT".into();
        request.source_fingerprint = "b".repeat(64);
        let current = prepare(&request).unwrap();
        assert_eq!(current.definitions[1].id, old.definitions[1].id);
        assert_eq!(current.definitions[1].body, "CURRENT π🙂");
        assert_eq!(current.main, "NEW BASE\nCURRENT SLOT");
        assert_eq!(current.inclusion[1].content_bytes, "CURRENT π🙂".len());
        assert!(webagent_capability_contracts::validate_result(&request, &old).is_err());
    }

    /// Previously selected names never revive host-withdrawn guidance or schemas.
    #[test]
    fn withdrawal_uses_current_host_candidates() {
        let mut request = fixture();
        request.guidance.remove(1);
        request.permitted_schemas.retain(|name| name != "file_read");
        let result = prepare(&request).unwrap();
        assert_eq!(result.definitions.len(), 1);
        assert_eq!(result.schema_order, vec!["calculate", "file_list"]);
        assert!(!serde_json::to_string(&result).unwrap().contains("SELECTED"));
    }

    /// Large complete skills are not summarized or truncated by this boundary.
    #[test]
    fn preserves_full_guidance_beyond_small_preview_limits() {
        let mut request = fixture();
        request.guidance[1].body = "A complete verification instruction. π🙂\n".repeat(1000);
        let result = prepare(&request).unwrap();
        assert_eq!(result.definitions[1].body, request.guidance[1].body);
        assert_eq!(result.inclusion[1].content_bytes, request.guidance[1].body.len());
    }

    /// A visible and selected identity appears once and records both causes.
    #[test]
    fn visible_and_selected_are_not_duplicated() {
        let mut request = fixture();
        request.guidance[0].selected = true;
        let result = prepare(&request).unwrap();
        assert_eq!(result.definitions.len(), 2);
        assert_eq!(result.inclusion[0].reason, InclusionReason::NaturalAndSelected);
    }
}
