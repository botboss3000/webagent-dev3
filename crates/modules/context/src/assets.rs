//! Materialization of validated plans; execution authority and durable history remain with the caller.
use crate::input as context_input;
// Materialize live guidance and permanent conversation selectors through bounded component plans; native host retains authorization and canonical history.
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_util::sync::CancellationToken;
use webagent_assets_contracts::{Operation, Plan, Request, State};
use webagent_capability_contracts::{Guidance, PrepareRequest, PreparedCapability};
use webagent_provider_contracts::ProfileV1;

pub struct Working {
    pub instructions: String,
    pub guidance: String,
    pub input: Vec<Value>,
    pub schemas: Vec<Value>,
    pub extra_json_bytes: usize,
    pub contributions_projected: bool,
}

/// Normalize only identity fields from trusted native producers; old hashes never pin guidance.
fn skill_selector(value: &Value) -> Result<Value> {
    let selector = json!({"type":"skill","ability_id":value["ability_id"],"kind":value["kind"],"skill_id":value["skill_id"]});
    webagent_assets_contracts::validate_selector(&selector)?;
    Ok(selector)
}

/// Label every guidance block as untrusted file guidance.
fn definition_body(definition: &webagent_capability_contracts::Definition) -> String {
    format!(
        "\n\n## Untrusted live guidance: {}\n{}",
        definition.id, definition.body
    )
}

/// Map only authorized schema identities back to the exact current host objects.
fn schemas_in_order(schemas: &[Value], order: &[String]) -> Result<Vec<Value>> {
    order
        .iter()
        .map(|name| {
            schemas
                .iter()
                .find(|schema| schema["name"] == name.as_str())
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Context schema unavailable"))
        })
        .collect()
}

/// Decode native call arguments while retaining their exact expected revisions and request IDs.
fn arguments(run: &Value) -> Result<Value> {
    let value = &run["pending"]["arguments"];
    if let Some(raw) = value.as_str() {
        Ok(serde_json::from_str(raw)?)
    } else {
        Ok(value.clone())
    }
}

/// Stage only validated explicit selection effects before append_tool clears pending identity.
pub fn record_tool_operation(
    run: &mut Value,
    result: &Value,
    _previous_tools: &Value,
    previous_skills: &Value,
) -> Result<()> {
    let Some(name) = run["pending"]["name"].as_str() else {
        return Ok(());
    };
    let name = name.to_owned();
    if !matches!(name.as_str(), "context_assets" | "load_skill") || result["status"] != "success" {
        return Ok(());
    }
    let args = arguments(run)?;
    if name == "context_assets" {
        if result["status"] != "success" || args["action"] != "unload" {
            return Ok(());
        }
        let operation = Operation::Unload {
            selector: args["selector"].clone(),
            expected_revision: args["expected_revision"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Unload revision"))?,
            request_id: args["request_id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Unload request ID"))?
                .into(),
        };
        if result["output"]["operation"] != json!(operation) {
            bail!("Unload operation result mismatch");
        }
        webagent_assets_contracts::validate_selector(&args["selector"])?;
        run["_context_assets_operation"] = json!(operation);
        return Ok(());
    }
    let mut selectors = Vec::new();
    match name.as_str() {
        // A tool name resolves to its kind `tool` selector, recorded like any skill.
        "load_skill" if result["output"]["status"] == "loaded" => {
            let expected = skill_selector(&result["output"])?;
            if run["loaded_skills"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|skill| skill_selector(skill).ok().as_ref() == Some(&expected))
                && !previous_skills
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|skill| skill_selector(skill).ok().as_ref() == Some(&expected))
            {
                selectors.push(expected);
            }
        }
        _ => {}
    }
    if selectors.is_empty() {
        return Ok(());
    }
    let revision = run["context_assets_revision"].as_u64().unwrap_or(0);
    let request_id = format!(
        "activation:{}",
        crate::digest(&json!([run["id"], run["pending"]["call_id"], name]))
    );
    run["_context_assets_operation"] = json!(Operation::Activate {
        selectors,
        expected_revision: revision,
        request_id
    });
    Ok(())
}

