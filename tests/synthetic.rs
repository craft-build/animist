use std::time::Duration;

use animist::provider::{
    ContentBlock, Message, MessageKind, Model, ModelInfo, Provider, ProviderError, ProviderImpl,
    RequestOptions, Role, StopReason, ThinkingConfig, Tool,
    synthetic::{Synthetic, SyntheticConfig},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};

const FIXTURE: &str = include_str!("fixtures/synthetic.sse");

#[tokio::test]
async fn final_done_event_does_not_require_a_trailing_blank_line() {
    use animist::provider::Effort;

    for ending in [
        "data: [DONE]",
        "data: [DONE]\n",
        "data: [DONE]\r\n",
        "data: [DONE]\r",
    ] {
        let body = format!(
            "data: {}\n\n{ending}",
            json!({
                "choices":[{"index":0,"delta":{"content":"Two sentences. Complete."},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":12,"completion_tokens":8}
            })
        );
        let (url, task) = server("200 OK", "text/event-stream", &body).await;
        let provider = Synthetic::new(
            SyntheticConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap();
        let mut model = model();
        model.id = "syn:small:text".into();
        model.info.features.thinking.supported = true;
        model.info.features.thinking.efforts = vec![Effort::Low];
        let (tx, rx) = flume::unbounded();
        let response = provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Effort(Effort::Low))
                    .fast(false)
                    .max_output_tokens(4096_u32)
                    .build(),
            )
            .await
            .unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(response.usage.input, 12);
        assert_eq!(response.usage.output, 8);
        assert_eq!(
            response.message.content,
            vec![ContentBlock::Text {
                text: "Two sentences. Complete.".into()
            }]
        );
        assert_eq!(rx.len(), 1);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn eof_still_requires_done_and_a_finish_reason() {
    use animist::protocol::{ProtocolError, openai::OpenAIProtocolError};

    for (finish, done) in [(None, false), (Some("stop"), false), (None, true)] {
        let mut body = format!(
            "data: {}",
            json!({
                "choices":[{"index":0,"delta":{"content":"Complete-looking text."},"finish_reason":finish}]
            })
        );
        if done {
            body.push_str("\n\ndata: [DONE]");
        }
        let (url, task) = server("200 OK", "text/event-stream", &body).await;
        let provider = Synthetic::new(
            SyntheticConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap();
        let (tx, _rx) = flume::unbounded();
        let error = provider
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err();
        match error {
            ProviderError::Protocol {
                source:
                    ProtocolError::OpenAI {
                        source: OpenAIProtocolError::TruncatedChatStream { finish_reason },
                    },
            } if !done => assert_eq!(finish_reason, finish.map(|_| StopReason::EndTurn)),
            ProviderError::Protocol {
                source:
                    ProtocolError::OpenAI {
                        source: OpenAIProtocolError::MissingFinishReason,
                    },
            } if done => {}
            error => panic!("unexpected completion error: {error:?}"),
        }
        task.await.unwrap();
    }
}

#[test]
fn config_defaults_and_credential_serialization_match_other_providers() {
    let defaults: SyntheticConfig = serde_json::from_value(json!({})).unwrap();
    assert!(defaults == SyntheticConfig::default());
    assert!(defaults == SyntheticConfig::builder().build());
    assert_eq!(defaults.base_url, "https://api.synthetic.new/openai/v1");
    assert_eq!(defaults.api_key_env, "SYNTHETIC_API_KEY");
    assert_eq!(defaults.max_output_tokens, 8192);
    let config = SyntheticConfig::builder().api_key("not-a-real-key").build();
    let value = serde_json::to_value(config).unwrap();
    assert!(value.get("api_key").is_none());
    assert!(value.get("client").is_none());
    assert!(!value.to_string().contains("not-a-real-key"));
    let restored: SyntheticConfig = serde_json::from_value(value).unwrap();
    assert!(restored == defaults);
}

#[test]
fn shared_chat_adapter_replays_reasoning_only_when_enabled() {
    use animist::protocol::{
        Protocol, ProtocolInput,
        openai::{ChatCompletions, OpenAiOptions},
    };

    let message = Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![
            ContentBlock::Thinking {
                thinking: "plan".into(),
                signature: None,
            },
            ContentBlock::ToolUse {
                id: "a".into(),
                name: "lookup".into(),
                input: json!({"q":"first"}),
                thought_signature: None,
            },
        ])
        .build();
    let messages = [message];
    let model = model();
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
        Err(animist::protocol::ProtocolError::OpenAI {
            source: animist::protocol::openai::OpenAIProtocolError::ChatReasoningReplayDisabled,
        })
    ));
    let body = ChatCompletions
        .request(
            input,
            &OpenAiOptions::builder()
                .replay_reasoning_content(true)
                .build(),
        )
        .unwrap();
    assert_eq!(body["messages"][0]["reasoning_content"], "plan");
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["arguments"],
        r#"{"q":"first"}"#
    );
}

