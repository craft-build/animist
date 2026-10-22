use animist::protocol::{ProtocolError, openai::OpenAIProtocolError};
use animist::provider::{
    ContentBlock, ImageSource, Message, MessageKind, Model, ModelInfo, Provider, ProviderError,
    ProviderEvent, RequestOptions, Role, StopReason, ThinkingConfig, Tool,
    openai::{OpenAi, OpenAiConfig, Protocol},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn incompatible_reasoning_history_fails_before_transport_with_typed_errors() {
    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "keep this in history".into(),
            signature: None,
        }])
        .build()];
    let before = serde_json::to_value(&messages).unwrap();
    for protocol in [Protocol::ChatCompletions, Protocol::Responses] {
        let (tx, rx) = flume::unbounded();
        let error = provider("http://127.0.0.1:1/v1", protocol.clone())
            .stream_message(&model(), &messages, "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err();
        match (protocol, error) {
            (
                Protocol::ChatCompletions,
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::ChatReasoningReplayDisabled,
                        },
                },
            ) => {}
            (
                Protocol::Responses,
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::MissingResponsesReasoningItem,
                        },
                },
            ) => {}
            (_, error) => panic!("unexpected failure: {error:?}"),
        }
        assert!(rx.is_empty());
        assert_eq!(serde_json::to_value(&messages).unwrap(), before);
    }
}

async fn server(status: u16, body: String) -> (String, tokio::task::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let (headers, body_start, length) = loop {
            let mut buf = [0; 1024];
            let count = socket.read(&mut buf).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buf[..count]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|n| n.parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                break (headers, end + 4, length);
            }
        };
        while bytes.len() < body_start + length {
            let mut buf = [0; 1024];
            let count = socket.read(&mut buf).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buf[..count]);
        }
        let request = if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&bytes[body_start..body_start + length]).unwrap()
        };
        socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
        for chunk in body.as_bytes().chunks(7) {
            if socket.write_all(chunk).await.is_err() {
                break;
            }
            tokio::task::yield_now().await;
        }
        (headers, request)
    });
    (url, task)
}

fn provider(url: &str, protocol: Protocol) -> Provider {
    Provider::new(
        OpenAi::new(
            OpenAiConfig::builder()
                .api_key("test-key")
                .base_url(url)
                .protocol(protocol)
                .organization("org-test")
                .project("proj-test")
                .build(),
        )
        .unwrap()
        .with_client(reqwest::Client::new()),
    )
}

fn model() -> Model {
    Model::builder()
        .id("test-model")
        .provider("openai")
        .info(ModelInfo::default())
        .build()
}

#[test]
fn provider_config_round_trips_with_defaults_and_without_credentials() {
    let config: OpenAiConfig = serde_json::from_value(json!({
        "protocol": "chat_completions",
        "base_url": "http://localhost:8080/v1",
        "organization": "org-test",
        "project": "project-test"
    }))
    .unwrap();
    assert_eq!(config.protocol, Protocol::ChatCompletions);
    assert_eq!(config.api_key_env, "OPENAI_API_KEY");
    let serialized = serde_json::to_value(&config).unwrap();
    assert_eq!(serialized["protocol"], "chat_completions");
    let restored: OpenAiConfig = serde_json::from_value(serialized).unwrap();
    assert!(restored == config.clone());

    let defaults: OpenAiConfig = serde_json::from_value(json!({})).unwrap();
    assert!(defaults == OpenAiConfig::default());
    assert!(defaults == OpenAiConfig::builder().build());
    assert_eq!(defaults.base_url, "https://api.openai.com/v1");
    assert_eq!(
        serde_json::to_value(defaults.protocol).unwrap(),
        "responses"
    );

    let config = OpenAiConfig::builder().api_key("not-a-real-key").build();
    let serialized = serde_json::to_value(config).unwrap();
    assert!(serialized.get("api_key").is_none());
    assert!(serialized.get("client").is_none());
    assert!(!serialized.to_string().contains("not-a-real-key"));
}

fn sse(events: Vec<Value>) -> String {
    events
        .into_iter()
        .map(|event| format!("event: ignored\r\ndata: {event}\r\n\r\n"))
        .collect()
}

