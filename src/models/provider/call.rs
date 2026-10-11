//! Ephemeral call preparation, not proof of live authorization.
//! No state is persisted or cloneable; callers revalidate authority at dispatch.
use super::*;

pub(crate) struct CallDraft<'a> {
    selection: &'a ProviderSelection,
    input: Vec<Value>,
    tools: Vec<Value>,
    session: &'a str,
    instructions: &'a str,
}

pub(crate) struct ValidatedCall<'a> {
    request: PreparedRequest<'a>,
}

pub(crate) struct ReadyCall<'a> {
    transport: ReadyTransport<'a>,
}

pub(super) struct SubscriptionRequest<'a> {
    pub(super) max_output_tokens: Option<usize>,
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
}

enum ReadyTransport<'a> {
    Subscription(SubscriptionRequest<'a>, SubscriptionCredentials<'a>),
    Compatible {
        body: Value,
        endpoint: &'static str,
        api_key: &'a str,
    },
}

impl<'a> CallDraft<'a> {
    pub(crate) fn new(
        selection: &'a ProviderSelection,
        input: Vec<Value>,
        tools: Vec<Value>,
        session: &'a str,
        instructions: &'a str,
    ) -> Self {
        Self {
            selection,
            input,
            tools,
            session,
            instructions,
        }
    }

    pub(crate) fn validate(self) -> Result<ValidatedCall<'a>> {
        if self.instructions.is_empty() {
            return Err(typed_failure(
                crate::model_selection::FailureKind::MalformedRequest,
                false,
                "Invalid provider instructions",
            ));
        }
        if let Some(budget) = self.selection.input_budget_bytes()? {
            #[derive(serde::Serialize)]
            struct Envelope<'a> { instructions:&'a str, input:&'a [Value], tools:&'a [Value] }
            let bytes = crate::context_accounting::json_size(&Envelope {
                instructions:self.instructions, input:&self.input, tools:&self.tools
            })?;
            if bytes > budget {
                return Err(typed_failure(crate::model_selection::FailureKind::MalformedRequest,
                    false, "Model input budget exceeded"));
            }
        }
        let request = if self.selection.provider == ProviderId::OpenaiSubscription {
            PreparedRequest::Subscription(SubscriptionRequest {
                max_output_tokens: self.selection.limits.as_ref().map(|limits| limits.output_tokens),
                model: &self.selection.model,
                input: self.input,
                tools: self.tools,
                session: self.session,
                instructions: self.instructions,
            })
        } else {
            // Reuse the adapter's exact translation and validation, once.
            let mut body = deepseek::request_body(
                &self.selection.model,
                self.input,
                self.tools,
                self.instructions,
            )
            .map_err(|_| {
                typed_failure(
                    crate::model_selection::FailureKind::MalformedRequest,
                    false,
                    "Invalid Provider request",
                )
            })?;
            if self.selection.provider != ProviderId::Deepseek {
                if let Some(messages) = body["messages"].as_array_mut() {
                    for message in messages {
                        if let Some(object) = message.as_object_mut() {
                            object.remove("reasoning_content");
                        }
                    }
                }
            }
            if let Some(limits) = &self.selection.limits {
                if self.selection.provider == ProviderId::OpenaiApi {
                    body.as_object_mut().unwrap().remove("max_tokens");
                    body["max_completion_tokens"] = json!(limits.output_tokens);
                } else { body["max_tokens"] = json!(limits.output_tokens); }
            }
            let endpoint = self
                .selection
                .provider
                .chat_endpoint()
                .ok_or_else(|| anyhow!("Provider does not use API keys"))?;
            PreparedRequest::Compatible { body, endpoint }
        };
        Ok(ValidatedCall { request })
    }
}

impl<'a> ValidatedCall<'a> {
    pub(crate) fn bind_credentials(
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
        };
        Ok(ReadyCall { transport })
    }
}

impl Provider {
    /// Consumes one locally prepared call. Live authority remains with the caller.
    pub(crate) async fn dispatch(
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_selection::FailureKind;

    fn kind(error: &anyhow::Error) -> FailureKind {
        error.downcast_ref::<ProviderFailure>().unwrap().kind
    }

    #[test]
    fn large_instructions_are_valid_when_the_complete_model_input_fits() {
        for provider in [ProviderId::OpenaiSubscription,ProviderId::Deepseek] {
            let selection=ProviderSelection{limits:None,provider,model:"fixture".into()};
            let text="x".repeat(120000);
            assert!(CallDraft::new(&selection,vec![],vec![],"s",&text).validate().is_ok());
        }
    }

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
 #[test]
 fn limits_bound_all_call_inputs_and_compatible_output() {
  let selection=ProviderSelection::parse(&json!({"provider":"deepseek","model":"fixture",
    "limits":{"input_tokens":100,"output_tokens":17}}),"unused").unwrap();
  let ready=CallDraft::new(&selection,vec![],vec![],"s","help").validate().unwrap();
  let PreparedRequest::Compatible{body,..}=ready.request else {panic!("compatible")};
  assert_eq!(body["max_tokens"],17);
  assert!(CallDraft::new(&selection,vec![json!({"role":"user","content":"x".repeat(500)})],vec![],"s","help").validate().is_err());
 }
 #[test]
 fn subscription_explicit_output_limit_is_prepared_and_defaults_omit_it() {
  let selection=ProviderSelection::parse(&json!({"provider":"openai_subscription","model":"gpt-6-luna",
    "limits":{"input_tokens":150000,"output_tokens":2000}}),"unused").unwrap();
  let ready=CallDraft::new(&selection,vec![],vec![],"s","help").validate().unwrap();
  let PreparedRequest::Subscription(request)=ready.request else {panic!("subscription")};
  assert_eq!(request.max_output_tokens,Some(2000));
 }
}
