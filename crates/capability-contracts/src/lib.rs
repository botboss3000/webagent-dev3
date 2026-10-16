//! Pure capability-composition DTOs and host-side safety validation.
//! Callers resolve live source and authority before supplying any body or name.
// @code-index {"version":1,"feature":"capability-composition","aliases":["live context","source correspondence"],"purpose":"Fixed capability safety contract: exact prompt/body correspondence, authorized schema identity/order and content-free reports. Mandatory safety invariants are not configuration bypass settings.","span_lines":80,"configuration":{"status":"not_applicable","references":[],"opportunities":[],"rationale":"Source/body/schema correspondence and serialized frame bounds are mandatory integrity checks shared by host and component. Configurable bypasses could let returned plans change authorized instructions."}}
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::{collections::{BTreeMap, BTreeSet}, io::{self, Write}};

/// The private engine wrapper reserves framing space inside its 16 MiB limit.
pub const MAX_DOCUMENT_BYTES: usize = 16 * 1024 * 1024 - 4096;
pub const INTERFACE_VERSION: u32 = 1;
pub const OPERATION: &str = "capability.prepare";
const MAX_GUIDANCE: usize = 8192;
const MAX_SCHEMAS: usize = 4096;

/// Kind preserves the host's classification; it never supplies execution grants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuidanceKind { Ability, Tool, Selectable, Own }

/// One already authorized current source body. Natural means supplied by visibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Guidance {
    pub id: String,
    pub kind: GuidanceKind,
    pub body: String,
    pub selected: bool,
    pub natural: bool,
}

/// Current host-read strings and identities only: no paths, credentials or grants.
/// live_slots includes the host's existing slot delimiters; composition adds none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareRequest {
    pub version: u32,
    pub source_fingerprint: String,
    pub base_prompt: String,
    pub live_slots: String,
    pub guidance: Vec<Guidance>,
    pub permitted_schemas: Vec<String>,
    pub prior_schema_order: Vec<String>,
    pub menu: String,
}

/// Permanent guidance is kept separate from call-scoped discovery menus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub kind: GuidanceKind,
    pub body: String,
}

/// Content-free reason for inclusion, derived solely from host-supplied flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InclusionReason { Natural, Selected, NaturalAndSelected, Discoverable }

/// This report never contains source text or authorization documents.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inclusion {
    pub id: String,
    pub kind: GuidanceKind,
    pub included: bool,
    pub reason: InclusionReason,
    pub content_bytes: usize,
}

/// The host maps schema_order back to its exact authorized schema objects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedCapability {
    pub version: u32,
    pub source_fingerprint: String,
    pub main: String,
    pub definitions: Vec<Definition>,
    pub schema_order: Vec<String>,
    pub temporary_menu: String,
    pub inclusion: Vec<Inclusion>,
}

/// Count serialized bytes without allocating another private payload copy.
struct Counter { bytes: usize }
impl Write for Counter {
    /// Count emitted JSON bytes and stop before the document bound.
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes = self.bytes.checked_add(buffer.len())
            .filter(|bytes| *bytes <= MAX_DOCUMENT_BYTES)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Capability document limit"))?;
        Ok(buffer.len())
    }
    /// Counting has no buffered private data to flush.
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}

/// Enforce the same bounded serialized document in linked and service modes.
fn validate_size(value: &impl Serialize) -> Result<()> {
    serde_json::to_writer(Counter { bytes: 0 }, value)?;
    Ok(())
}

/// Opaque qualified IDs are labels, never filesystem paths or executable names.
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 256
        && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b':'))
}

/// Match registered native/plugin tool naming without interpreting any schema.
fn valid_schema_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 128
        && name.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