fn tool() -> Tool {
    Tool::builder()
        .name("lookup")
        .description("Search")
        .input_schema(json!({"type":"object","properties":{"q":{"type":"string"}}}))
        .build()
}

fn message() -> Message {
    Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![
            ContentBlock::Text {
                text: "Find it".into(),
            },
            ContentBlock::Image {
                source: ImageSource::from_bytes("image/png", b"png"),
            },
        ])
        .build()
}

#[tokio::test]
async fn chat_streams_interleaved_calls_and_usage() {
    let body = sse(vec![
        json!({"choices":[{"index":0,"delta":{"content":"Héllo"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":0,"id":"call-a","function":{"name":"lookup","arguments":"{\"q\":"}},
            {"index":1,"id":"call-b","function":{"name":"lookup","arguments":"{\"q\":\"b"}}
        ]}}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[
            {"index":1,"function":{"arguments":"\"}"}},
            {"index":0,"function":{"arguments":"\"a\"}"}}
        ]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":20,"prompt_tokens_details":{"cached_tokens":40}}}),
    ]) + "data: [DONE]\r\n\r\n";
    let (url, task) = server(200, body).await;
    let (tx, rx) = flume::unbounded();
    let response = provider(&url, Protocol::ChatCompletions)
        .stream_message(
            &model(),
            &[message()],
            "Be helpful",
            &[tool()],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Off)
                .fast(true)
                .max_output_tokens(123_u32)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(response.usage.input, 60);
    assert_eq!(response.usage.cache_read, 40);
    assert_eq!(response.usage.output, 20);
    assert!(
        matches!(&response.message.content[1], ContentBlock::ToolUse { id, input, .. } if id == "call-a" && input == &json!({"q":"a"}))
    );
    assert!(
        matches!(&response.message.content[2], ContentBlock::ToolUse { id, input, .. } if id == "call-b" && input == &json!({"q":"b"}))
    );
    let events: Vec<_> = rx.try_iter().collect();
    assert!(matches!(&events[0], ProviderEvent::TextDelta { text } if text == "Héllo"));
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, ProviderEvent::ToolUseStart { .. }))
            .count(),
        2
    );
    let (headers, request) = task.await.unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions "));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer test-key")
    );
    assert!(
        headers
            .to_lowercase()
            .contains("openai-organization: org-test")
    );
    assert!(headers.to_lowercase().contains("openai-project: proj-test"));
    assert_eq!(request["messages"][0]["content"], "Be helpful");
    assert_eq!(
        request["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,cG5n"
    );
    assert_eq!(
        request["tools"][0]["function"]["parameters"],
        tool().input_schema
    );
    assert_eq!(request["max_completion_tokens"], 123);
    assert_eq!(request["service_tier"], "priority");
}

#[tokio::test]
async fn responses_streams_and_replays_encrypted_reasoning() {
    let reasoning = json!({"type":"reasoning","id":"rs-1","summary":[{"type":"summary_text","text":"Plan"}],"encrypted_content":"opaque"});
    let call = json!({"type":"function_call","id":"fc-1","call_id":"call-1","name":"lookup","arguments":"{\"q\":\"rust\"}","status":"completed"});
    let body = sse(vec![
        json!({"type":"response.reasoning_summary_text.delta","delta":"Plan"}),
        json!({"type":"response.output_item.added","output_index":1,"item":call}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","delta":"{\"q\":"}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-1","delta":"\"rust\"}"}),
        json!({"type":"response.completed","response":{"status":"completed","output":[reasoning,call],"usage":{"input_tokens":100,"output_tokens":20,"input_tokens_details":{"cached_tokens":40}}}}),
    ]);
    let (url, task) = server(200, body).await;
    let (tx, rx) = flume::unbounded();
    let response = provider(&url, Protocol::Responses)
        .stream_message(
            &model(),
            &[message()],
            "Be helpful",
            &[tool()],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(response.usage.input, 60);
    assert!(
        matches!(&response.message.content[0], ContentBlock::Thinking { signature: Some(signature), .. } if serde_json::from_str::<Value>(signature).unwrap() == reasoning)
    );
    assert_eq!(rx.try_iter().count(), 4);
    let (headers, request) = task.await.unwrap();
    assert!(headers.starts_with("POST /v1/responses "));
    assert_eq!(request["store"], false);
    assert_eq!(request["include"][0], "reasoning.encrypted_content");
    assert_eq!(request["tools"][0]["strict"], false);
    assert_eq!(request["input"][0]["content"][1]["type"], "input_image");

    let (url, task) = server(200, sse(vec![json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"output_text","text":"Done"}]}],"usage":{}}})])).await;
    let result = Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::ToolResult {
            tool_use_id: "call-1".into(),
            content: "Found".into(),
            is_error: false,
            loaded_tools: vec![],
        }])
        .build();
    provider(&url, Protocol::Responses)
        .stream_message(
            &model(),
            &[response.message, result],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    let (_, request) = task.await.unwrap();
    assert_eq!(request["input"][0], reasoning);
    assert_eq!(request["input"][1]["call_id"], "call-1");
    assert_eq!(request["input"][2]["type"], "function_call_output");
}

