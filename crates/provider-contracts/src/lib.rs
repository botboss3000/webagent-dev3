//! Version-one provider projection references. The host owns content, authority and transport.
// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Shared provider wire types and reference-only validation; materialize reconstructs host-owned content without accepting routing authority.","span_lines":80,"configuration":{"status":"not_applicable","rationale":"Reference coverage, provider identity, replay rules and transport authority are fixed protocol invariants."}}
use anyhow::{anyhow, bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const CONTRACT_VERSION: u32 = 1;
pub const INTERFACE_VERSION: u32 = CONTRACT_VERSION;
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProviderIdV1 {
    OpenaiSubscription, Deepseek, OpenaiApi, Openrouter, Abacus, Gemini, Groq,
    Together, Mistral, Fireworks, Xai, Perplexity, Deepinfra, Typesafe, ClaudeBridge,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum InstructionRoleV1 { Developer, System }
impl InstructionRoleV1 {
    /// Return the only native instruction role allowed by this profile.
    pub const fn as_str(self) -> &'static str {
        match self { Self::Developer => "developer", Self::System => "system" }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransportV1 { Responses, ChatCompletions, Decision }
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningReplayV1 { ResponsesItems, DeepseekContent, None }
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheControlV1 { SubscriptionSessionKey, ImplicitPrefix, None }

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProfileV1 {
    pub append_instructions: bool,
    pub instruction_role: InstructionRoleV1,
    pub transport: TransportV1,
    pub reasoning_replay: ReasoningReplayV1,
    pub cache_control: CacheControlV1,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RequestV1 {
    Profile { version: u32, provider: ProviderIdV1, model: String },
    Project {
        version: u32, provider: ProviderIdV1, model: String,
        instructions: String, input: Vec<Value>, tools: Vec<Value>,
    },
}
impl RequestV1 {
    /// Provider identity is host-selected; projection never selects credentials or an endpoint.
    pub fn provider(&self) -> ProviderIdV1 {
        match self { Self::Profile { provider, .. } | Self::Project { provider, .. } => *provider }
    }
    /// Keep the host-selected model intact without allowing a returned replacement.
    pub fn model(&self) -> &str {
        match self { Self::Profile { model, .. } | Self::Project { model, .. } => model }
    }
}

/// These groups contain positions only, never arbitrary replacement history or schemas.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceGroupV1 {
    ResponseItem { source_index: u32 },
    Message { source_index: u32 },
    Assistant { message: Option<u32>, calls: Vec<u32> },
    ToolResult { source_index: u32 },
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionV1 {
    pub provider: ProviderIdV1,
    pub transport: TransportV1,
    pub groups: Vec<SourceGroupV1>,
    pub omitted: Vec<u32>,
    pub tools: Vec<u32>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResultV1 {
    Profile { version: u32, profile: ProfileV1 },
    Projected { version: u32, projection: ProjectionV1 },
}

/// Reconstructed content remains in host memory; the component returns only references.
#[derive(Clone, Debug, PartialEq)]
pub enum MaterializedV1 {
    Responses { instructions: String, input: Vec<Value>, tools: Vec<Value> },
    ChatCompletions { messages: Vec<Value>, tools: Vec<Value> },
}

/// Installed wire contracts are safety invariants, not user-configurable permissions.
pub fn profile_for(provider: ProviderIdV1) -> ProfileV1 {
    match provider {
        ProviderIdV1::OpenaiSubscription => ProfileV1 {
            append_instructions: true, instruction_role: InstructionRoleV1::Developer,
            transport: TransportV1::Responses, reasoning_replay: ReasoningReplayV1::ResponsesItems,
            cache_control: CacheControlV1::SubscriptionSessionKey,
        },
        ProviderIdV1::Deepseek => ProfileV1 {
            append_instructions: true, instruction_role: InstructionRoleV1::System,
            transport: TransportV1::ChatCompletions, reasoning_replay: ReasoningReplayV1::DeepseekContent,
            cache_control: CacheControlV1::ImplicitPrefix,
        },
        ProviderIdV1::Typesafe => ProfileV1 {
            append_instructions: false, instruction_role: InstructionRoleV1::System,
            transport: TransportV1::Decision, reasoning_replay: ReasoningReplayV1::None,
            cache_control: CacheControlV1::None,
        },
        _ => ProfileV1 {
            append_instructions: false, instruction_role: InstructionRoleV1::System,
            transport: TransportV1::ChatCompletions, reasoning_replay: ReasoningReplayV1::None,
            cache_control: CacheControlV1::None,
        },
    }
}

struct Counter(usize);
impl std::io::Write for Counter {
    /// Count serialized bytes and reject arithmetic overflow without retaining private content.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self.0.checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("Provider frame overflow"))?;
        Ok(bytes.len())
    }
    /// Counting has no buffered output to flush.
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}
/// Count the serialized contract without allocating another copy of private history.
fn bounded<T: Serialize>(value: &T) -> Result<()> {
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)?;
    if counter.0 > MAX_FRAME_BYTES { bail!("Provider contract frame limit"); }
    Ok(())
}
/// Preserve the existing host model-name syntax and reject unsupported contract versions.
pub fn validate_request(request: &RequestV1) -> Result<()> {
    let version = match request {
        RequestV1::Profile { version, .. } | RequestV1::Project { version, .. } => *version,
    };
    let model = request.model();
    if version != CONTRACT_VERSION || model.is_empty() || model.len() > 128
        || !model.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'/' | b':')) {
        bail!("Provider request contract rejected");
    }
    bounded(request)?;
    if let RequestV1::Project { provider, instructions, input, tools, .. } = request {
        if instructions.is_empty() { bail!("Invalid provider instructions"); }
        if input.len() > u32::MAX as usize || tools.len() > u32::MAX as usize {
            bail!("Provider reference bounds");
        }
        if *provider != ProviderIdV1::OpenaiSubscription {
            for item in input {
                if *provider == ProviderIdV1::Deepseek
                    && item.get("deepseek_reasoning").is_some_and(|v| !v.is_string()) {
                    bail!("Invalid provider reasoning content");
                }
                match item_type(item) {
                    "reasoning" => {},
                    "function_call" => {
                        required(item, "call_id")?; required(item, "name")?; required(item, "arguments")?;
                    },
                    "function_call_output" => { required(item, "call_id")?; },
                    "message" => {
                        if !matches!(role(item), "user" | "assistant" | "system" | "developer") {
                            bail!("Invalid provider message role");
                        }
                        message_content(&item["content"])?;
                    },
                    _ => bail!("Unsupported provider input item"),
                }
            }
            for tool in tools { required(tool, "name")?; }
        }
    }
    Ok(())
}
/// Only the existing Responses-shaped item tags are interpreted for compatible endpoints.
pub fn item_type(item: &Value) -> &str { item["type"].as_str().unwrap_or("message") }
/// Legacy untagged messages default to user, matching the existing compatible adapter.
pub fn role(item: &Value) -> &str { item["role"].as_str().unwrap_or("user") }
/// Require existing textual IDs/arguments without copying their content into errors.
fn required<'a>(item: &'a Value, field: &str) -> Result<&'a str> {
    item.get(field).and_then(Value::as_str).filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("Provider reference missing required text"))
}
/// Resolve a checked position against the original host input.
fn source(input: &[Value], index: u32) -> Result<&Value> {
    input.get(index as usize).ok_or_else(|| anyhow!("Provider source reference out of range"))
}