#[tokio::test]
async fn protocol_errors_keep_the_existing_layered_types_and_payloads() {
    use animist::protocol::{ProtocolError, openai::OpenAIProtocolError};

    for (body, kind) in [
        (
            "data: {\"error\":{\"code\":\"test-error\"}}\n\n".to_owned(),
            "api",
        ),
        (FIXTURE.replace("\\\"first\\\"}", "invalid}"), "arguments"),
        (
            FIXTURE.split("data: [DONE]").next().unwrap().to_owned(),
            "truncated",
        ),
    ] {
        let (url, task) = server("200 OK", "text/event-stream", &body).await;
        let provider = Synthetic::new(
            SyntheticConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap();
        let (tx, _rx) = flume::unbounded();
        let error = provider
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err();
        match (kind, error) {
            (
                "api",
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::Api { error },
                        },
                },
            ) => assert_eq!(error["code"], "test-error"),
            (
                "arguments",
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::DecodeToolArguments { id, .. },
                        },
                },
            ) => assert_eq!(id, "a"),
            (
                "truncated",
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::TruncatedChatStream { .. },
                        },
                },
            ) => {}
            (_, error) => panic!("unexpected {kind} error: {error:?}"),
        }
        task.await.unwrap();
    }
}

async fn read_request(socket: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            if bytes.len() >= end + 4 + length {
                let body = if length == 0 {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()
                };
                return (headers.to_lowercase(), body);
            }
        }
    }
}

async fn server(
    status: &str,
    content_type: &str,
    body: &str,
) -> (String, JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/openai/v1", listener.local_addr().unwrap());
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            socket.write_all(response.as_bytes()).await.unwrap();
            request
        })
        .await
        .unwrap()
    });
    (url, task)
}

fn model() -> Model {
    Model::builder()
        .id("syn:large:text")
        .provider("synthetic")
        .info(ModelInfo::default())
        .build()
}

#[tokio::test]
async fn downstream_provider_streams_and_sends_only_wire_fields() {
    let (url, task) = server("200 OK", "text/event-stream", FIXTURE).await;
    let provider = Provider::new(
        Synthetic::new(
            SyntheticConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap(),
    );
    assert_eq!(provider.name(), "synthetic");
    let (tx, rx) = flume::unbounded();
    let message = Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Text {
            text: "hello".into(),
        }])
        .build();
    let tool = Tool::builder()
        .name("lookup")
        .description("Look up")
        .input_schema(json!({"type":"object","properties":{"q":{"type":"string"}}}))
        .build();
    let response = provider
        .stream_message(
            &model(),
            &[message],
            "system",
            &[tool],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Off)
                .fast(false)
                .max_output_tokens(128_u32)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(rx.len(), 9);
    assert_eq!(response.usage.input, 80);
    assert_eq!(response.usage.cache_read, 20);
    assert!(
        matches!(&response.message.content[0], ContentBlock::Thinking {
        thinking, signature: None,
    } if thinking == "Consider 🦀")
    );
    for (index, id, value) in [(2, "a", "first"), (3, "b", "second")] {
        assert!(
            matches!(&response.message.content[index], ContentBlock::ToolUse {
            id: actual, name, input, ..
        } if actual == id && name == "lookup" && input == &json!({"q":value}))
        );
    }
    let events: Vec<_> = rx.drain().collect();
    for id in ["a", "b"] {
        let start = events
            .iter()
            .position(|event| {
                matches!(event,
            animist::provider::ProviderEvent::ToolUseStart { id: actual, name }
                if actual == id && name == "lookup")
            })
            .unwrap();
        let input = events
            .iter()
            .position(|event| {
                matches!(event,
            animist::provider::ProviderEvent::ToolInputDelta { id: actual, .. }
                if actual == id)
            })
            .unwrap();
        assert!(start < input);
    }
    let (headers, body) = task.await.unwrap();
    assert!(headers.starts_with("post /openai/v1/chat/completions "));
    assert!(headers.contains("authorization: bearer test-key"));
    assert!(!headers.contains("anthropic-version"));
    assert_eq!(body["model"], "syn:large:text");
    assert_eq!(body["max_completion_tokens"], 128);
    assert_eq!(
        body["messages"][0],
        json!({"role":"system","content":"system"})
    );
    assert_eq!(body["stream"], true);
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert_eq!(body["tools"][0]["function"]["name"], "lookup");
    assert_eq!(
        body["messages"][1],
        json!({"role":"user","content":[{"type":"text","text":"hello"}]})
    );
}