#[tokio::test]
async fn errors_and_truncated_streams_return_errors() {
    for (protocol, status, body, expected) in [
        (
            Protocol::ChatCompletions,
            401,
            r#"{"error":{"message":"invalid key"}}"#.into(),
            None,
        ),
        (
            Protocol::ChatCompletions,
            200,
            sse(vec![
                json!({"choices":[{"index":0,"delta":{"content":"partial"}}]}),
            ]),
            Some(OpenAIProtocolError::TruncatedChatStream {
                finish_reason: None,
            }),
        ),
        (
            Protocol::Responses,
            200,
            sse(vec![
                json!({"type":"response.failed","response":{"error":{"message":"failed"}}}),
            ]),
            Some(OpenAIProtocolError::ResponseFailed {
                error: json!({"message":"failed"}),
            }),
        ),
        (
            Protocol::Responses,
            200,
            sse(vec![
                json!({"type":"response.output_text.delta","delta":"partial"}),
            ]),
            Some(OpenAIProtocolError::TruncatedResponsesStream),
        ),
    ] {
        let (url, task) = server(status, body).await;
        let (tx, rx) = flume::unbounded();
        let result = provider(&url, protocol)
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await;
        let error = result.unwrap_err();
        if let Some(expected) = expected {
            let ProviderError::Protocol {
                source: ProtocolError::OpenAI { source },
            } = error
            else {
                panic!("Expected OpenAI protocol error, got {error:?}");
            };
            assert_eq!(
                std::mem::discriminant(&source),
                std::mem::discriminant(&expected)
            );
            if let OpenAIProtocolError::ResponseFailed { error } = source {
                assert_eq!(error, json!({"message":"failed"}));
            }
        } else {
            assert!(matches!(error, ProviderError::Http { status: 401, .. }));
        }
        if status == 200 && rx.len() == 1 {
            assert!(matches!(
                rx.try_recv().unwrap(),
                ProviderEvent::TextDelta { .. }
            ));
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn list_models_uses_api_ids_without_inventing_capabilities() {
    let (url, task) = server(200, json!({"data":[{"id":"custom-model"}]}).to_string()).await;
    let models = provider(&url, Protocol::Responses)
        .list_models()
        .await
        .unwrap();
    assert_eq!(models[0].ids, ["custom-model"]);
    assert!(!models[0].features.thinking.supported);
    assert!(task.await.unwrap().0.starts_with("GET /v1/models "));
}

#[tokio::test]
async fn auth_and_unsupported_options_fail_before_network_access() {
    assert!(matches!(
        OpenAi::new(OpenAiConfig::builder().api_key("").build()),
        Err(ProviderError::EmptyApiKey)
    ));
    assert!(matches!(
        OpenAi::new(
            OpenAiConfig::builder()
                .api_key_env("ANIMIST_TEST_MISSING_KEY_59630")
                .build()
        ),
        Err(ProviderError::Credentials { .. })
    ));
    let provider = provider("http://127.0.0.1:1", Protocol::Responses);
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    let (tx, _) = flume::unbounded();
    let result = provider
        .stream_message(
            &model(),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Budget(10))
                .fast(false)
                .build(),
        )
        .await;
    assert!(matches!(
        result,
        Err(ProviderError::ReasoningBudgetUnsupported)
    ));
}

#[tokio::test]
async fn both_protocols_report_output_limits() {
    for (protocol, body) in [
        (
            Protocol::ChatCompletions,
            sse(vec![
                json!({"choices":[{"index":0,"delta":{"content":"partial"},"finish_reason":"length"}]}),
            ]) + "data: [DONE]\n\n",
        ),
        (
            Protocol::Responses,
            sse(vec![
                json!({"type":"response.incomplete","response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","content":[{"type":"output_text","text":"partial"}]}]}}),
            ]),
        ),
    ] {
        let (url, task) = server(200, body).await;
        let (tx, _rx) = flume::unbounded();
        let response = provider(&url, protocol)
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::MaxTokens));
        assert_eq!(
            response.message.content,
            vec![ContentBlock::Text {
                text: "partial".into()
            }]
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn chat_replays_tool_results_before_followup_text() {
    let (url, task) = server(
        200,
        sse(vec![
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ]) + "data: [DONE]\n\n",
    )
    .await;
    let previous = Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::ToolUse {
            id: "call-1".into(),
            name: "lookup".into(),
            input: json!({"q":"rust"}),
            thought_signature: None,
        }])
        .build();
    let result = Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![
            ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            },
            ContentBlock::Text {
                text: "Summarize it".into(),
            },
        ])
        .build();
    let (tx, _rx) = flume::unbounded();
    provider(&url, Protocol::ChatCompletions)
        .stream_message(
            &model(),
            &[previous, result],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    let (_, request) = task.await.unwrap();
    assert_eq!(request["messages"][0]["tool_calls"][0]["id"], "call-1");
    assert!(request["messages"][0]["content"].is_null());
    assert_eq!(request["messages"][1]["role"], "tool");
    assert_eq!(request["messages"][2]["content"][0]["text"], "Summarize it");
}

