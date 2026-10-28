use animist::{
    history::{ContentBlock, Message, MessageKind, Role},
    protocol::{
        Protocol, ProtocolError, ProtocolInput, Result,
        openai::{ChatCompletions, OpenAIProtocolError, OpenAiOptions, Responses},
    },
    provider::{
        Model, ModelInfo, Provider, ProviderEvent, RequestOptions, StopReason, StreamResponse,
        ThinkingConfig, TokenUsage,
        ollama::{Ollama, OllamaConfig},
    },
};
use async_trait::async_trait;
use flume::Sender;
use serde_json::{Value, json};

#[test]
fn protocol_errors_remain_typed_through_the_provider_boundary() {
    use animist::{
        protocol::{ProtocolError, openai::OpenAIProtocolError},
        provider::ProviderError,
    };
    use std::error::Error;

    let source = serde_json::from_str::<Value>("{").unwrap_err();
    let protocol_error = OpenAIProtocolError::DecodeEvent { source };
    let display = protocol_error.to_string();
    let error: ProtocolError = protocol_error.into();
    assert_eq!(error.to_string(), display);
    let error: ProviderError = error.into();
    assert_eq!(error.to_string(), display);
    assert!(error.source().unwrap().is::<serde_json::Error>());
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::DecodeEvent { .. }
            }
        }
    ));
}

struct PlainText;

#[async_trait]
impl Protocol for PlainText {
    type Request = String;
    type Response = String;
    type Options = ();

    fn request(&self, input: ProtocolInput<'_>, _options: &()) -> Result<String> {
        Ok(input.system.to_owned())
    }

    async fn assemble(
        &self,
        response: String,
        tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse> {
        tx.send_async(ProviderEvent::TextDelta {
            text: response.clone(),
        })
        .await
        .unwrap();
        Ok(StreamResponse::builder()
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .kind(MessageKind::Turn)
                    .content(vec![ContentBlock::Text { text: response }])
                    .build(),
            )
            .usage(TokenUsage::default())
            .stop_reason(StopReason::EndTurn)
            .input_transformations(Vec::new())
            .build())
    }
}

#[tokio::test]
async fn downstream_protocol_supports_non_http_transport_and_dynamic_dispatch() {
    let protocol: &dyn Protocol<Request = String, Response = String, Options = ()> = &PlainText;
    let model = Model::builder()
        .id("protocol-test")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(&[][..])
        .system("Hello")
        .tools(&[][..])
        .options(&options)
        .build();
    let request = protocol.request(input, &()).unwrap();
    let (tx, rx) = flume::unbounded();
    let response = protocol.assemble(request, &tx).await.unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        rx.try_recv().unwrap(),
        ProviderEvent::TextDelta {
            text: "Hello".into()
        }
    );
}

#[test]
fn openai_adapters_share_the_contract_without_sharing_wire_shapes() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ChatCompletions>();
    assert_send_sync::<Responses>();

    let model = Model::builder()
        .id("compatible-model")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(&[][..])
        .system("Instructions")
        .tools(&[][..])
        .options(&options)
        .build();
    let wire_options = OpenAiOptions::builder().reasoning_effort("high").build();
    let adapters: [&dyn Protocol<Request = Value, Response = reqwest::Response, Options = OpenAiOptions>;
        2] = [&ChatCompletions, &Responses];
    let chat = adapters[0].request(input, &wire_options).unwrap();
    let responses = adapters[1].request(input, &wire_options).unwrap();
    assert_eq!(chat["messages"][0]["content"], "Instructions");
    assert_eq!(chat["reasoning_effort"], "high");
    assert!(chat.get("input").is_none());
    assert_eq!(responses["instructions"], "Instructions");
    assert_eq!(responses["reasoning"]["effort"], "high");
    assert!(responses.get("messages").is_none());
}