/// Reject ambiguous identities and missing mandatory text before composition.
/// Fingerprints are consistency evidence only; the host still rechecks authority.
pub fn validate_request(request: &PrepareRequest) -> Result<()> {
    if request.version != INTERFACE_VERSION
        || request.source_fingerprint.len() != 64
        || !request.source_fingerprint.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || request.base_prompt.trim().is_empty()
        || request.guidance.len() > MAX_GUIDANCE
        || request.permitted_schemas.len() > MAX_SCHEMAS
        || request.prior_schema_order.len() > MAX_SCHEMAS {
        bail!("Invalid capability request");
    }
    let mut identities = BTreeSet::new();
    for item in &request.guidance {
        if !valid_id(&item.id) || !identities.insert(item.id.as_str())
            || (item.selected || item.natural) && item.body.trim().is_empty() {
            bail!("Invalid capability guidance");
        }
    }
    for names in [&request.permitted_schemas, &request.prior_schema_order] {
        let mut seen = BTreeSet::new();
        if names.iter().any(|name| !valid_schema_name(name) || !seen.insert(name.as_str())) {
            bail!("Invalid capability schema identities");
        }
    }
    validate_size(request)
}

/// Derive report semantics independently of rendering or source revision hashes.
pub fn inclusion_reason(item: &Guidance) -> InclusionReason {
    match (item.natural, item.selected) {
        (true, true) => InclusionReason::NaturalAndSelected,
        (true, false) => InclusionReason::Natural,
        (false, true) => InclusionReason::Selected,
        (false, false) => InclusionReason::Discoverable,
    }
}