#[tokio::test]
async fn reasoning_policy_is_explicit_and_respects_overrides() {
    use animist::provider::{Effort, ThinkingSupport};
    for protocol in [Protocol::ChatCompletions, Protocol::Responses] {
        let body = match protocol {
            Protocol::ChatCompletions => {
                sse(vec![
                    json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                ]) + "data: [DONE]\n\n"
            }
            Protocol::Responses => sse(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[]}}),
            ]),
        };
        let (url, task) = server(200, body).await;
        let provider = provider(&url, protocol.clone());
        let mut model = model();
        model.info.features.thinking.supported = true;
        model.info.features.thinking.efforts = vec![Effort::High];
        model.overrides.thinking = Some(ThinkingSupport::Required);
        let (tx, _rx) = flume::unbounded();
        assert!(matches!(
            provider
                .stream_message(&model, &[], "", &[], &tx, RequestOptions::default())
                .await,
            Err(ProviderError::ReasoningRequired)
        ));
        provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Effort(Effort::High))
                    .fast(false)
                    .build(),
            )
            .await
            .unwrap();
        let (_, request) = task.await.unwrap();
        match protocol {
            Protocol::ChatCompletions => assert_eq!(request["reasoning_effort"], "high"),
            Protocol::Responses => assert_eq!(
                request["reasoning"],
                json!({"effort":"high","summary":"auto"})
            ),
        }
    }
}