/// Validate reference coverage, trust roles, schema order and indivisible assistant exchanges.
pub fn validate_result(request: &RequestV1, result: &ResultV1) -> Result<()> {
    validate_request(request)?;
    bounded(result)?;
    match (request, result) {
        (RequestV1::Profile { provider, .. }, ResultV1::Profile { version, profile })
            if *version == CONTRACT_VERSION && *profile == profile_for(*provider) => Ok(()),
        (RequestV1::Project { provider, input, tools, .. }, ResultV1::Projected { version, projection }) => {
            let profile = profile_for(*provider);
            if *version != CONTRACT_VERSION || projection.provider != *provider
                || projection.transport != profile.transport {
                bail!("Provider projection identity rejected");
            }
            if projection.groups.len() > input.len() || projection.omitted.len() > input.len() {
                bail!("Provider projection reference bounds");
            }
            let tool_indices: Vec<u32> = (0..tools.len()).map(|index| index as u32).collect();
            if projection.tools != tool_indices { bail!("Provider schema order rejected"); }
            if profile.transport == TransportV1::Responses {
                if !projection.omitted.is_empty() || projection.groups.len() != input.len() {
                    bail!("Responses projection coverage rejected");
                }
                for (index, group) in projection.groups.iter().enumerate() {
                    if *group != (SourceGroupV1::ResponseItem { source_index: index as u32 }) {
                        bail!("Responses projection order rejected");
                    }
                }
                return Ok(());
            }
            let omitted: Vec<u32> = input.iter().enumerate().filter(|(_, item)| item_type(item) == "reasoning")
                .map(|(index, _)| index as u32).collect();
            if projection.omitted != omitted { bail!("Provider omission rejected"); }
            let expected: Vec<u32> = input.iter().enumerate().filter(|(_, item)| item_type(item) != "reasoning")
                .map(|(index, _)| index as u32).collect();
            let mut covered = Vec::new();
            let mut prior_assistant = false;
            for group in &projection.groups {
                match group {
                    SourceGroupV1::Message { source_index } => {
                        let item = source(input, *source_index)?;
                        if item_type(item) != "message" || role(item) == "assistant" {
                            bail!("Provider message reference rejected");
                        }
                        covered.push(*source_index); prior_assistant = false;
                    },
                    SourceGroupV1::ToolResult { source_index } => {
                        if item_type(source(input, *source_index)?) != "function_call_output" {
                            bail!("Provider tool result reference rejected");
                        }
                        covered.push(*source_index); prior_assistant = false;
                    },
                    SourceGroupV1::Assistant { message, calls } => {
                        if message.is_none() && (calls.is_empty() || prior_assistant) {
                            bail!("Provider assistant grouping rejected");
                        }
                        if let Some(index) = message {
                            let item = source(input, *index)?;
                            if item_type(item) != "message" || role(item) != "assistant" {
                                bail!("Provider assistant message rejected");
                            }
                            covered.push(*index);
                        }
                        for &index in calls {
                            if item_type(source(input, index)?) != "function_call" {
                                bail!("Provider assistant call rejected");
                            }
                            covered.push(index);
                        }
                        if *provider == ProviderIdV1::Deepseek { group_reasoning(input, *message, calls)?; }
                        prior_assistant = true;
                    },
                    SourceGroupV1::ResponseItem { .. } => bail!("Provider transport reference rejected"),
                }
            }
            if covered != expected { bail!("Provider source coverage or order rejected"); }
            Ok(())
        },
        _ => bail!("Provider result operation rejected"),
    }
}

