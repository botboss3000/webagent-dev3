//! Pure provider projection planning. Returned positions cannot replace host content.
use anyhow::{bail, Result};
use webagent_provider_contracts::{
    item_type, profile_for, role, validate_request, validate_result, ProjectionV1,
    RequestV1, ResultV1, SourceGroupV1, TransportV1, CONTRACT_VERSION,
};
/// The service reports the package version attested by its registered descriptor.
pub const IMPLEMENTATION_VERSION: &str = "0.1.1";

// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Plan native provider profiles and ordered source groups; Calls provider-contracts validate_request and validate_result.","span_lines":60,"configuration":{"status":"not_applicable","rationale":"Reference coverage, provider identity, replay rules and transport authority are fixed protocol invariants."}}
/// Plan installed provider formatting without files, credentials, network or side effects.
/// The shared host contract validates ordered source coverage before any reconstruction.
pub fn project(request: &RequestV1) -> Result<ResultV1> {
    validate_request(request)?;
    let result = match request {
        RequestV1::Profile { provider, .. } => ResultV1::Profile {
            version: CONTRACT_VERSION, profile: profile_for(*provider),
        },
        RequestV1::Project { provider, input, tools, .. } => {
            let profile = profile_for(*provider);
            if profile.transport == TransportV1::Decision { bail!("Provider does not support chat projection"); }
            let mut groups = Vec::new();
            let mut omitted = Vec::new();
            for (index, item) in input.iter().enumerate() {
                let index = index as u32;
                if profile.transport == TransportV1::Responses {
                    groups.push(SourceGroupV1::ResponseItem { source_index: index });
                    continue;
                }
                match item_type(item) {
                    "reasoning" => omitted.push(index),
                    "function_call" => {
                        if let Some(SourceGroupV1::Assistant { calls, .. }) = groups.last_mut() {
                            calls.push(index);
                        } else {
                            groups.push(SourceGroupV1::Assistant { message: None, calls: vec![index] });
                        }
                    },
                    "function_call_output" => groups.push(SourceGroupV1::ToolResult { source_index: index }),
                    "message" if role(item) == "assistant" =>
                        groups.push(SourceGroupV1::Assistant { message: Some(index), calls: vec![] }),
                    "message" => groups.push(SourceGroupV1::Message { source_index: index }),
                    _ => bail!("Unsupported provider input item"),
                }
            }
            ResultV1::Projected {
                version: CONTRACT_VERSION,
                projection: ProjectionV1 {
                    provider: *provider, transport: profile.transport, groups, omitted,
                    tools: (0..tools.len()).map(|index| index as u32).collect(),
                },
            }
        },
    };
    validate_result(request, &result)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use webagent_provider_contracts::{
        materialize, CacheControlV1, InstructionRoleV1, MaterializedV1, ProviderIdV1,
    };
    /// Fixtures bind only pure source content; no transport authorization enters planning.
    fn request(provider: ProviderIdV1) -> RequestV1 {
        RequestV1::Project { version:1, provider, model:"vendor/model".into(), instructions:"core".into(),
            input:vec![
                json!({"role":"user","content":"inspect"}),
                json!({"type":"reasoning","id":"rs-fixture","encrypted_content":"opaque-fixture","summary":[]}),
                json!({"role":"assistant","content":"checking"}),
                json!({"type":"function_call","call_id":"call-1","name":"read","arguments":"{}","deepseek_reasoning":"thought"}),
                json!({"type":"function_call_output","call_id":"call-1","output":" exact result "}),
                json!({"role":"developer","content":"live guidance"}),
            ], tools:vec![json!({"name":"z_read"}),json!({"name":"a_read"})] }
    }
    /// Each transport retains its established role, replay and cache profile for all models.
    #[test]
    fn profiles_are_endpoint_specific_without_new_transports() {
        for provider in [ProviderIdV1::OpenaiSubscription,ProviderIdV1::Deepseek,
            ProviderIdV1::OpenaiApi,ProviderIdV1::Openrouter,ProviderIdV1::Abacus,
            ProviderIdV1::Gemini,ProviderIdV1::Groq,ProviderIdV1::Together,
            ProviderIdV1::Mistral,ProviderIdV1::Fireworks,ProviderIdV1::Xai,
            ProviderIdV1::Perplexity,ProviderIdV1::Deepinfra,ProviderIdV1::Typesafe] {
            let result = project(&RequestV1::Profile{version:1,provider,model:"fixture".into()}).unwrap();
            let ResultV1::Profile { profile, .. } = result else { panic!("Profile expected") };
            match provider {
                ProviderIdV1::OpenaiSubscription => {
                    assert!(profile.append_instructions);
                    assert_eq!(profile.instruction_role,InstructionRoleV1::Developer);
                    assert_eq!(profile.cache_control,CacheControlV1::SubscriptionSessionKey);
                },
                ProviderIdV1::Deepseek => {
                    assert!(profile.append_instructions);
                    assert_eq!(profile.instruction_role,InstructionRoleV1::System);
                    assert_eq!(profile.cache_control,CacheControlV1::ImplicitPrefix);
                },
                _ => assert!(!profile.append_instructions),
            }
        }
    }
    /// DeepSeek keeps one assistant/tool-call exchange and the original reasoning value.
    #[test]
    fn deepseek_groups_text_calls_and_reasoning_before_the_result() {
        let request = request(ProviderIdV1::Deepseek);
        let result = project(&request).unwrap();
        let ResultV1::Projected { projection, .. } = &result else { panic!("Projection expected") };
        assert_eq!(projection.omitted,vec![1]);
        assert_eq!(projection.groups[1],SourceGroupV1::Assistant{message:Some(2),calls:vec![3]});
        assert!(!serde_json::to_string(&result).unwrap().contains("exact result"));
        let MaterializedV1::ChatCompletions { messages, tools } = materialize(&request,&result).unwrap()
            else { panic!("Compatible expected") };
        assert_eq!(messages.len(),5);
        assert_eq!(messages[2]["reasoning_content"],"thought");
        assert_eq!(messages[2]["content"],"checking");
        assert_eq!(messages[3]["tool_call_id"],"call-1");
        assert_eq!(messages[3]["content"]," exact result ");
        assert_eq!(messages[4],json!({"role":"system","content":"live guidance"}));
        assert_eq!(tools[0]["function"]["name"],"z_read");
        assert_eq!(tools[1]["function"]["name"],"a_read");
    }
    /// Subscription replay preserves opaque Responses reasoning but removes foreign private fields.
    #[test]
    fn subscription_keeps_responses_items_and_native_developer_placement() {
        let mut request = request(ProviderIdV1::OpenaiSubscription);
        let RequestV1::Project { input, .. } = &mut request else { unreachable!() };
        input[0]["author_id"] = json!("internal-author");
        input[2]["reasoning_content"] = json!("foreign");
        let result = project(&request).unwrap();
        let MaterializedV1::Responses { instructions,input,tools } = materialize(&request,&result).unwrap()
            else { panic!("Responses expected") };
        assert_eq!(instructions,"core");
        assert_eq!(input[1]["encrypted_content"],"opaque-fixture");
        assert!(input[0].get("author_id").is_none());
        assert!(input[2].get("reasoning_content").is_none());
        assert!(input[3].get("deepseek_reasoning").is_none());
        assert_eq!(input[3]["arguments"],"{}");
        assert_eq!(input[5]["role"],"developer");
        assert_eq!(tools[0]["name"],"z_read");
    }
    /// Compatible endpoints do not inherit or validate another provider's private reasoning.
    #[test]
    fn other_compatible_endpoints_exclude_foreign_reasoning() {
        let mut request = request(ProviderIdV1::Openrouter);
        let RequestV1::Project { input, .. } = &mut request else { unreachable!() };
        input[3]["deepseek_reasoning"] = json!(42);
        let result = project(&request).unwrap();
        let MaterializedV1::ChatCompletions { messages,.. } = materialize(&request,&result).unwrap()
            else { panic!("Compatible expected") };
        assert!(messages.iter().all(|message|message.get("reasoning_content").is_none()));
        assert_eq!(messages[2]["tool_calls"][0]["function"]["arguments"],"{}");
    }
    /// Media references are transformed structurally; no component fetch or URL selection occurs.
    #[test]
    fn compatible_media_preserves_the_original_image_reference() {
        let mut request = request(ProviderIdV1::Deepseek);
        let RequestV1::Project { input, .. } = &mut request else { unreachable!() };
        input[0]["content"] = json!([
            {"type":"input_text","text":"look"},
            {"type":"input_image","image_url":"https://example.invalid/image.png"},
        ]);
        let result=project(&request).unwrap();
        let MaterializedV1::ChatCompletions { messages,.. }=materialize(&request,&result).unwrap()
            else {panic!("Compatible expected")};
        assert_eq!(messages[1]["content"],json!([
            {"type":"text","text":"look"},
            {"type":"image_url","image_url":{"url":"https://example.invalid/image.png"}},
        ]));
    }
    /// Malformed/conflicting DeepSeek replay cannot produce a reference plan.
    #[test]
    fn malformed_and_conflicting_reasoning_fail_before_materialization() {
        let mut request=request(ProviderIdV1::Deepseek);
        let RequestV1::Project { input,.. }=&mut request else {unreachable!()};
        input[2]["deepseek_reasoning"]=json!("different");
        assert!(project(&request).is_err());
        let RequestV1::Project { input,.. }=&mut request else {unreachable!()};
        input[2]["deepseek_reasoning"]=json!(42);
        assert!(project(&request).is_err());
    }
}