#[tokio::test]
async fn malformed_tool_json_and_refusals_are_not_successes() {
    for (protocol, body, tool_call_id) in [
        (
            Protocol::ChatCompletions,
            sse(vec![
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c","function":{"name":"f","arguments":"{"}}]},"finish_reason":"tool_calls"}]}),
            ]) + "data: [DONE]\n\n",
            Some("c"),
        ),
        (
            Protocol::ChatCompletions,
            sse(vec![
                json!({"choices":[{"index":0,"delta":{"refusal":"No"},"finish_reason":"stop"}]}),
            ]),
            None,
        ),
        (
            Protocol::Responses,
            sse(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"function_call","call_id":"c","name":"f","arguments":"{"}]}}),
            ]),
            Some("c"),
        ),
        (
            Protocol::Responses,
            sse(vec![
                json!({"type":"response.completed","response":{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"No"}]}]}}),
            ]),
            None,
        ),
    ] {
        let (url, task) = server(200, body).await;
        let (tx, _rx) = flume::unbounded();
        let error = provider(&url, protocol)
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err();
        let ProviderError::Protocol {
            source: ProtocolError::OpenAI { source },
        } = error
        else {
            panic!("Expected OpenAI protocol error, got {error:?}");
        };
        match (tool_call_id, source) {
            (Some(expected), OpenAIProtocolError::DecodeToolArguments { id, source }) => {
                assert_eq!(id, expected);
                assert!(source.is_eof());
            }
            (None, OpenAIProtocolError::Refusal) => {}
            (_, source) => panic!("Unexpected OpenAI error: {source:?}"),
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn dropping_stream_future_closes_the_exchange() {
    use std::time::Duration;
    for protocol in [Protocol::ChatCompletions, Protocol::Responses] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let event = match protocol {
            Protocol::ChatCompletions => {
                json!({"choices":[{"index":0,"delta":{"content":"started"}}]})
            }
            Protocol::Responses => json!({"type":"response.output_text.delta","delta":"started"}),
        };
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut buf = [0; 1024];
                let count = socket.read(&mut buf).await.unwrap();
                assert_ne!(count, 0);
                request.extend_from_slice(&buf[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length: usize = headers
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
            let data = sse(vec![event]);
            socket
                .write_all(format!("{:x}\r\n{data}\r\n", data.len()).as_bytes())
                .await
                .unwrap();
            let mut buf = [0; 1];
            let closed = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut buf))
                .await
                .unwrap();
            assert!(matches!(closed, Ok(0) | Err(_)));
        });
        let provider = provider(&url, protocol);
        let model = model();
        let (tx, rx) = flume::bounded(1);
        {
            let stream =
                provider.stream_message(&model, &[], "", &[], &tx, RequestOptions::default());
            tokio::pin!(stream);
            tokio::select! {
                event = rx.recv_async() => assert!(matches!(event.unwrap(), ProviderEvent::TextDelta { .. })),
                result = &mut stream => panic!("Stream finished unexpectedly: {result:?}"),
                _ = tokio::time::sleep(Duration::from_secs(3)) => panic!("No stream event"),
            }
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn sse_accepts_bom_comments_and_multiline_data() {
    let body = "\u{feff}: heartbeat\r\n\r\ndata: {\"choices\":\r\ndata: [{\"index\":0,\"delta\":{\"content\":\"Héllo\"},\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\r\n\r\n";
    let (url, task) = server(200, body.into()).await;
    let (tx, _rx) = flume::unbounded();
    let response = provider(&url, Protocol::ChatCompletions)
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        response.message.content,
        vec![ContentBlock::Text {
            text: "Héllo".into()
        }]
    );
    task.await.unwrap();
}

#[tokio::test]
async fn responses_keeps_interleaved_tool_identity_and_output_order() {
    let a = json!({"type":"function_call","id":"fc-a","call_id":"call-a","name":"lookup","arguments":"{\"q\":\"a\"}"});
    let b = json!({"type":"function_call","id":"fc-b","call_id":"call-b","name":"lookup","arguments":"{\"q\":\"b\"}"});
    let body = sse(vec![
        json!({"type":"response.output_item.added","item":a}),
        json!({"type":"response.output_item.added","item":b}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-a","delta":"{\"q\":"}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-b","delta":"{\"q\":\"b\"}"}),
        json!({"type":"response.function_call_arguments.delta","item_id":"fc-a","delta":"\"a\"}"}),
        json!({"type":"response.completed","response":{"status":"completed","output":[a,b]}}),
    ]);
    let (url, task) = server(200, body).await;
    let (tx, rx) = flume::unbounded();
    let previous = Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Text {
            text: "Earlier".into(),
        }])
        .build();
    let response = provider(&url, Protocol::Responses)
        .stream_message(
            &model(),
            &[previous],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert!(
        matches!(&response.message.content[0], ContentBlock::ToolUse { id, input, .. } if id == "call-a" && input == &json!({"q":"a"}))
    );
    assert!(
        matches!(&response.message.content[1], ContentBlock::ToolUse { id, input, .. } if id == "call-b" && input == &json!({"q":"b"}))
    );
    let events: Vec<_> = rx.try_iter().collect();
    assert!(
        matches!(&events[3], ProviderEvent::ToolInputDelta { id, partial_json } if id == "call-b" && partial_json == "{\"q\":\"b\"}")
    );
    let (_, request) = task.await.unwrap();
    assert_eq!(request["input"][0]["role"], "assistant");
    assert_eq!(request["input"][0]["content"][0]["type"], "input_text");
}