/// Validate service output against current host inputs, not the service's claims.
/// Exact bodies, mandatory definitions, schema membership/order and counts cannot
/// be changed by a compatible implementation. The host rechecks live authority
/// and source_fingerprint immediately before using this result for dispatch.
pub fn validate_result(request: &PrepareRequest, result: &PreparedCapability) -> Result<()> {
    validate_request(request)?;
    if result.version != INTERFACE_VERSION
        || result.source_fingerprint != request.source_fingerprint
        || result.main.strip_prefix(request.base_prompt.as_str()) != Some(request.live_slots.as_str())
        || result.temporary_menu != request.menu
        || result.inclusion.len() != request.guidance.len() {
        bail!("Capability response identity or prompt mismatch");
    }
    let expected = request.guidance.iter().filter(|item| item.selected || item.natural);
    if result.definitions.len() != expected.clone().count() {
        bail!("Capability mandatory guidance mismatch");
    }
    for (definition, source) in result.definitions.iter().zip(expected) {
        if definition.id != source.id || definition.kind != source.kind || definition.body != source.body {
            bail!("Capability source body mismatch");
        }
    }
    for (reported, source) in result.inclusion.iter().zip(&request.guidance) {
        let included = source.selected || source.natural;
        if reported.id != source.id || reported.kind != source.kind
            || reported.included != included || reported.reason != inclusion_reason(source)
            || reported.content_bytes != if included { source.body.len() } else { 0 } {
            bail!("Capability inclusion report mismatch");
        }
    }
    let permitted = request.permitted_schemas.iter().enumerate()
        .map(|(index, name)| (name.as_str(), index)).collect::<BTreeMap<_, _>>();
    let prior = request.prior_schema_order.iter().enumerate()
        .map(|(index, name)| (name.as_str(), index)).collect::<BTreeMap<_, _>>();
    if result.schema_order.len() != permitted.len() {
        bail!("Capability schema membership mismatch");
    }
    let mut seen = BTreeSet::new();
    let mut previous = None;
    for name in &result.schema_order {
        let Some(index) = permitted.get(name.as_str()) else { bail!("Capability schema not authorized"); };
        if !seen.insert(name.as_str()) { bail!("Capability duplicate schema"); }
        let rank = prior.get(name.as_str()).map(|index| (0, *index)).unwrap_or((1, *index));
        if previous.is_some_and(|previous| previous >= rank) {
            bail!("Capability unstable schema order");
        }
        previous = Some(rank);
    }
    validate_size(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Fixed independent wire fixture establishes the contract's exact outputs.
    fn fixture() -> (PrepareRequest, PreparedCapability) {
        let request = PrepareRequest {
            version: 1, source_fingerprint: "a".repeat(64), base_prompt: "BASE".into(),
            live_slots: "\n\nSLOT".into(), menu: "MENU".into(),
            guidance: vec![Guidance { id: "skill:base:own:guide".into(), kind: GuidanceKind::Own,
                body: "π🙂".into(), selected: true, natural: false }],
            permitted_schemas: vec!["calculate".into(), "file_read".into()],
            prior_schema_order: vec!["removed_tool".into(), "file_read".into()],
        };
        let result = PreparedCapability {
            version: 1, source_fingerprint: "a".repeat(64), main: "BASE\n\nSLOT".into(),
            definitions: vec![Definition { id: "skill:base:own:guide".into(), kind: GuidanceKind::Own, body: "π🙂".into() }],
            schema_order: vec!["file_read".into(), "calculate".into()], temporary_menu: "MENU".into(),
            inclusion: vec![Inclusion { id: "skill:base:own:guide".into(), kind: GuidanceKind::Own,
                included: true, reason: InclusionReason::Selected, content_bytes: 6 }],
        };
        (request, result)
    }

    /// A valid response preserves exact UTF-8 and excludes revoked prior schemas.
    #[test]
    fn accepts_exact_source_correspondence() {
        let (request, result) = fixture();
        validate_result(&request, &result).unwrap();
    }

    /// A process cannot fabricate instruction text, capabilities or report facts.
    #[test]
    fn rejects_forged_outputs() {
        let (request, honest) = fixture();
        let mut cases = Vec::new();
        let mut value = honest.clone(); value.main.push_str("INJECTED"); cases.push(value);
        let mut value = honest.clone(); value.definitions[0].body = "STALE".into(); cases.push(value);
        let mut value = honest.clone(); value.definitions.clear(); cases.push(value);
        let mut value = honest.clone(); value.definitions.push(value.definitions[0].clone()); cases.push(value);
        let mut value = honest.clone(); value.schema_order[0] = "file_write".into(); cases.push(value);
        let mut value = honest.clone(); value.schema_order.reverse(); cases.push(value);
        let mut value = honest.clone(); value.schema_order[1] = "file_read".into(); cases.push(value);
        let mut value = honest.clone(); value.inclusion[0].content_bytes = 2; cases.push(value);
        let mut value = honest.clone(); value.inclusion[0].included = false; cases.push(value);
        let mut value = honest.clone(); value.temporary_menu.clear(); cases.push(value);
        let mut value = honest; value.source_fingerprint = "b".repeat(64); cases.push(value);
        for forged in cases { assert!(validate_result(&request, &forged).is_err()); }
    }

    /// Mandatory visible text and unambiguous current identities cannot disappear.
    #[test]
    fn rejects_invalid_inputs_and_unknown_authority_fields() {
        let (request, _) = fixture();
        let mut bad = request.clone(); bad.base_prompt = "  ".into(); assert!(validate_request(&bad).is_err());
        let mut bad = request.clone(); bad.guidance[0].body.clear(); assert!(validate_request(&bad).is_err());
        let mut bad = request.clone(); bad.guidance.push(bad.guidance[0].clone()); assert!(validate_request(&bad).is_err());
        let mut bad = request.clone(); bad.permitted_schemas.push("calculate".into()); assert!(validate_request(&bad).is_err());
        let mut bad = request.clone(); bad.guidance[0].id = "../source".into(); assert!(validate_request(&bad).is_err());
        let mut bad = serde_json::to_value(request).unwrap(); bad["credentials"] = json!({});
        assert!(serde_json::from_value::<PrepareRequest>(bad).is_err());
    }

    /// Linked calls obey the framed-service byte bound too, including escaping.
    #[test]
    fn rejects_oversized_serialized_text() {
        let (mut request, _) = fixture();
        request.base_prompt = "\u{0001}".repeat(MAX_DOCUMENT_BYTES / 6 + 1);
        assert!(validate_request(&request).is_err());
    }
}