#[test]
fn unsigned_reasoning_is_rejected_instead_of_silently_omitted() {
    for trailing in [
        vec![],
        vec![ContentBlock::Text {
            text: "answer".into(),
        }],
        vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: "lookup".into(),
            input: json!({}),
            thought_signature: None,
        }],
    ] {
        let mut content = vec![ContentBlock::Thinking {
            thinking: "plan".into(),
            signature: None,
        }];
        content.extend(trailing);
        let mut messages = vec![
            Message::builder()
                .role(Role::Assistant)
                .kind(MessageKind::Turn)
                .content(content)
                .build(),
        ];
        if messages[0]
            .content
            .iter()
            .any(|block| matches!(block, ContentBlock::ToolUse { .. }))
        {
            messages.push(
                Message::builder()
                    .role(Role::User)
                    .kind(MessageKind::Turn)
                    .content(vec![ContentBlock::ToolResult {
                        tool_use_id: "call-1".into(),
                        content: "Found".into(),
                        is_error: false,
                        loaded_tools: vec![],
                    }])
                    .build(),
            );
        }
        let before = serde_json::to_value(&messages).unwrap();
        let model = Model::builder()
            .id("protocol-test")
            .provider(Provider::new(Ollama::new(OllamaConfig::default())))
            .info(ModelInfo::default())
            .build();
        let options = RequestOptions::default();
        let input = ProtocolInput::builder()
            .model(&model)
            .messages(messages.as_slice())
            .system("")
            .tools([].as_slice())
            .options(&options)
            .build();
        assert!(matches!(
            ChatCompletions.request(input, &OpenAiOptions::default()),
            Err(ProtocolError::OpenAI {
                source: OpenAIProtocolError::ChatReasoningReplayDisabled
            })
        ));
        for wire_options in [
            OpenAiOptions::default(),
            OpenAiOptions::builder()
                .replay_reasoning_content(true)
                .build(),
        ] {
            assert!(matches!(
                Responses.request(input, &wire_options),
                Err(ProtocolError::OpenAI {
                    source: OpenAIProtocolError::MissingResponsesReasoningItem
                })
            ));
        }
        assert_eq!(serde_json::to_value(&messages).unwrap(), before);
    }
}

#[test]
fn chat_reasoning_only_and_tool_loop_history_survive_when_new_thinking_is_off() {
    let messages = vec![
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![
                ContentBlock::Thinking {
                    thinking: "  first\n".into(),
                    signature: None,
                },
                ContentBlock::Thinking {
                    thinking: "then 🦀  ".into(),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    input: json!({"path":"src"}),
                    thought_signature: None,
                },
            ])
            .build(),
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            }])
            .build(),
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Thinking {
                thinking: "reasoning-only".into(),
                signature: None,
            }])
            .build(),
    ];
    let before = serde_json::to_value(&messages).unwrap();
    let model = Model::builder()
        .id("protocol-test")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    for thinking in [ThinkingConfig::Off, ThinkingConfig::Adaptive] {
        let options = RequestOptions::builder()
            .thinking(thinking)
            .fast(false)
            .build();
        let input = ProtocolInput::builder()
            .model(&model)
            .messages(messages.as_slice())
            .system("")
            .tools([].as_slice())
            .options(&options)
            .build();
        let body = ChatCompletions
            .request(
                input,
                &OpenAiOptions::builder()
                    .replay_reasoning_content(true)
                    .build(),
            )
            .unwrap();
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(
            body["messages"][0]["reasoning_content"],
            "  first\nthen 🦀  "
        );
        assert_eq!(body["messages"][0]["tool_calls"][0]["id"], "call-1");
        assert_eq!(
            body["messages"][1],
            json!({"role":"tool","tool_call_id":"call-1","content":"Found"})
        );
        assert_eq!(body["messages"][2]["reasoning_content"], "reasoning-only");
        assert_eq!(body["messages"][2]["role"], "assistant");
    }
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}

