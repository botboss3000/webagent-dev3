//! Replaceable duplicate-result planner. Host storage and authority stay outside this library.
use anyhow::{bail, Context, Result};
use std::collections::{BTreeMap, BTreeSet};
pub use webagent_context_contracts::tool_results::*;
/// Produce stable assignments without executing, authorizing, recalling, or persisting anything.
pub fn project(request: &Request) -> Result<Plan> {
    if request.version != VERSION || request.observations.len() > MAX_OBSERVATIONS
        || request.state.assignments.len() > MAX_OBSERVATIONS * 2
        || request.state.last.len() > MAX_OBSERVATIONS * 2 {
        bail!("Duplicate-result projection bounds exceeded");
    }
    if request.enabled {
        validate_template(request.template.as_deref()
            .context("prompts.json: tool_result_deduplication.placeholder is required")?)?;
    } else if let Some(template) = &request.template { validate_template(template)?; }
    let mut seen = BTreeSet::new();
    let mut observations = BTreeMap::new();
    for item in &request.observations {
        if !valid_id(&item.call_id) || !valid_id(&item.tool) || !seen.insert(&item.call_id) {
            bail!("Invalid or ambiguous tool-result execution identity");
        }
        observations.insert(item.call_id.clone(), item);
    }
    let mut state = request.state.clone();
    for item in &request.observations {
        let identity = key(item)?;
        if let Some(existing) = state.assignments.get(&item.call_id) {
            if existing.key != identity || existing.output_sha256 != fingerprint(&item.output) { bail!("Tool-result execution identity changed"); }
            continue;
        }
        let previous = state.last.get(&identity).and_then(|id| observations.get(id));
        let previous_assignment = previous.and_then(|prior| state.assignments.get(&prior.call_id));
        // Only policy-controlled observations were guaranteed full model-facing bodies.
        // Seeded, disabled, and excluded observations may have been emitted as ordinary previews.
        let duplicate = request.enabled && item.included && item.success
            && previous_assignment.is_some_and(|entry|entry.template.is_some())
            && previous.is_some_and(|prior| prior.success && prior.included && prior.tool == item.tool && prior.inputs == item.inputs && prior.scope == item.scope
                && fingerprint(&prior.output) == fingerprint(&item.output) && prior.output == item.output);
        let (sequence, primary) = if duplicate {
            if let Some(prior) = previous_assignment {
                (prior.sequence.clone(), prior.primary.clone())
            } else { (item.call_id.clone(), item.call_id.clone()) }
        } else { (item.call_id.clone(), item.call_id.clone()) };
        let template = (request.enabled && item.included).then(|| request.template.clone()).flatten();
        let placeholder = if primary != item.call_id {
            replacement(item, &primary, template.as_deref().context("Duplicate template missing")?)?
        } else { None };
        state.assignments.insert(item.call_id.clone(), Assignment {
            key: identity.clone(), output_sha256: fingerprint(&item.output), sequence, primary, placeholder, template,
        });
        state.last.insert(identity, item.call_id.clone());
    }
    if request.rebase {
        let mut latest = BTreeMap::new();
        for item in &request.observations {
            let entry = &state.assignments[&item.call_id];
            if item.success { latest.insert(entry.sequence.clone(), item.call_id.clone()); }
        }
        for item in &request.observations {
            let entry = state.assignments.get_mut(&item.call_id).context("Assignment missing")?;
            if !item.success { continue; }
            let primary = latest.get(&entry.sequence).context("Sequence primary missing")?;
            entry.primary = primary.clone();
            entry.placeholder = if primary != &item.call_id {
                let template = request.template.as_deref().or(entry.template.as_deref());
                match template { Some(template) => replacement(item, primary, template)?, None => None }
            } else { None };
        }
        state.assignments.retain(|id, _| observations.contains_key(id));
        // Keep folded reset barriers for surviving keys, but do not accumulate fully folded keys forever.
        let present_keys:BTreeSet<_>=state.assignments.values().map(|entry|entry.key.clone()).collect();
        state.last.retain(|key,_|present_keys.contains(key));
        // Folding an unchanged last observation preserves the promoted primary; a different sequence remains a reset barrier.
        for last in state.last.values_mut() {
            if !observations.contains_key(last) {
                if let Some(previous)=request.state.assignments.get(last) {
                    if let Some(primary)=latest.get(&previous.sequence) { *last=primary.clone(); }
                }
            }
        }
    }
    let mut replacements = BTreeMap::new();
    for item in &request.observations {
        let entry = &state.assignments[&item.call_id];
        if let Some(text) = &entry.placeholder {
            let anchor = observations.get(&entry.primary).context("Duplicate reference has no full result in context")?;
            let anchor_entry = state.assignments.get(&entry.primary).context("Duplicate primary assignment missing")?;
            if !item.success || !anchor.success || entry.key != key(anchor)?
                || item.output != anchor.output || anchor_entry.placeholder.is_some()
                || entry.sequence != anchor_entry.sequence {
                bail!("Duplicate reference failed exact original-output validation");
            }
            replacements.insert(item.call_id.clone(), text.clone());
        }
    }
    if state.assignments.len() > MAX_OBSERVATIONS * 2 || state.last.len() > MAX_OBSERVATIONS * 2 {
        bail!("Duplicate-result state requires compaction");
    }
    let plan = Plan { version: VERSION, state, replacements };
    validate_plan(request, &plan)?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    const TEMPLATE: &str = "Call {current_call_id} executed; result equals {reference_call_id} from {tool}.";
    fn item(id: &str, path: &str, body: &str) -> Observation {
        Observation { call_id:id.into(), tool:"file_read".into(), inputs:json!({"path":path}),
            scope:json!({"session":"s1","workspace":"w1"}), output:body.repeat(1000), success:true, included:true }
    }
    fn request(items: Vec<Observation>) -> Request {
        Request { version:VERSION, observations:items, state:State::default(), enabled:true,
            template:Some(TEMPLATE.into()), rebase:false }
    }
    #[test]
    fn unrelated_reads_never_evict_and_only_later_identical_results_are_suppressed() {
        let r=request(vec![item("a","a.rs","x"),item("b","b.rs","y"),item("c","a.rs","x")]);
        let p=project(&r).unwrap();
        assert_eq!(p.replacements.len(),1);
        assert_eq!(p.state.assignments["c"].primary,"a");
        assert!(p.replacements.contains_key("c"));
    }
    #[test]
    fn changes_errors_inputs_and_scope_reset_independently() {
        let mut error=item("e","a","x");error.success=false;
        let mut scoped=item("s","a","x");scoped.scope=json!({"workspace":"other"});
        let r=request(vec![item("a","a","x"),item("b","a","y"),item("c","a","x"),
            error,item("f","a","x"),scoped,item("g","a","x")]);
        let p=project(&r).unwrap();
        assert_eq!(p.replacements.len(),1);
        assert_eq!(p.state.assignments["g"].primary,"f");
    }
    #[test]
    fn append_restart_and_configuration_edits_preserve_existing_prefix() {
        let first=request(vec![item("a","a","x"),item("b","a","x")]);
        let p=project(&first).unwrap();
        let serialized=serde_json::to_vec(&p.state).unwrap();
        let mut next=first.clone();next.state=serde_json::from_slice(&serialized).unwrap();
        next.template=Some("Changed {current_call_id} matches {reference_call_id}".into());
        next.observations.push(item("c","a","x"));
        let q=project(&next).unwrap();
        assert_eq!(q.replacements["b"],p.replacements["b"]);
        assert!(q.replacements["c"].contains("Changed"));
        next.state=q.state;next.enabled=false;next.template=None;
        next.observations.push(item("d","a","x"));
        let off=project(&next).unwrap();
        assert_eq!(off.replacements["b"],p.replacements["b"]);
        assert!(!off.replacements.contains_key("d"));
    }
    #[test]
    fn compaction_promotes_latest_survivor_with_direct_forward_references() {
        let mut r=request(vec![item("a","a","x"),item("b","a","x"),item("c","a","x")]);
        r.state=project(&r).unwrap().state;r.observations.remove(0);r.rebase=true;
        let p=project(&r).unwrap();
        assert_eq!(p.state.assignments["b"].primary,"c");
        assert!(p.replacements.contains_key("b"));assert!(!p.replacements.contains_key("c"));
        r.state=p.state;r.rebase=false;r.observations.push(item("d","a","x"));
        assert_eq!(project(&r).unwrap().state.assignments["d"].primary,"c");
    }
    #[test]
    fn folded_change_or_error_does_not_merge_distinct_equal_sequences() {
        let mut r=request(vec![item("a","a","x"),item("b","a","y"),item("c","a","x"),item("d","a","x")]);
        r.state=project(&r).unwrap().state;r.observations.remove(1);r.rebase=true;
        let p=project(&r).unwrap();assert!(!p.replacements.contains_key("a"));
        assert_eq!(p.state.assignments["c"].primary,"d");
        let mut e=item("e","a","x");e.success=false;
        let mut r=request(vec![item("a","a","x"),e,item("b","a","x")]);
        r.state=project(&r).unwrap().state;r.observations.remove(1);r.rebase=true;
        assert!(project(&r).unwrap().replacements.is_empty());
    }
    #[test]
    fn template_is_required_and_result_text_is_exact() {
        for invalid in ["", "{unknown}", "{current_call_id", "{current_call_id} {reference_call_id}}"] {
            assert!(validate_template(invalid).is_err());
        }
        let mut r=request(vec![item("a","a","x")]);r.template=None;assert!(project(&r).is_err());
        let mut b=item("b","a","x");b.output.push('\n');
        let r=request(vec![item("a","a","x"),b]);assert!(project(&r).unwrap().replacements.is_empty());
    }
    #[test]
    fn canonical_inputs_match_but_output_order_and_metadata_remain_significant() {
        let a=item("a","a","x");let mut b=a.clone();b.call_id="b".into();
        b.output=format!("{} timestamp",b.output);
        assert!(project(&request(vec![a,b])).unwrap().replacements.is_empty());
        assert_eq!(canonical(&json!({"a":1,"b":2})).unwrap(),canonical(&json!({"b":2,"a":1})).unwrap());
    }
    #[test]
    fn small_results_and_excluded_tools_remain_full_and_invalid_references_fail_closed() {
        let mut a=item("a","a","x");a.output="ok".into();let mut b=a.clone();b.call_id="b".into();
        assert!(project(&request(vec![a,b])).unwrap().replacements.is_empty());
        let mut b=item("b","a","x");b.included=false;
        assert!(project(&request(vec![item("a","a","x"),b])).unwrap().replacements.is_empty());
        let mut r=request(vec![item("a","a","x"),item("b","a","x")]);
        r.state=project(&r).unwrap().state;r.observations.remove(0);
        assert!(project(&r).is_err());
    }

    #[test]
    fn host_contract_rejects_forged_reference_text_and_changed_old_assignments() {
        let mut r=request(vec![item("a","a","x"),item("b","a","x")]);
        let p=project(&r).unwrap();let mut forged=p.clone();
        forged.state.assignments.get_mut("b").unwrap().placeholder=Some("forged text".into());
        forged.replacements.insert("b".into(),"forged text".into());assert!(validate_plan(&r,&forged).is_err());
        r.state=p.state.clone();let mut forged=p;forged.state.assignments.get_mut("a").unwrap().sequence="changed".into();
        assert!(validate_plan(&r,&forged).is_err());
    }


    #[test]
    fn seeded_observations_cannot_become_references_to_potential_previews() {
        let mut r=request(vec![item("seeded","a","x")]);r.enabled=false;r.template=None;
        r.state=project(&r).unwrap().state;r.enabled=true;r.template=Some(TEMPLATE.into());
        r.observations.push(item("first-full","a","x"));
        let full=project(&r).unwrap();assert!(!full.replacements.contains_key("first-full"));
        assert_eq!(full.state.assignments["first-full"].primary,"first-full");
        r.state=full.state;r.observations.push(item("later","a","x"));
        assert_eq!(project(&r).unwrap().state.assignments["later"].primary,"first-full");
    }

    #[test]
    fn toggle_and_allowlist_transitions_require_a_new_guaranteed_full_primary() {
        for global in [false,true] {
            let mut r=request(vec![item("a","a","x"),item("duplicate","a","x")]);
            let original=project(&r).unwrap();r.state=original.state;
            if global {r.enabled=false;r.template=None;}
            let mut excluded=item("excluded","a","x");excluded.included=global;
            r.observations.push(excluded);r.state=project(&r).unwrap().state;
            r.enabled=true;r.template=Some(TEMPLATE.into());
            r.observations.last_mut().unwrap().included=true;
            r.observations.push(item("new-full","a","x"));
            let p=project(&r).unwrap();assert!(!p.replacements.contains_key("new-full"));
            assert_eq!(p.state.assignments["new-full"].primary,"new-full");
            assert!(p.replacements.contains_key("duplicate"));
        }
    }

    #[test]
    fn folding_last_unchanged_observation_keeps_the_latest_surviving_primary() {
        let mut r=request(vec![item("a","a","x"),item("b","a","x"),item("c","a","x")]);
        r.state=project(&r).unwrap().state;r.observations.pop();r.rebase=true;
        let p=project(&r).unwrap();let key=p.state.assignments["b"].key.clone();
        assert_eq!(p.state.last[&key],"b");assert!(!p.replacements.contains_key("b"));
        r.state=p.state;r.rebase=false;r.observations.push(item("d","a","x"));
        assert_eq!(project(&r).unwrap().state.assignments["d"].primary,"b");
    }

    #[test]
    fn host_rejects_compaction_reply_that_keeps_the_old_primary() {
        let mut r=request(vec![item("a","a","x"),item("b","a","x")]);
        let old=project(&r).unwrap();r.state=old.state.clone();r.rebase=true;
        assert!(validate_plan(&r,&old).is_err());
        assert_eq!(project(&r).unwrap().state.assignments["a"].primary,"b");
    }

    #[test]
    fn compaction_prunes_absent_keys_but_preserves_folded_reset_barriers() {
        let mut r=request(vec![item("old-a","a","x"),item("old-b","b","x"),item("changed-a","a","y")]);
        r.state=project(&r).unwrap().state;
        let b_key=r.state.assignments["old-b"].key.clone();
        let a_key=r.state.assignments["old-a"].key.clone();
        r.observations.retain(|item|item.call_id=="old-a");r.rebase=true;
        let plan=project(&r).unwrap();assert!(!plan.state.last.contains_key(&b_key));
        assert_eq!(plan.state.last[&a_key],"changed-a");
        r.state=plan.state;r.rebase=false;r.observations.push(item("new-a","a","x"));
        assert!(project(&r).unwrap().replacements.is_empty());
    }

}