/// Retain exact reasoning across the original assistant message and its grouped calls.
fn group_reasoning(input: &[Value], message: Option<u32>, calls: &[u32]) -> Result<Option<Value>> {
    let mut reasoning = None;
    for index in message.into_iter().chain(calls.iter().copied()) {
        if let Some(current) = source(input, index)?.get("deepseek_reasoning") {
            if !current.is_string() { bail!("Invalid provider reasoning content"); }
            if reasoning.as_ref().is_some_and(|prior| prior != current) {
                bail!("Conflicting provider reasoning content");
            }
            reasoning = Some(current.clone());
        }
    }
    Ok(reasoning)
}
/// Keep existing compatible text-part conversion; no external media is fetched.
fn text(value: &Value) -> String {
    if let Some(text) = value.as_str() { return text.to_owned(); }
    value.as_array().map(|parts| parts.iter().filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>().join("")).unwrap_or_default()
}
/// Translate only the compatible adapter's supported text and image parts.
fn message_content(value: &Value) -> Result<Value> {
    let Some(parts) = value.as_array() else { return Ok(json!(text(value))); };
    if !parts.iter().any(|part| part["type"] == "input_image") { return Ok(json!(text(value))); }
    parts.iter().map(|part| match part["type"].as_str() {
        Some("input_text" | "text" | "output_text") => Ok(json!({"type":"text","text":part["text"]})),
        Some("input_image") => Ok(json!({"type":"image_url","image_url":{"url":required(part,"image_url")?}})),
        _ => bail!("Unsupported provider media part"),
    }).collect::<Result<Vec<_>>>().map(|parts| json!(parts))
}
/// Map developer to system while preserving text and provider-owned reasoning.
fn compatible_message(item: &Value, preserve_reasoning: bool) -> Result<Value> {
    let original_role = role(item);
    let mut output = json!({"role":if original_role == "developer" {"system"} else {original_role},
        "content":message_content(&item["content"])?});
    if preserve_reasoning {
        if let Some(reasoning) = item.get("deepseek_reasoning") { output["reasoning_content"] = reasoning.clone(); }
    }
    Ok(output)
}
/// Normalize application-only metadata and foreign reasoning in place.
fn normalize_response_item(item: &mut Value) {
    if let Some(object) = item.as_object_mut() {
        object.remove("deepseek_reasoning");
        object.remove("reasoning_content");
        if object.contains_key("role") {
            for key in ["author_id","author_name","run_id","context_content",
                "participation_generation","context_eligible","queued"] { object.remove(key); }
        }
    }
}
/// Borrowed callers receive a normalized copy while owning callers retain their vector allocation.
fn response_item(item: &Value) -> Value {
    let mut item = item.clone();
    normalize_response_item(&mut item);
    item
}

