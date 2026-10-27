//! Ephemeral call preparation, not proof of live authorization.
//! No state is persisted or cloneable; callers revalidate authority at dispatch.
use super::*;
use crate::transport::Projection;

pub struct CallDraft<'a> {
    selection: &'a ProviderSelection,
    reasoning_effort: Option<&'a str>,
    input: Vec<Value>,
    tools: Vec<Value>,
    session: &'a str,
    instructions: &'a str,
}

pub struct ValidatedCall<'a> {
    request: PreparedRequest<'a>,
}

pub struct ReadyCall<'a> {
    transport: ReadyTransport<'a>,
}

pub(super) struct SubscriptionRequest<'a> {
    pub(super) max_output_tokens: Option<usize>,
    pub(super) reasoning_effort: Option<&'a str>,
    pub(super) model: &'a str,
    pub(super) input: Vec<Value>,
    pub(super) tools: Vec<Value>,
    pub(super) session: &'a str,
    pub(super) instructions: &'a str,
}

pub(super) struct SubscriptionCredentials<'a> {
    pub(super) access_token: &'a str,
    pub(super) account_id: Option<&'a str>,
    pub(super) residency: Option<&'a str>,
}

enum PreparedRequest<'a> {
    Subscription(SubscriptionRequest<'a>),
    Compatible { body: Value, endpoint: &'static str },
    Bridge { body: Value, session: &'a str },
}

enum ReadyTransport<'a> {
    Subscription(SubscriptionRequest<'a>, SubscriptionCredentials<'a>),
    Compatible {
        body: Value,
        endpoint: &'static str,
        api_key: &'a str,
    },
    Bridge { body: Value, session: &'a str, link: &'a str },
}

// @code-index {"version":1,"feature":"provider-projection","aliases":["provider formatting","pure provider component"],"purpose":"Host projection preparation precedes credential binding and network dispatch. Config: config/engine-components.json -> disabled; provider descriptor -> versions and limits.","span_lines":80,"configuration":{"status":"configured","references":[{"path":"config/engine-components.json","pointers":["/disabled"]},{"path":"plugins/engine/provider-projection/component.json","pointers":["/interface_version","/implementation_version","/limits"]}],"opportunities":[]}}
impl<'a> CallDraft<'a> {
    /// Own the ephemeral input/schema vectors while borrowing the host-selected route and instructions.
    pub fn new(
        selection: &'a ProviderSelection,
        input: Vec<Value>,
        tools: Vec<Value>,
        session: &'a str,
        instructions: &'a str,
    ) -> Self {
        Self {
            selection,
            reasoning_effort: None,
            input,
            tools,
            session,
            instructions,
        }
    }

    pub fn with_reasoning_effort(mut self, effort: Option<&'a str>) -> Self {
        self.reasoning_effort=effort; self
    }

    /// Resolve provider projection before the caller reacquires its dispatch gate.
    /// No credentials reach the component; live authority must still be rechecked.
    pub async fn prepare<P:Projection>(
        mut self,
        projection:&P,
        cancel:&CancellationToken,
    )->Result<ValidatedCall<'a>> {
        let request = self.projection_request()?;
        if cancel.is_cancelled() {
            return Err(typed_failure(crate::model_selection::FailureKind::Cancelled,
                false, "Provider request cancelled"));
        }
        let projection = projection.project(&request,cancel).await
            .map_err(|_| typed_failure(
                if cancel.is_cancelled() { crate::model_selection::FailureKind::Cancelled }
                else { crate::model_selection::FailureKind::Protocol },
                false, "Provider projection unavailable or rejected"))?;
        self.validate_prepared(request, projection)
    }

    /// Linked fixtures use the same planner and contract; live builds require the registered service.
    pub fn validate_with(mut self, project:impl FnOnce(&webagent_provider_contracts::RequestV1)->Result<webagent_provider_contracts::ResultV1>)->Result<ValidatedCall<'a>> {
        let request = self.projection_request()?;
        let projection = project(&request)
            .map_err(|_| typed_failure(crate::model_selection::FailureKind::MalformedRequest,
                false, "Invalid Provider request"))?;
        self.validate_prepared(request, projection)
    }
    #[cfg(test)]
    pub fn validate(self)->Result<ValidatedCall<'a>> {self.validate_with(webagent_provider_projection::project)}


    /// Normalize host-only metadata and budget inputs before sending private context over the pipe.
    fn projection_request(&mut self) -> Result<webagent_provider_contracts::RequestV1> {
        super::validate_reasoning_effort(self.selection.provider,&self.selection.model,self.reasoning_effort)?;
        if self.instructions.is_empty() {
            return Err(typed_failure(crate::model_selection::FailureKind::MalformedRequest,
                false, "Invalid provider instructions"));
        }
        for item in &mut self.input {
            if let Some(object) = item.as_object_mut() {
                if self.selection.provider != ProviderId::Deepseek {
                    object.remove("deepseek_reasoning");
                    object.remove("reasoning_content");
                }
                if object.contains_key("role") {
                    for key in ["author_id","author_name","run_id","context_content",
                        "participation_generation","context_eligible","queued"] {
                        object.remove(key);
                    }
                }
            }
        }
        if let Some(budget) = self.selection.input_budget_bytes()? {
            #[derive(serde::Serialize)]
            struct Envelope<'a> { instructions: &'a str, input: &'a [Value], tools: &'a [Value] }
            let bytes = webagent_context_contracts::context_accounting::json_size(&Envelope {
                instructions:self.instructions, input:&self.input, tools:&self.tools,
            })?;
            if bytes > budget {
                return Err(typed_failure(crate::model_selection::FailureKind::MalformedRequest,
                    false, "Model input budget exceeded"));
            }
        }
        let request = webagent_provider_contracts::RequestV1::Project {
            version:webagent_provider_contracts::CONTRACT_VERSION,
            provider:projection_id(self.selection.provider),
            model:self.selection.model.clone(),
            instructions:self.instructions.to_owned(),
            input:std::mem::take(&mut self.input),
            tools:std::mem::take(&mut self.tools),
        };
        webagent_provider_contracts::validate_request(&request)
            .map_err(|_| typed_failure(crate::model_selection::FailureKind::MalformedRequest,
                false, "Invalid Provider request"))?;
        // Shape errors precede the legacy unsupported-transport error; no service
        // or credential work is attempted for a Decision-only model.
        if !matches!(self.selection.provider, ProviderId::OpenaiSubscription | ProviderId::ClaudeBridge)
            && self.selection.provider.chat_endpoint().is_none() {
            return Err(anyhow!("Provider does not use API keys"));
        }
        Ok(request)
    }

    /// Accept references only, reconstruct original content, then add host-owned wire parameters.
    fn validate_prepared(
        self,
        request: webagent_provider_contracts::RequestV1,
        projection: webagent_provider_contracts::ResultV1,
    ) -> Result<ValidatedCall<'a>> {
        match &request {
            webagent_provider_contracts::RequestV1::Project { provider, model, instructions, .. }
                if *provider == projection_id(self.selection.provider)
                    && model == &self.selection.model && instructions == self.instructions => {},
            _ => return Err(typed_failure(crate::model_selection::FailureKind::Protocol,
                false, "Provider projection request identity changed")),
        }
        let materialized = webagent_provider_contracts::materialize_owned(request, &projection)
            .map_err(|_| typed_failure(crate::model_selection::FailureKind::Protocol,
                false, "Invalid Provider projection"))?;
        let request = match materialized {
            webagent_provider_contracts::MaterializedV1::Responses { instructions, input, tools } => {
                if self.selection.provider != ProviderId::OpenaiSubscription
                    || instructions != self.instructions {
                    return Err(typed_failure(crate::model_selection::FailureKind::Protocol,
                        false, "Invalid Provider projection"));
                }
                PreparedRequest::Subscription(SubscriptionRequest {
                    reasoning_effort:self.reasoning_effort,
                    max_output_tokens:self.selection.limits.as_ref().map(|limits|limits.output_tokens),
                    model:&self.selection.model, input, tools, session:self.session,
                    instructions:self.instructions,
                })
            },
            webagent_provider_contracts::MaterializedV1::ChatCompletions { messages, tools }
                if self.selection.provider == ProviderId::ClaudeBridge => {
                let mut body=compatible_body(self.selection,messages,tools);
                if let Some(effort)=self.reasoning_effort {body["reasoning_effort"]=json!(effort);}
                PreparedRequest::Bridge { body, session:self.session }
            },
            webagent_provider_contracts::MaterializedV1::ChatCompletions { messages, tools } => {
                let endpoint = self.selection.provider.chat_endpoint()
                    .ok_or_else(|| anyhow!("Provider does not use API keys"))?;
                let mut body=compatible_body(self.selection,messages,tools);
                if let Some(effort)=self.reasoning_effort {
                    if self.selection.provider==ProviderId::Openrouter {body["reasoning"]=json!({"effort":effort});}
                    // DeepSeek has no "none" level; thinking is switched off instead.
                    else if self.selection.provider==ProviderId::Deepseek && effort=="none" {body["thinking"]=json!({"type":"disabled"});}
                    else {body["reasoning_effort"]=json!(effort);}
                }
                PreparedRequest::Compatible { body, endpoint }
            },
        };
        Ok(ValidatedCall { request })
    }

}