pub fn identity(run: &Value, current_user_id: &str) -> Result<(String, String, String)> {
    let sid = run["session_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Context assets session missing"))?;
    let agent = run["agent_id"].as_str().unwrap_or("shared_default");
    let requester = run["requester_user_id"].as_str().unwrap_or(current_user_id);
    if [sid, agent, requester]
        .iter()
        .any(|id| !webagent_assets_contracts::valid_id(id))
    {
        bail!("Context assets identity");
    }
    Ok((sid.into(), agent.into(), requester.into()))
}
/// Hydrate exact permanent selectors using the current owner-supplied catalog operation.
pub fn hydrate_selectors(
    run: &mut Value,
    state: &State,
    tool_marker: impl Fn(&str) -> String,
) -> Result<bool> {
    let mut loaded = Vec::new();
    let mut tools = Vec::new();
    let mut skills = Vec::new();
    for selector in &state.selectors {
        match selector["type"].as_str() {
   Some("ability")=>{let value=selector["ability_id"].clone();if !loaded.contains(&value){loaded.push(value);}},
   Some("tool")=>{
    let Some(name)=selector["tool_name"].as_str() else {continue;};
    tools.push(json!(name));let marker=json!(tool_marker(name));
    // A tool selector exposes only that tool; ability IDs come from explicit activation alone.
    if !loaded.contains(&marker){loaded.push(marker);}
   },
   Some("skill")=>skills.push(json!({"ability_id":selector["ability_id"],"kind":selector["kind"],"skill_id":selector["skill_id"]})),
   _=>bail!("Invalid context asset selector"),
  }
    }
    let before = crate::digest(&json!([
        run["loaded_abilities"],
        run["selected_tools"],
        run["loaded_skills"],
        run["context_assets_revision"]
    ]));
    run["loaded_abilities"] = json!(loaded);
    run["selected_tools"] = json!(tools);
    run["loaded_skills"] = json!(skills);
    run["context_assets_revision"] = json!(state.revision);
    Ok(before
        != crate::digest(&json!([
            run["loaded_abilities"],
            run["selected_tools"],
            run["loaded_skills"],
            run["context_assets_revision"]
        ])))
}
pub fn legacy_seed_needed(run: &Value, state: &State) -> bool {
    state.revision == 0
        && run["context_assets_revision"].is_null()
        && run["status"] == "running"
        && (!run["selected_tools"]
            .as_array()
            .is_none_or(|v| v.is_empty())
            || !run["loaded_skills"].as_array().is_none_or(|v| v.is_empty()))
}
pub fn legacy_operation(
    run: &Value,
    canonical: &Value,
    current_user_id: &str,
) -> Result<Operation> {
    let tools = run["selected_tools"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let skills = run["loaded_skills"].as_array().cloned().unwrap_or_default();
    let (sid, agent, requester) = identity(run, current_user_id)?;
    let rid = run["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Context assets run missing"))?
        .to_owned();
    let canonical_requester = canonical["requester_user_id"]
        .as_str()
        .unwrap_or(current_user_id);
    if canonical["status"] != "running"
        || canonical["session_id"] != sid
        || canonical["agent_id"].as_str().unwrap_or("shared_default") != agent
        || canonical_requester != requester
        || canonical["run_revision"] != run["run_revision"]
        || canonical["selected_tools"] != run["selected_tools"]
        || canonical["loaded_skills"] != run["loaded_skills"]
    {
        bail!("Context assets legacy checkpoint changed");
    }
    let mut selectors = Vec::new();
    for tool in tools {
        let value = json!({"type":"tool","tool_name":tool});
        webagent_assets_contracts::validate_selector(&value)?;
        if !selectors.contains(&value) {
            selectors.push(value);
        }
    }
    for skill in skills {
        let value = skill_selector(&skill)?;
        if !selectors.contains(&value) {
            selectors.push(value);
        }
    }
    let operation = Operation::Activate {
        selectors,
        expected_revision: 0,
        request_id: format!(
            "legacy:{}",
            crate::digest(&json!([sid, agent, requester, rid]))
        ),
    };
    Ok(operation)
}
/// The identity of the session's compacted history; placements reset when it changes.
pub fn history_basis(session: &Value) -> String {
    crate::digest(&json!([
        session["history_revision"],
        session["checkpoint_through"]
    ]))
}
/// The identity of the session's Main Notes snapshot. An ordinary fold keeps the notes
/// changes in the history ahead of its new block, so only a rewind or a block merge (which
/// rewrites the start of the history anyway) rebuilds the snapshot.
pub fn notes_basis(session: &Value) -> String {
    crate::digest(&json!([
        session["history_revision"],
        session["notes_front"]
    ]))
}
/// Place `cap`'s definitions and the host's `extra` ones (id, body), such as Main Notes,
/// around `history`. An extra body must stay unchanged while its ID is placed.
pub async fn project_context(
    run: &mut Value,
    cap: &PreparedCapability,
    profile: &ProfileV1,
    schemas: &[Value],
    history: &[Value],
    contributions: &[context_input::Contribution],
    extra: &[(String, String)],
    call: context_input::CallIdentity,
    ledger: State,
    session: &Value,
    agent: &str,
    requester: &str,
    root: &std::path::Path,
    planner: &impl crate::ports::Planner,
    cancel: &CancellationToken,
) -> Result<Working> {
    let history_basis = history_basis(session);
    let mut bodies = BTreeMap::new();
    let mut definitions = Vec::new();
    for definition in &cap.definitions {
        if bodies
            .insert(definition.id.clone(), definition_body(definition))
            .is_some()
        {
            bail!("Duplicate capability definition");
        }
        definitions.push(definition.id.clone());
    }
    for (id, body) in extra {
        if bodies.insert(id.clone(), body.clone()).is_some() {
            bail!("Duplicate context definition");
        }
        definitions.push(id.clone());
    }
    // Only the call's own contributions (mode, publish guidance) change per call; they follow
    // the history. Everything else, the menu included, stays in the instructions.
    let mut native = String::new();
    for selected in context_input::select(contributions, call)? {
        if selected.disposition == "included" {
            if !native.is_empty() {
                native.push('\n');
            }
            native.push_str(&selected.item.content);
        }
    }
    // Every profile keeps definitions in the instructions; the planner only fixes schema order.
    let request = Request::Place {
        version: 1,
        state: ledger.clone(),
        history: history.to_vec(),
        definitions: definitions.clone(),
        temporary: Vec::new(),
        schema_names: cap.schema_order.clone(),
        history_basis: history_basis.clone(),
    };
    let plan = planner.assets_plan(root, &request, cancel).await?;
    webagent_assets_contracts::validate_plan(&request, &plan)?;
    let Plan::Place {
        placements,
        schema_order,
        ..
    } = &plan
    else {
        bail!("Context placement kind");
    };
    let mut next = ledger.clone();
    next.placements = placements
        .iter()
        .filter(|placement| definitions.contains(&placement.id))
        .cloned()
        .collect();
    next.schema_order = schema_order.clone();
    next.history_basis = history_basis;
    if next != ledger {
        next.revision = ledger
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("Context assets revision exhausted"))?;
        run["_context_assets_write"] = json!({"scope":crate::ledger::scope_key(&agent,&requester),
   "expected_revision":ledger.revision,"state":next});
    }
    let schemas = schemas_in_order(schemas, schema_order)?;
    let mut guidance = definitions
        .iter()
        .map(|id| bodies[id].as_str())
        .collect::<Vec<_>>()
        .join("");
    guidance.push_str(&cap.temporary_menu);
    // The call's notes follow the history as one message, for this call only, so the cached
    // instructions and history stay unchanged.
    let mut input = history.to_vec();
    let mut extra_json_bytes = 0;
    if !native.is_empty() {
        let note = json!({"role":profile.instruction_role.as_str(),"content":native});
        extra_json_bytes = serde_json::to_vec(&note)?.len() + 1;
        input.push(note);
    }
    Ok(Working {
        instructions: cap.main.clone(),
        guidance,
        input,
        schemas,
        extra_json_bytes,
        contributions_projected: true,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_tool_skill_delta_only() {
        let skill = json!({"ability_id":"files","kind":"tool","skill_id":"file_read"});
        let mut run = json!({"id":"r","context_assets_revision":4,"pending":{"name":"load_skill","call_id":"c","arguments":{"tool_name":"file_read"}},
   "loaded_skills":[skill.clone()]});
        let mut output = skill.clone();
        output["status"] = json!("loaded");
        record_tool_operation(
            &mut run,
            &json!({"status":"success","output":output}),
            &json!([]),
            &json!([]),
        )
        .unwrap();
        let operation: Operation =
            serde_json::from_value(run["_context_assets_operation"].clone()).unwrap();
        let Operation::Activate {
            selectors,
            expected_revision,
            ..
        } = operation
        else {
            panic!()
        };
        assert_eq!(expected_revision, 4);
        assert_eq!(
            selectors,
            vec![json!({"type":"skill","ability_id":"files","kind":"tool","skill_id":"file_read"})]
        );
        let mut denied = run.clone();
        denied
            .as_object_mut()
            .unwrap()
            .remove("_context_assets_operation");
        record_tool_operation(
            &mut denied,
            &json!({"status":"denied"}),
            &json!([]),
            &json!([]),
        )
        .unwrap();
        assert!(denied.get("_context_assets_operation").is_none());
    }
    #[test]
    fn normalized_skill_is_identity_only() {
        let value=skill_selector(&json!({"ability_id":"base","kind":"own","skill_id":"guide","sha256":"old","revision":8})).unwrap();
        assert_eq!(
            value,
            json!({"type":"skill","ability_id":"base","kind":"own","skill_id":"guide"})
        );
    }
}

pub fn capability_request(
    base_prompt: String,
    live_slots: String,
    ledger: State,
    schemas: &[Value],
    guidance: Vec<Guidance>,
    menu: String,
    sourcefingerprint: &str,
) -> Result<PrepareRequest> {
    Ok(PrepareRequest {
        version: 1,
        source_fingerprint: sourcefingerprint.into(),
        base_prompt,
        live_slots,
        guidance,
        permitted_schemas: schemas
            .iter()
            .map(|schema| {
                schema["name"]
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| anyhow::anyhow!("Schema identity"))
            })
            .collect::<Result<_>>()?,
        prior_schema_order: ledger.schema_order,
        menu,
    })
}
