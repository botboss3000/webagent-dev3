//! Pure context algorithm; implementation changes do not rebuild a live core.
use anyhow::Result;
use serde::Serialize;
use crate::{context_compaction,context_input};
use crate::context_input::Request;
pub use webagent_context_contracts::context_assembly::*;
/// Use the shared JSON-byte convention so service reports and host validation agree.
fn size<T: Serialize + ?Sized>(value: &T) -> Result<usize> { crate::context_accounting::json_size(value) }

/// Select inputs once, combine instructions, then validate the exact logical envelope.
/// A summary is only planned here; the loop must generate, validate and persist it.
pub fn prepare<'a>(request: Request<'a>) -> Result<Prepared<'a>> {
    prepare_incremental(request, &mut crate::context_accounting::ContextAccounting::default())
}
/// Reuse append-only measurements while retaining borrowed history and schemas.
/// The caller must reset accounting after replacing history; assembly never persists it.
pub fn prepare_incremental<'a>(request: Request<'a>, state: &mut crate::context_accounting::ContextAccounting) -> Result<Prepared<'a>> {
    state.sync(request.history)?;
    // Input selects once; assembly alone combines the selected ingredients in stable order.
    let selected = context_input::select(request.contributions, request.identity).map_err(|_| Failure::new("invalid_contribution", "selection"))?;
    // Reserve exact included UTF-8 bytes once; stable instruction order and accounting stay unchanged.
    let capacity = selected.iter().filter(|decision| decision.disposition == "included")
        .fold(request.base.len().saturating_add(request.guidance.len()), |bytes, decision|
            bytes.saturating_add(1).saturating_add(decision.item.content.len()));
    let mut instructions = String::with_capacity(capacity);
    instructions.push_str(request.base);
    instructions.push_str(request.guidance);
    for decision in &selected {
        if decision.disposition == "included" && !state.contributions_projected { instructions.push('\n'); instructions.push_str(&decision.item.content); }
    }
    if instructions.is_empty() {
        return Err(Failure::new("instruction_limit", "instructions").into());
    }
    // Keep the existing logical-envelope budget semantics, without allocating JSON buffers.
    let instruction_json_bytes = size(&instructions)?;
    let schemas_json_bytes = size(request.schemas)?;
    let framing = b"{\"instructions\":,\"tools\":,\"input\":}".len();
    // Empty history contributes two brackets. No transcript/schema copies are needed.
    let overhead = framing + instruction_json_bytes + schemas_json_bytes + state.projection_extra_bytes;
    let history_budget = request.max_bytes.checked_sub(overhead).filter(|remaining| *remaining > 2)
        .ok_or_else(|| Failure::new("overhead_exceeds_budget", "budget"))?;
    // Assembly owns the envelope ceiling; only the compaction module owns folding policy.
    let reserve = request.compaction.summary_reserve_bytes;
    if history_budget <= reserve { return Err(Failure::new("summary_reserve_unavailable", "budget").into()); }
    let (sizes, characters) = state.measurements();
    let plan = context_compaction::plan_request_cached(request.history, history_budget, request.compaction, sizes, characters, overhead).map_err(compaction_error)?;
    if let Some(plan) = plan { return Ok(Prepared::Compact(plan)); }
    let history_json_bytes = state.bytes().checked_add(state.projection_extra_bytes)
        .ok_or_else(|| Failure::new("overhead_exceeds_budget", "budget"))?;
    let envelope_bytes = framing + instruction_json_bytes + schemas_json_bytes + history_json_bytes;
    if envelope_bytes > request.max_bytes { return Err(Failure::new("envelope_exceeds_budget", "budget").into()); }
    let report = Report {
        version: 1, scope: "logical_call_envelope".into(), accounting: "serialized_json_bytes_not_provider_tokens".into(),
        provider: request.model.provider.clone(), model: request.model.model.clone(),
        transport: request.model.transport.clone(),
        turn: request.identity.turn, call: request.identity.call,
        max_bytes: request.max_bytes, history_budget_bytes: history_budget,
        summary_reserve_bytes: reserve, envelope_bytes,
        instruction_json_bytes, base_content_bytes: request.base.len(),
        guidance_content_bytes: request.guidance.len(), schemas_json_bytes, history_json_bytes, projection_extra_bytes:state.projection_extra_bytes,
        framing_bytes: envelope_bytes - instruction_json_bytes - schemas_json_bytes - history_json_bytes,
        history: state.report(),
        overlays: selected.iter().map(|decision| Overlay { id: decision.item.id.clone(), source: decision.item.source.clone(), lifetime: decision.item.lifetime, disposition: decision.disposition.into(), content_bytes: decision.item.content.len() }).collect(),
        schemas: request.schemas.iter().map(|schema| Ok(Schema {
            name: schema["name"].as_str().unwrap_or("").into(), bytes: size(schema)?,
            bootstrap: matches!(schema["name"].as_str(), Some("list_abilities" | "load_ability")),
        })).collect::<Result<Vec<_>>>()?,
    };
    Ok(Prepared::Ready(Envelope { input: request.history, instructions: instructions,
        schemas: request.schemas, report }))
}