/// Host-owned network framing; the projection component never chooses limits,
/// tool choice, endpoints, streaming options, cache keys or credentials.
pub(super) fn compatible_body(selection: &ProviderSelection, messages: Vec<Value>, tools: Vec<Value>) -> Value {
    let mut body = json!({"model":selection.model,"messages":messages,"stream":true,
        "stream_options":{"include_usage":true},"max_tokens":8192});
    if !tools.is_empty() {
        body["tools"] = json!(tools);
        body["tool_choice"] = json!("auto");
    }
    if let Some(limits) = &selection.limits {
        if selection.provider == ProviderId::OpenaiApi {
            body.as_object_mut().unwrap().remove("max_tokens");
            body["max_completion_tokens"] = json!(limits.output_tokens);
        } else {
            body["max_tokens"] = json!(limits.output_tokens);
        }
    }
    body
}

impl<'a> ValidatedCall<'a> {
    /// Bind only the selected transport's credentials after the caller rechecks live authority.
    /// This synchronous step preserves cancellation/credential error precedence; it starts no network request.
    pub fn bind_credentials(
        self,
        tokens: &'a Value,
        cancel: &CancellationToken,
    ) -> Result<ReadyCall<'a>> {
        let transport = match self.request {
            PreparedRequest::Subscription(request) => {
                // Preserve subscription cancellation-before-credentials precedence.
                if cancel.is_cancelled() {
                    return Err(typed_failure(
                        crate::model_selection::FailureKind::Cancelled,
                        false,
                        "Provider request cancelled",
                    ));
                }
                let credentials = SubscriptionCredentials {
                    access_token: required(tokens, "access_token")?,
                    account_id: tokens["account_id"].as_str().filter(|s| !s.is_empty()),
                    residency: tokens["residency"].as_str().filter(|s| !s.is_empty()),
                };
                ReadyTransport::Subscription(request, credentials)
            }
            PreparedRequest::Compatible { body, endpoint } => ReadyTransport::Compatible {
                body,
                endpoint,
                api_key: required(tokens, "api_key")?,
            },
            PreparedRequest::Bridge { body, session } => ReadyTransport::Bridge {
                body, session, link: required(tokens, "api_key")?,
            },
        };
        Ok(ReadyCall { transport })
    }
}