/// Consume a validated request without cloning subscription history/schema vectors.
pub fn materialize_owned(request: RequestV1, result: &ResultV1) -> Result<MaterializedV1> {
    validate_result(&request, result)?;
    match request {
        RequestV1::Project { provider: ProviderIdV1::OpenaiSubscription, instructions, mut input, tools, .. } => {
            for item in &mut input { normalize_response_item(item); }
            Ok(MaterializedV1::Responses { instructions, input, tools })
        },
        other => materialize(&other, result),
    }
}

/// Reconstruct supported content from host inputs after validating reference-only results.
/// Preserve exact strings; compatible media/roles and host metadata use the established normalization.
/// The host still adds model, output limits, streaming, cache key, endpoint and credentials.
pub fn materialize(request: &RequestV1, result: &ResultV1) -> Result<MaterializedV1> {
    validate_result(request, result)?;
    let (RequestV1::Project { provider, instructions, input, tools, .. },
        ResultV1::Projected { projection, .. }) = (request, result) else {
        bail!("Provider materialization requires a projection");
    };
    if projection.transport == TransportV1::Responses {
        return Ok(MaterializedV1::Responses {
            instructions: instructions.clone(), input: input.iter().map(response_item).collect(),
            tools: tools.clone(),
        });
    }
    let preserve_reasoning = *provider == ProviderIdV1::Deepseek;
    let mut messages = vec![json!({"role":"system","content":instructions})];
    for group in &projection.groups {
        match group {
            SourceGroupV1::Message { source_index } =>
                messages.push(compatible_message(source(input, *source_index)?, preserve_reasoning)?),
            SourceGroupV1::ToolResult { source_index } => {
                let item = source(input, *source_index)?;
                messages.push(json!({"role":"tool","tool_call_id":required(item,"call_id")?,
                    "content":text(&item["output"])}));
            },
            SourceGroupV1::Assistant { message, calls } => {
                let mut assistant = if let Some(index) = message {
                    compatible_message(source(input, *index)?, false)?
                } else { json!({"role":"assistant","content":null}) };
                if !calls.is_empty() {
                    let calls = calls.iter().map(|&index| {
                        let item = source(input, index)?;
                        Ok(json!({"id":required(item,"call_id")?,"type":"function","function":{
                            "name":required(item,"name")?,"arguments":required(item,"arguments")?}}))
                    }).collect::<Result<Vec<_>>>()?;
                    assistant["tool_calls"] = json!(calls);
                }
                if preserve_reasoning {
                    if let Some(reasoning) = group_reasoning(input, *message, calls)? {
                        assistant["reasoning_content"] = reasoning;
                    }
                }
                messages.push(assistant);
            },
            SourceGroupV1::ResponseItem { .. } => unreachable!("Validated compatible projection"),
        }
    }
    let tools = tools.iter().map(|tool| json!({"type":"function","function":{
        "name":tool["name"],"description":tool["description"],"parameters":tool["parameters"]}})).collect();
    Ok(MaterializedV1::ChatCompletions { messages, tools })
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Supply one exact source exchange with deliberately nonalphabetic tool order.
    fn request() -> RequestV1 {
        RequestV1::Project { version:1, provider:ProviderIdV1::Deepseek, model:"deepseek-chat".into(),
            instructions:"core".into(), input:vec![
                json!({"role":"user","content":"hello"}),
                json!({"role":"assistant","content":"checking"}),
                json!({"type":"function_call","call_id":"call-1","name":"read","arguments":"{\"q\":\"é\"}","deepseek_reasoning":"thought"}),
                json!({"type":"function_call_output","call_id":"call-1","output":" exact result "}),
                json!({"role":"developer","content":"live skill"}),
            ], tools:vec![json!({"name":"z_read"}),json!({"name":"a_read"})] }
    }
    /// Describe the only canonical source grouping accepted for the shared exchange.
    fn result() -> ResultV1 {
        ResultV1::Projected { version:1, projection:ProjectionV1 {
            provider:ProviderIdV1::Deepseek, transport:TransportV1::ChatCompletions,
            groups:vec![SourceGroupV1::Message{source_index:0},
                SourceGroupV1::Assistant{message:Some(1),calls:vec![2]},
                SourceGroupV1::ToolResult{source_index:3},SourceGroupV1::Message{source_index:4}],
            omitted:vec![],tools:vec![0,1],
        }}
    }
    /// Reject invented references, dropped source, regrouped calls and reordered tool schemas.
    #[test]
    fn forged_reference_plans_cannot_replace_or_reorder_host_content() {
        for mutation in 0..5 {
            let mut result = result();
            let ResultV1::Projected { projection, .. } = &mut result else { unreachable!() };
            match mutation {
                0 => projection.groups[0] = SourceGroupV1::Message{source_index:99},
                1 => { projection.groups.remove(0); },
                2 => projection.tools.reverse(),
                3 => projection.omitted.push(0),
                _ => projection.groups[1] = SourceGroupV1::Assistant{message:Some(0),calls:vec![2]},
            }
            assert!(validate_result(&request(), &result).is_err());
        }
    }
    /// Native system/developer authority and endpoint selection cannot enter through result JSON.
    #[test]
    fn unrestricted_payloads_and_profile_escalation_are_rejected() {
        let injected = json!({"kind":"projected","version":1,"projection":{
            "provider":"deepseek","transport":"chat_completions","groups":[],"omitted":[],"tools":[],
            "endpoint":"https://example.invalid/","headers":{},"cache_key":"forged"}});
        assert!(serde_json::from_value::<ResultV1>(injected).is_err());
        let request = RequestV1::Profile{version:1,provider:ProviderIdV1::Deepseek,model:"deepseek-chat".into()};
        let mut profile = profile_for(ProviderIdV1::Deepseek);
        profile.instruction_role = InstructionRoleV1::Developer;
        assert!(validate_result(&request,&ResultV1::Profile{version:1,profile}).is_err());
    }
    /// Content, tool arguments, results and live-guidance roles come only from original inputs.
    #[test]
    fn references_reconstruct_exact_text_calls_and_tool_order() {
        let MaterializedV1::ChatCompletions { messages, tools } = materialize(&request(), &result()).unwrap()
            else { panic!("Compatible expected") };
        assert_eq!(messages[2]["content"],"checking");
        assert_eq!(messages[2]["reasoning_content"],"thought");
        assert_eq!(messages[2]["tool_calls"][0]["function"]["arguments"],"{\"q\":\"é\"}");
        assert_eq!(messages[3]["content"]," exact result ");
        assert_eq!(messages[4],json!({"role":"system","content":"live skill"}));
        assert_eq!(tools[0]["function"]["name"],"z_read");
        assert_eq!(tools[1]["function"]["name"],"a_read");
    }
    /// Invalid replay is rejected before materialization can produce an upstream request.
    #[test]
    fn conflicting_and_malformed_deepseek_reasoning_is_rejected() {
        let mut request = request();
        let RequestV1::Project { input, .. } = &mut request else { unreachable!() };
        input[1]["deepseek_reasoning"] = json!("different");
        assert!(validate_result(&request,&result()).is_err());
        let RequestV1::Project { input, .. } = &mut request else { unreachable!() };
        input[1]["deepseek_reasoning"] = json!(42);
        assert!(validate_result(&request,&result()).is_err());
    }
    /// Contract versions and outer fields are closed; model IDs retain the existing syntax.
    #[test]
    fn request_contract_versions_and_unknown_fields_fail_closed() {
        let value=json!({"kind":"profile","version":1,"provider":"deepseek","model":"deepseek-chat","api_key":"fixture"});
        assert!(serde_json::from_value::<RequestV1>(value).is_err());
        let request=RequestV1::Profile{version:2,provider:ProviderIdV1::Deepseek,model:"deepseek-chat".into()};
        assert!(validate_request(&request).is_err());
        let request=RequestV1::Profile{version:1,provider:ProviderIdV1::Deepseek,model:"bad\nmodel".into()};
        assert!(validate_request(&request).is_err());
    }
}