#[test]
fn default_chat_dialect_preserves_existing_assistant_wire_shapes() {
    use animist::protocol::openai::ChatReasoningField;

    assert_eq!(
        OpenAiOptions::default().chat_reasoning_field,
        ChatReasoningField::ReasoningContent
    );
    assert_eq!(OpenAiOptions::builder().build(), OpenAiOptions::default());
    let mut messages = vec![
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![
                ContentBlock::Text {
                    text: "first".into(),
                },
                ContentBlock::Text {
                    text: "second".into(),
                },
            ])
            .build(),
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "lookup".into(),
                input: json!({"path":"src"}),
                thought_signature: None,
            }])
            .build(),
    ];
    let mut expected = vec![
        json!({"role":"assistant","content":[
            {"type":"text","text":"first"},{"type":"text","text":"second"}
        ]}),
        json!({"role":"assistant","content":null,"tool_calls":[{
            "type":"function","id":"call-1",
            "function":{"name":"lookup","arguments":"{\"path\":\"src\"}"}
        }]}),
    ];
    messages.push(
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            }])
            .build(),
    );
    expected.push(json!({"role":"tool","tool_call_id":"call-1","content":"Found"}));
    let model = Model::builder()
        .id("protocol-test")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    let options = RequestOptions::default();
    for replay in [false, true] {
        if replay {
            messages.push(
                Message::builder()
                    .role(Role::Assistant)
                    .kind(MessageKind::Turn)
                    .content(vec![
                        ContentBlock::Thinking {
                            thinking: "plan".into(),
                            signature: None,
                        },
                        ContentBlock::Text {
                            text: "answer".into(),
                        },
                    ])
                    .build(),
            );
            expected.push(json!({
                "role":"assistant","content":[{"type":"text","text":"answer"}],
                "reasoning_content":"plan"
            }));
            messages.push(
                Message::builder()
                    .role(Role::Assistant)
                    .kind(MessageKind::Turn)
                    .content(vec![ContentBlock::Thinking {
                        thinking: "reasoning-only".into(),
                        signature: None,
                    }])
                    .build(),
            );
            expected.push(json!({
                "role":"assistant","content":null,"reasoning_content":"reasoning-only"
            }));
        }
        let input = ProtocolInput::builder()
            .model(&model)
            .messages(messages.as_slice())
            .system("")
            .tools([].as_slice())
            .options(&options)
            .build();
        let body = ChatCompletions
            .request(
                input,
                &OpenAiOptions::builder()
                    .replay_reasoning_content(replay)
                    .build(),
            )
            .unwrap();
        assert_eq!(body["messages"], json!(expected));
        for field in [
            "reasoning_budget_tokens",
            "enable_thinking",
            "thinking_budget",
        ] {
            assert!(body.get(field).is_none(), "unexpected field: {field}");
        }
    }
}

#[test]
fn responses_replays_complete_opaque_items_and_rejects_cross_protocol_conversion() {
    let item = json!({
        "type":"reasoning", "id":"rs-1", "encrypted_content":"opaque-not-plain-text",
        "summary":[{"type":"summary_text","text":"summary","future_field":"preserved"}],
        "future_metadata":{"preserve":true}
    });
    let messages = vec![
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![
                ContentBlock::Thinking {
                    thinking: "summary".into(),
                    signature: Some(item.to_string()),
                },
                ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    input: json!({}),
                    thought_signature: None,
                },
            ])
            .build(),
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            }])
            .build(),
    ];
    let model = Model::builder()
        .id("protocol-test")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    let before = serde_json::to_value(&messages).unwrap();
    for thinking in [ThinkingConfig::Off, ThinkingConfig::Adaptive] {
        let options = RequestOptions::builder()
            .thinking(thinking)
            .fast(false)
            .build();
        let input = ProtocolInput::builder()
            .model(&model)
            .messages(messages.as_slice())
            .system("")
            .tools([].as_slice())
            .options(&options)
            .build();
        let body = Responses.request(input, &OpenAiOptions::default()).unwrap();
        assert_eq!(body["input"][0], item);
        assert_eq!(body["input"][1]["call_id"], "call-1");
        assert_eq!(body["input"][2]["type"], "function_call_output");
        assert!(matches!(
            ChatCompletions.request(
                input,
                &OpenAiOptions::builder()
                    .replay_reasoning_content(true)
                    .build()
            ),
            Err(ProtocolError::OpenAI {
                source: OpenAIProtocolError::UnsupportedChatContent
            })
        ));
    }
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}

#[test]
fn redacted_reasoning_is_not_discarded_or_converted_to_text() {
    use animist::protocol::anthropic::{AnthropicOptions, Messages};

    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::RedactedThinking {
            data: "opaque-redacted-data".into(),
        }])
        .build()];
    let before = serde_json::to_value(&messages).unwrap();
    let model = Model::builder()
        .id("protocol-test")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(messages.as_slice())
        .system("")
        .tools([].as_slice())
        .options(&options)
        .build();
    let body = Messages
        .request(input, &AnthropicOptions::default())
        .unwrap();
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({
            "type":"redacted_thinking","data":"opaque-redacted-data"
        })
    );
    assert!(matches!(
        ChatCompletions.request(
            input,
            &OpenAiOptions::builder()
                .replay_reasoning_content(true)
                .build()
        ),
        Err(ProtocolError::OpenAI {
            source: OpenAIProtocolError::UnsupportedChatContent
        })
    ));
    assert!(matches!(
        Responses.request(input, &OpenAiOptions::default()),
        Err(ProtocolError::OpenAI {
            source: OpenAIProtocolError::UnsupportedResponsesContent
        })
    ));
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}