impl Provider {
    /// Consumes one locally prepared call. Live authority remains with the caller.
    pub async fn dispatch(
        &self,
        call: ReadyCall<'_>,
        delta: mpsc::Sender<String>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        match call.transport {
            ReadyTransport::Subscription(request, credentials) => {
                self.send_subscription(request, credentials, delta, cancel)
                    .await
            }
            ReadyTransport::Compatible {
                body,
                endpoint,
                api_key,
            } => {
                self.send_compatible(body, endpoint, api_key, delta, cancel)
                    .await
            }
            ReadyTransport::Bridge { body, session, link } => {
                let queue = self.bridge.as_ref().ok_or_else(|| typed_failure(
                    crate::model_selection::FailureKind::ModelUnavailable, false, "Claude Bridge is unavailable here"))?;
                let reply = queue.exchange(super::BridgeCall { link:link.to_owned(), session:session.to_owned(), body, cancel }).await?;
                super::deepseek::bridge_result(&reply)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
use crate::transport::Projection;
    use crate::model_selection::FailureKind;

    /// Extract the classified failure expected by preparation and dispatch-boundary fixtures.
    fn kind(error: &anyhow::Error) -> FailureKind {
        error.downcast_ref::<ProviderFailure>().unwrap().kind
    }

    /// A malformed component result is rejected before credentials or network preparation.
    #[test]
    fn projection_cannot_forge_transport_or_history_references() {
        let selection = ProviderSelection { limits:None,
            provider:ProviderId::OpenaiSubscription, model:"fixture".into() };
        let mut draft = CallDraft::new(&selection,
            vec![json!({"role":"user","content":"original"})], vec![], "s", "core");
        let request = draft.projection_request().unwrap();
        let projection = webagent_provider_contracts::ResultV1::Projected {
            version:1, projection:webagent_provider_contracts::ProjectionV1 {
                provider:webagent_provider_contracts::ProviderIdV1::Deepseek,
                transport:webagent_provider_contracts::TransportV1::ChatCompletions,
                groups:vec![webagent_provider_contracts::SourceGroupV1::Message { source_index:0 }],
                omitted:vec![], tools:vec![],
            },
        };
        let error = draft.validate_prepared(request, projection).err().unwrap();
        assert_eq!(kind(&error), FailureKind::Protocol);
    }

    /// Verify long instruction bodies are accepted under the complete envelope budget rather than an unrelated fixed prompt cap.
    #[test]
    fn large_instructions_are_valid_when_the_complete_model_input_fits() {
        for provider in [ProviderId::OpenaiSubscription,ProviderId::Deepseek] {
            let selection=ProviderSelection{limits:None,provider,model:"fixture".into()};
            let text="x".repeat(120000);
            assert!(CallDraft::new(&selection,vec![],vec![],"s",&text).validate().is_ok());
        }
    }

    /// Verify invalid instructions fail as malformed input before unsupported transport or missing credential checks.
    #[test]
    fn instruction_failure_precedes_transport_and_credentials() {
        for provider in [
            ProviderId::OpenaiSubscription,
            ProviderId::Deepseek,
            ProviderId::Typesafe,
        ] {
            let selection = ProviderSelection { limits:None,
                provider,
                model: "fixture".into(),
            };
            for instructions in ["".to_owned()] {
                let error = CallDraft::new(&selection, vec![], vec![], "s", &instructions)
                    .validate()
                    .err()
                    .unwrap();
                assert_eq!(kind(&error), FailureKind::MalformedRequest);
                assert!(
                    !error
                        .downcast_ref::<ProviderFailure>()
                        .unwrap()
                        .response_started
                );
            }
        }
    }

    /// Verify Decision-only selections retain malformed-input precedence before their unsupported chat error.
    #[test]
    fn compatible_body_failure_precedes_unsupported_transport() {
        let selection = ProviderSelection { limits:None,
            provider: ProviderId::Typesafe,
            model: "fixture".into(),
        };
        let error = CallDraft::new(
            &selection,
            vec![json!({"type":"unknown"})],
            vec![],
            "s",
            "help",
        )
        .validate()
        .err()
        .unwrap();
        assert_eq!(kind(&error), FailureKind::MalformedRequest);
        let error = CallDraft::new(&selection, vec![], vec![], "s", "help")
            .validate()
            .err()
            .unwrap();
        assert_eq!(error.to_string(), "Provider does not use API keys");
    }

    /// Verify subscription cancellation and compatible missing-key errors preserve their established precedence.
    #[test]
    fn cancellation_and_missing_credentials_keep_transport_precedence() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let missing = json!({});
        let subscription = ProviderSelection { limits:None,
            provider: ProviderId::OpenaiSubscription,
            model: "fixture".into(),
        };
        let error = CallDraft::new(&subscription, vec![], vec![], "s", "help")
            .validate()
            .unwrap()
            .bind_credentials(&missing, &cancel)
            .err()
            .unwrap();
        assert_eq!(kind(&error), FailureKind::Cancelled);
        let compatible = ProviderSelection { limits:None,
            provider: ProviderId::Deepseek,
            model: "fixture".into(),
        };
        let error = CallDraft::new(&compatible, vec![], vec![], "s", "help")
            .validate()
            .unwrap()
            .bind_credentials(&missing, &cancel)
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            required(&missing, "api_key").err().unwrap().to_string()
        );
        let error = CallDraft::new(&subscription, vec![], vec![], "s", "help")
            .validate()
            .unwrap()
            .bind_credentials(&missing, &CancellationToken::new())
            .err()
            .unwrap();
        assert_eq!(
            error.to_string(),
            required(&missing, "access_token")
                .err()
                .unwrap()
                .to_string()
        );
    }

    /// Verify subscription preparation transfers original vector allocations and ignores unrelated API-key fields.
    #[test]
    fn subscription_preparation_moves_history_and_binds_only_subscription_fields() {
        let selection = ProviderSelection { limits:None,
            provider: ProviderId::OpenaiSubscription,
            model: "fixture".into(),
        };
        // Subscription historically permits arbitrary JSON input; do not add compatible validation.
        let input = vec![json!({"type":"opaque_subscription_item"})];
        let tools = vec![json!({"type":"function","name":"read"})];
        let input_ptr = input.as_ptr();
        let tools_ptr = tools.as_ptr();
        // Public dummy credential recognized by the Git source scanner.
        let tokens = json!({"access_token":"fixture-token","api_key":"ignore","account_id":"","residency":"eu"});
        let ready = CallDraft::new(&selection, input, tools, "session", "help")
            .validate()
            .unwrap()
            .bind_credentials(&tokens, &CancellationToken::new())
            .unwrap();
        let ReadyTransport::Subscription(request, credentials) = ready.transport else {
            panic!("subscription transport expected")
        };
        assert_eq!(request.input.as_ptr(), input_ptr);
        assert_eq!(request.tools.as_ptr(), tools_ptr);
        assert_eq!(request.input[0]["type"], "opaque_subscription_item");
        assert_eq!(credentials.access_token, "fixture-token");
        assert_eq!(credentials.account_id, None);
        assert_eq!(credentials.residency, Some("eu"));
    }

    /// A bridge call reaches the host queue as a compatible body and its reply is validated like a stream.
    #[tokio::test]
    async fn bridge_calls_round_trip_through_the_host_queue() {
        struct Fake(std::sync::Mutex<Option<(String, String, Value)>>);
        impl super::super::BridgeQueue for Fake {
            fn exchange(&self, call: super::super::BridgeCall) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>> {
                *self.0.lock().unwrap() = Some((call.link, call.session, call.body));
                Box::pin(async { Ok(json!({"message":{"role":"assistant","content":"",
                    "tool_calls":[{"id":"call_1","type":"function","function":{"name":"read","arguments":"{}"}}]}})) })
            }
        }
        let fake = std::sync::Arc::new(Fake(std::sync::Mutex::new(None)));
        let provider = Provider::new().with_bridge(fake.clone());
        let selection = ProviderSelection { limits:None, provider:ProviderId::ClaudeBridge, model:"claude-opus-5-5".into() };
        let tools = vec![json!({"name":"read","description":"inspect","parameters":{"type":"object"}})];
        let tokens = json!({"api_key":"link"});
        // Effort levels come from the live provider file, which lands after this code.
        let effort = crate::providers::efforts(ProviderId::ClaudeBridge, "claude-opus-5-5")
            .is_ok_and(|e| !e.levels.is_empty()).then_some("high");
        let ready = CallDraft::new(&selection, vec![json!({"role":"user","content":"hi"})], tools, "chat", "guide")
            .with_reasoning_effort(effort).validate().unwrap()
            .bind_credentials(&tokens, &CancellationToken::new()).unwrap();
        let (sender, _receiver) = mpsc::channel(1);
        let result = provider.dispatch(ready, sender, CancellationToken::new()).await.unwrap();
        assert_eq!(result["output"][0]["type"], "function_call");
        assert_eq!(result["output"][0]["name"], "read");
        let (link, session, body) = fake.0.lock().unwrap().take().unwrap();
        assert_eq!((link.as_str(), session.as_str()), ("link", "chat"));
        assert_eq!(body["model"], "claude-opus-5-5");
        assert_eq!(body.get("reasoning_effort").and_then(Value::as_str), effort);
        assert_eq!(body["tools"][0]["function"]["name"], "read");
        // Without a host queue the call fails as unavailable rather than reaching the network.
        let ready = CallDraft::new(&selection, vec![json!({"role":"user","content":"hi"})], vec![], "chat", "guide")
            .validate().unwrap().bind_credentials(&tokens, &CancellationToken::new()).unwrap();
        let (sender, _receiver) = mpsc::channel(1);
        assert!(Provider::new().dispatch(ready, sender, CancellationToken::new()).await.is_err());
    }

    /// Verify exact compatible text/schema framing while retaining reasoning only for DeepSeek.
    #[test]
    fn compatible_preparation_preserves_wire_values_and_reasoning_rules() {
        for provider in [ProviderId::Deepseek, ProviderId::Openrouter] {
            let selection = ProviderSelection { limits:None,
                provider,
                model: "vendor/model".into(),
            };
            let input = vec![
                json!({"role":"assistant","content":"é\ntext","deepseek_reasoning":"thought"}),
            ];
            let tools =
                vec![json!({"name":"read","description":"inspect","parameters":{"type":"object"}})];
            let tokens = json!({"api_key":"api","access_token":"ignore","account_id":"ignore"});
            let ready = CallDraft::new(&selection, input, tools, "s", "guide")
                .validate()
                .unwrap()
                .bind_credentials(&tokens, &CancellationToken::new())
                .unwrap();
            let ReadyTransport::Compatible {
                body,
                endpoint,
                api_key,
            } = ready.transport
            else {
                panic!("compatible transport expected")
            };
            let mut prior = json!({"role":"assistant","content":"é\ntext"});
            if provider == ProviderId::Deepseek {
                prior["reasoning_content"] = json!("thought");
            }
            assert_eq!(
                body,
                json!({
                    "model":"vendor/model","messages":[{"role":"system","content":"guide"},prior],
                    "stream":true,"stream_options":{"include_usage":true},"max_tokens":8192,
                    "tools":[{"type":"function","function":{"name":"read","description":"inspect","parameters":{"type":"object"}}}],
                    "tool_choice":"auto"
                })
            );
            assert_eq!(endpoint, provider.chat_endpoint().unwrap());
            assert_eq!(api_key, "api");
        }
    }

    /// DeepSeek "none" turns thinking off; other levels travel as reasoning_effort; unlisted levels are refused.
    #[test]
    fn deepseek_effort_maps_to_thinking_or_reasoning_effort() {
        let selection = ProviderSelection { limits:None, provider:ProviderId::Deepseek, model:"deepseek-flash".into() };
        let tokens = json!({"api_key":"api"});
        let body = |effort| {
            let ready = CallDraft::new(&selection, vec![json!({"role":"user","content":"hi"})], vec![], "s", "guide")
                .with_reasoning_effort(Some(effort)).validate()?.bind_credentials(&tokens, &CancellationToken::new())?;
            let ReadyTransport::Compatible { body, .. } = ready.transport else { panic!("compatible transport expected") };
            Ok::<_, anyhow::Error>(body)
        };
        let off = body("none").unwrap();
        assert_eq!(off["thinking"], json!({"type":"disabled"}));
        assert!(off.get("reasoning_effort").is_none());
        assert_eq!(body("max").unwrap()["reasoning_effort"], "max");
        assert!(body("medium").is_err());
    }

    /// Verify malformed, unsupported, credential-less and cancelled calls cannot reach a local transport listener.
    #[tokio::test]
    async fn rejected_preparation_and_cancelled_ready_calls_never_connect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let provider = Provider::for_test(format!(
            "http://{}/responses",
            listener.local_addr().unwrap()
        ));
        for transport in [
            ProviderId::OpenaiSubscription,
            ProviderId::Deepseek,
            ProviderId::Typesafe,
        ] {
            let selection = ProviderSelection { limits:None,
                provider: transport,
                model: "fixture".into(),
            };
            assert!(CallDraft::new(&selection, vec![], vec![], "s", "")
                .validate()
                .is_err());
            if transport == ProviderId::Typesafe {
                assert!(CallDraft::new(&selection, vec![], vec![], "s", "help")
                    .validate()
                    .is_err());
                continue;
            }
            let missing = json!({});
            assert!(CallDraft::new(&selection, vec![], vec![], "s", "help")
                .validate()
                .unwrap()
                .bind_credentials(&missing, &CancellationToken::new())
                .is_err());
            let tokens = json!({"access_token":"fixture","api_key":"fixture"});
            let cancel = CancellationToken::new();
            let ready = CallDraft::new(&selection, vec![], vec![], "s", "help")
                .validate()
                .unwrap()
                .bind_credentials(&tokens, &cancel)
                .unwrap();
            cancel.cancel(); // Revocation of the token after preparation is still observed at send.
            let (tx, _rx) = mpsc::channel(1);
            let error = provider.dispatch(ready, tx, cancel).await.err().unwrap();
            assert_eq!(kind(&error), FailureKind::Cancelled);
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}

#[cfg(test)]
mod limit_tests {
 use super::*;
use crate::transport::Projection;
 /// Verify configured limits bound the complete input and select the compatible output-token field.
 #[test]
 fn limits_bound_all_call_inputs_and_compatible_output() {
  let selection=ProviderSelection::parse(&json!({"provider":"deepseek","model":"fixture",
    "limits":{"input_tokens":100,"output_tokens":17}}),"unused").unwrap();
  let ready=CallDraft::new(&selection,vec![],vec![],"s","help").validate().unwrap();
  let PreparedRequest::Compatible{body,..}=ready.request else {panic!("compatible")};
  assert_eq!(body["max_tokens"],17);
  assert!(CallDraft::new(&selection,vec![json!({"role":"user","content":"x".repeat(500)})],vec![],"s","help").validate().is_err());
 }
 /// Verify the subscription prepared request carries an explicitly configured output limit.
 #[test]
 fn subscription_explicit_output_limit_is_prepared_and_defaults_omit_it() {
  let selection=ProviderSelection::parse(&json!({"provider":"openai_subscription","model":"gpt-6-luna",
    "limits":{"input_tokens":150000,"output_tokens":2000}}),"unused").unwrap();
  let ready=CallDraft::new(&selection,vec![],vec![],"s","help").validate().unwrap();
  let PreparedRequest::Subscription(request)=ready.request else {panic!("subscription")};
  assert_eq!(request.max_output_tokens,Some(2000));
 }
}

#[cfg(test)]
mod shared_message_wire_tests {
 use super::*;
use crate::transport::Projection;
 /// Verify first and recovered user messages omit host metadata without altering opaque reasoning or tool argument/result strings.
 #[test]
 fn subscription_first_and_recovered_user_messages_strip_internal_metadata() {
  let selection=ProviderSelection { limits:None,provider:ProviderId::OpenaiSubscription,model:"fixture".into() };
  let call=json!({"type":"function_call","id":"fc-provider","call_id":"call-1","name":"read","arguments":"{\"run_id\":\"argument-data\"}"});
  let output=json!({"type":"function_call_output","call_id":"call-1","output":"{\"author_id\":\"result-data\"}"});
  let reasoning=json!({"type":"reasoning","id":"reason-provider","encrypted_content":"opaque","summary":[]});
  for recovered in [false,true] {
   let mut input=vec![];
   if recovered {input.extend([call.clone(),output.clone(),reasoning.clone()]);}
   input.push(json!({"role":"user","content":"Shared request","type":"message",
    "author_id":"credential-owner","author_name":"Owner","run_id":"internal-run",
    "context_content":"attachment preparation","participation_generation":9,
    "context_eligible":true,"queued":false}));
   let ready=CallDraft::new(&selection,input,vec![],"s","guide").validate().unwrap();
   let PreparedRequest::Subscription(request)=ready.request else {panic!("subscription")};
   assert_eq!(request.input.last().unwrap(),&json!({"role":"user","content":"Shared request","type":"message"}));
   if recovered {
    assert_eq!(request.input[0],call);assert_eq!(request.input[1],output);assert_eq!(request.input[2],reasoning);
   }
  }
 }
}