#[tokio::test]
async fn sends_reasoning_effort_and_preserves_server_default_for_adaptive() {
    use animist::provider::Effort;

    for (thinking, expected) in [
        (ThinkingConfig::Effort(Effort::High), Some("high")),
        (ThinkingConfig::Off, Some("none")),
        (ThinkingConfig::Adaptive, None),
    ] {
        let (url, task) = server("200 OK", "text/event-stream", FIXTURE).await;
        let provider = Synthetic::new(
            SyntheticConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap();
        let mut model = model();
        model.info.features.thinking.supported = true;
        model.info.features.thinking.efforts = vec![Effort::High];
        let (tx, _rx) = flume::unbounded();
        provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(thinking)
                    .fast(false)
                    .build(),
            )
            .await
            .unwrap();
        let (_, body) = task.await.unwrap();
        assert_eq!(
            body.get("reasoning_effort").and_then(Value::as_str),
            expected
        );
        assert_eq!(body["max_completion_tokens"], 8192);
    }
}

#[tokio::test]
async fn discovery_preserves_metadata_and_normalizes_prices_without_name_heuristics() {
    let body = json!({"data":[
        {"id":"syn:large:vision","display_name":"Recommended","input_modalities":["text","image"],
         "output_modalities":["text"],"context_length":100000,"max_output_length":4096,
         "supported_features":["tools","reasoning"],
         "reasoning_parameters":{"efforts":["none","low","high","future"]},
         "pricing":{"prompt":"$0.000001","completion":"$0.000002","input_cache_reads":"$0.0000001"},
         "future_metadata":{"preserve":true}},
        {"id":"hf:example/embed","output_modalities":["embeddings"]}
    ]})
    .to_string();
    let (url, task) = server("200 OK", "application/json", &body).await;
    let provider = Synthetic::new(
        SyntheticConfig::builder()
            .base_url(url)
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let catalog = provider.model_catalog().await.unwrap();
    assert_eq!(catalog.len(), 1);
    let entry = &catalog[0];
    assert_eq!(entry.metadata["future_metadata"]["preserve"], true);
    assert_eq!(entry.info.ids, ["syn:large:vision"]);
    assert!(entry.info.features.vision);
    assert!(entry.info.features.thinking.supported);
    assert!(!entry.info.features.thinking.required);
    assert_eq!(entry.info.features.thinking.efforts.len(), 2);
    assert_eq!(entry.info.pricing.input, 1.0);
    assert_eq!(entry.info.pricing.output, 2.0);
    assert!((entry.info.pricing.cache_read - 0.1).abs() < 1e-9);
    assert_eq!(entry.info.config.max_output_tokens, Some(4096));
    assert_eq!(entry.info.tier, Default::default());
    let (headers, _) = task.await.unwrap();
    assert!(headers.starts_with("get /openai/v1/models "));
}

#[tokio::test]
async fn preserves_http_error_payloads() {
    let (url, task) = server(
        "429 Too Many Requests",
        "application/json",
        "{\"error\":{\"message\":\"rate limit\"}}",
    )
    .await;
    let provider = Synthetic::new(
        SyntheticConfig::builder()
            .base_url(url)
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let error = provider.list_models().await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Http { status: 429, ref body }
        if serde_json::from_str::<Value>(body).unwrap()["error"]["message"] == "rate limit")
    );
    task.await.unwrap();
}

#[tokio::test]
async fn rejects_invalid_credentials_and_unsupported_controls_without_io() {
    assert!(matches!(
        Synthetic::new(SyntheticConfig::builder().api_key("").build()),
        Err(ProviderError::EmptyApiKey)
    ));
    assert!(matches!(
        Synthetic::new(SyntheticConfig::builder().api_key("invalid\nkey").build()),
        Err(ProviderError::CredentialHeader { .. })
    ));
    let provider = Synthetic::new(
        SyntheticConfig::builder()
            .base_url("http://127.0.0.1:1")
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    for options in [
        RequestOptions::builder()
            .thinking(ThinkingConfig::Adaptive)
            .fast(false)
            .build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Off)
            .fast(true)
            .build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Off)
            .fast(false)
            .max_output_tokens(0_u32)
            .build(),
    ] {
        assert!(matches!(
            provider
                .stream_message(&model(), &[], "", &[], &tx, options)
                .await,
            Err(ProviderError::ThinkingUnsupported { .. }
                | ProviderError::FastModeUnsupported { .. }
                | ProviderError::ZeroOutputTokenLimit)
        ));
    }
}

#[tokio::test]
async fn dropping_the_request_future_closes_the_stream() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Synthetic::new(
        SyntheticConfig::builder()
            .base_url(format!(
                "http://{}/openai/v1",
                listener.local_addr().unwrap()
            ))
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n").await.unwrap();
        ready_tx.send(()).unwrap();
        let mut byte = [0];
        match timeout(Duration::from_secs(5), socket.read(&mut byte))
            .await
            .unwrap()
        {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("expected a closed connection, got {result:?}"),
        }
    });
    let (tx, _rx) = flume::unbounded();
    let model = model();
    let mut request =
        Box::pin(provider.stream_message(&model, &[], "", &[], &tx, RequestOptions::default()));
    tokio::select! {
        result = &mut request => panic!("request unexpectedly finished: {result:?}"),
        _ = ready_rx => {}
    }
    drop(request);
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}
