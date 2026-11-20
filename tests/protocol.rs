#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

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
    assert_eq!(error.status(), None);
    assert!(error.malformed_tool_arguments().is_none());
}

#[test]
fn provider_status_covers_transport_and_in_stream_api_failures() {
    use animist::{
        protocol::{ProtocolError, anthropic::AnthropicProtocolError, openai::OpenAIProtocolError},
        provider::ProviderError,
    };

    assert_eq!(
        ProviderError::Http {
            status: 429,
            body: "busy".into(),
            body_truncated: false,
            backtrace: std::backtrace::Backtrace::capture(),
        }
        .status(),
        Some(429)
    );
    let status_of = |source: OpenAIProtocolError| {
        let error: ProviderError = ProtocolError::OpenAI { source }.into();
        error.status()
    };
    assert_eq!(
        status_of(OpenAIProtocolError::Api {
            status: 503,
            error: json!({"message": "busy"}),
            backtrace: std::backtrace::Backtrace::capture(),
        }),
        Some(503)
    );
    assert_eq!(
        status_of(OpenAIProtocolError::ResponseFailed {
            status: 500,
            error: json!({"code": "server_error"}),
            backtrace: std::backtrace::Backtrace::capture(),
        }),
        Some(500)
    );
    assert_eq!(
        status_of(OpenAIProtocolError::TruncatedChatStream {
            finish_reason: None,
            backtrace: std::backtrace::Backtrace::capture(),
        }),
        None
    );
    let anthropic: ProviderError = ProtocolError::Anthropic {
        source: AnthropicProtocolError::Api {
            status: 529,
            error: json!({"type": "overloaded_error"}),
            backtrace: std::backtrace::Backtrace::capture(),
        },
    }
    .into();
    assert_eq!(anthropic.status(), Some(529));
    assert_eq!(
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::DecodeEvent {
                    source: serde_json::from_str::<Value>("{").unwrap_err(),
                },
            },
        }
        .status(),
        None
    );
}

#[test]
fn malformed_tool_arguments_classify_across_protocol_sources() {
    use animist::{
        protocol::{ProtocolError, anthropic::AnthropicProtocolError, openai::OpenAIProtocolError},
        provider::{MalformedToolArguments, ProviderError},
    };

    let decode =
        |source: OpenAIProtocolError| -> ProviderError { ProtocolError::OpenAI { source }.into() };
    let openai_error = decode(OpenAIProtocolError::DecodeToolArguments {
        id: "call-1".into(),
        source: serde_json::from_str::<Value>("{").unwrap_err(),
        arguments: "{\"text\":}".into(),
    });
    let openai: MalformedToolArguments<'_> = openai_error
        .malformed_tool_arguments()
        .expect("decode failures classify");
    assert_eq!(openai.id, "call-1");
    assert_eq!(openai.arguments, "{\"text\":}");
    assert!(openai.reason.contains("EOF while parsing an object"));
    assert_eq!(
        decode(OpenAIProtocolError::InvalidToolInput {
            id: "call-2".into(),
            arguments: "[1]".into(),
            backtrace: std::backtrace::Backtrace::capture(),
        })
        .malformed_tool_arguments()
        .expect("non-object failures classify")
        .reason,
        "not a JSON object"
    );
    let anthropic: ProviderError = ProtocolError::Anthropic {
        source: AnthropicProtocolError::DecodeToolArguments {
            id: "call-3".into(),
            source: serde_json::from_str::<Value>("{").unwrap_err(),
            arguments: "{\"x\":}".into(),
        },
    }
    .into();
    let view = anthropic
        .malformed_tool_arguments()
        .expect("anthropic classifies");
    assert_eq!(view.id, "call-3");
    assert_eq!(view.arguments, "{\"x\":}");
    #[cfg(feature = "bedrock")]
    {
        let bedrock: ProviderError = ProtocolError::Bedrock {
            source: animist::protocol::bedrock::BedrockProtocolError::InvalidToolInput {
                id: "call-4".into(),
                arguments: "[1]".into(),
                backtrace: std::backtrace::Backtrace::capture(),
            },
        }
        .into();
        let view = bedrock
            .malformed_tool_arguments()
            .expect("bedrock classifies");
        assert_eq!(view.id, "call-4");
        let bedrock: ProviderError = ProtocolError::Bedrock {
            source: animist::protocol::bedrock::BedrockProtocolError::DecodeToolInput {
                id: "call-5".into(),
                source: serde_json::from_str::<Value>("{").unwrap_err(),
                arguments: "{\"x\":".into(),
            },
        }
        .into();
        let view = bedrock
            .malformed_tool_arguments()
            .expect("bedrock decode classifies");
        assert_eq!(view.arguments, "{\"x\":");
        assert!(view.reason.contains("EOF while parsing"));
    }
    for none in [
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::Refusal {
                    backtrace: std::backtrace::Backtrace::capture(),
                },
            },
        },
        ProviderError::Protocol {
            source: ProtocolError::Anthropic {
                source: AnthropicProtocolError::TruncatedStream {
                    backtrace: std::backtrace::Backtrace::capture(),
                },
            },
        },
    ] {
        assert!(none.malformed_tool_arguments().is_none());
    }
}

#[tokio::test]
async fn in_stream_api_errors_carry_the_response_status() {
    use animist::provider::ProviderError;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert_ne!(socket.read(&mut request).await.unwrap(), 0);
        let body = "data: {\"error\":{\"message\":\"busy\"}}\n\n";
        socket
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let response = reqwest::get(format!("http://{address}")).await.unwrap();
    let (tx, _rx) = flume::unbounded();
    let error = ChatCompletions.assemble(response, &tx).await.unwrap_err();
    assert!(matches!(
        &error,
        ProtocolError::OpenAI {
            source: OpenAIProtocolError::Api { status: 503, .. }
        }
    ));
    let provider_error: ProviderError = error.into();
    assert_eq!(provider_error.status(), Some(503));
    server.await.unwrap();
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
                        name: None,
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
                source: OpenAIProtocolError::ChatReasoningReplayDisabled { .. }
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
                    source: OpenAIProtocolError::MissingResponsesReasoningItem { .. }
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
                name: None,
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
                name: None,
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
                name: None,
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
                source: OpenAIProtocolError::UnsupportedChatContent { .. }
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
            source: OpenAIProtocolError::UnsupportedChatContent { .. }
        })
    ));
    assert!(matches!(
        Responses.request(input, &OpenAiOptions::default()),
        Err(ProtocolError::OpenAI {
            source: OpenAIProtocolError::UnsupportedResponsesContent { .. }
        })
    ));
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}
