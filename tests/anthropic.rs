#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::fmt::Write as _;

use animist::{
    catalog::{CatalogModel, ProviderCatalog},
    history::{ContentBlock, ImageSource, Message, MessageKind, Role},
    protocol::{ProtocolError, anthropic::AnthropicProtocolError},
    provider::{
        Effort, Features, Model, ModelInfo, Pricing, Provider, ProviderError, ProviderEvent,
        RequestOptions, StopReason, Thinking, ThinkingConfig, ThinkingSupport, Tier, Tool,
        anthropic::{Anthropic, AnthropicConfig},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn read_request(socket: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let (headers, start, length) = loop {
        let mut chunk = [0; 1024];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers = String::from_utf8(bytes[..end].to_vec()).unwrap();
            let length = headers
                .lines()
                .find_map(|line| {
                    line.to_lowercase()
                        .strip_prefix("content-length: ")
                        .map(|length| length.parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            break (headers, end + 4, length);
        }
    };
    while bytes.len() < start + length {
        let mut chunk = [0; 1024];
        let count = socket.read(&mut chunk).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&chunk[..count]);
    }
    let body = if length == 0 {
        Value::Null
    } else {
        serde_json::from_slice(&bytes[start..start + length]).unwrap()
    };
    (headers, body)
}

async fn server(
    replies: Vec<(u16, String)>,
) -> (String, tokio::task::JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in replies {
            let (mut socket, _) = listener.accept().await.unwrap();
            requests.push(read_request(&mut socket).await);
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
            for chunk in body.as_bytes().chunks(7) {
                if socket.write_all(chunk).await.is_err() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        }
        requests
    });
    (url, task)
}

fn provider(url: &str) -> Provider {
    Provider::new(
        Anthropic::new(
            AnthropicConfig::builder()
                .api_key("test-key")
                .base_url(url)
                .beta(vec!["custom-beta".to_owned()])
                .build(),
        )
        .unwrap()
        .with_client(reqwest::Client::new()),
    )
}

fn model(provider: Provider) -> Model {
    Model::builder()
        .id("custom-model")
        .provider(provider)
        .info(ModelInfo::default())
        .build()
}

fn sse(events: Vec<Value>) -> String {
    let mut body = String::new();
    for event in events {
        write!(
            body,
            "event: {}\r\ndata: {event}\r\n\r\n",
            event["type"].as_str().unwrap()
        )
        .expect("String formatting is infallible");
    }
    body
}

fn start() -> Value {
    json!({"type":"message_start","message":{
        "id":"msg-test","type":"message","role":"assistant","content":[],
        "usage":{"input_tokens":90,"output_tokens":1,"cache_creation_input_tokens":10,"cache_read_input_tokens":30}
    }})
}

fn finish(reason: &str) -> Vec<Value> {
    vec![
        json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":20}}),
        json!({"type":"message_stop"}),
    ]
}

fn text_stream(text: &str) -> String {
    let mut events = vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":text}}),
        json!({"type":"content_block_stop","index":0}),
    ];
    events.extend(finish("end_turn"));
    sse(events)
}

#[test]
fn config_defaults_and_credentials_are_safe_to_serialize() {
    let config: AnthropicConfig = serde_json::from_value(json!({})).unwrap();
    assert!(config == AnthropicConfig::default());
    assert!(config == AnthropicConfig::builder().build());
    assert_eq!(config.api_key_env, "ANTHROPIC_API_KEY");
    assert_eq!(config.base_url, "https://api.anthropic.com/v1");
    assert_eq!(config.version, "2023-06-01");
    assert_eq!(config.max_output_tokens, 4096);
    let config: AnthropicConfig = serde_json::from_value(json!({
        "api_key":"not-a-real-key","base_url":"http://localhost/v1","beta":["custom"],"max_output_tokens":8192
    })).unwrap();
    let serialized = serde_json::to_value(config.clone()).unwrap();
    assert!(serialized.get("api_key").is_none());
    assert!(!serialized.to_string().contains("not-a-real-key"));
    assert!(serialized.get("client").is_none());
    let restored: AnthropicConfig = serde_json::from_value(serialized).unwrap();
    let mut expected = config;
    expected.api_key = None;
    assert!(expected == restored);
}

#[tokio::test]
async fn provider_auth_and_typed_request_translation_work_over_http() {
    let (url, task) = server(vec![(200, text_stream("Héllo"))]).await;
    let provider = provider(&url);
    assert_eq!(provider.name(), "anthropic");
    let mut model = model(provider.clone());
    model.info.config.max_output_tokens = Some(256);
    let user = Message::builder()
        .role(Role::User)
        .content(vec![
            ContentBlock::Text {
                text: "Find it".into(),
            },
            ContentBlock::Image {
                source: ImageSource::from_bytes("image/png", b"png"),
            },
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/image.png".into(),
                },
            },
        ])
        .kind(MessageKind::Turn)
        .build();
    let schema =
        json!({"type":"object","properties":{"q":{"type":"string"}},"additionalProperties":false});
    let tool = Tool::builder()
        .name("lookup")
        .description("Search")
        .input_schema(schema.clone())
        .build();
    let (tx, rx) = flume::unbounded();
    let response = provider
        .stream_message(
            &model,
            &[user],
            "Be helpful",
            &[tool],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Off)
                .fast(true)
                .max_output_tokens(128_u32)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(response.usage.input, 90);
    assert_eq!(response.usage.output, 20);
    assert_eq!(response.usage.cache_creation, 10);
    assert_eq!(response.usage.cache_read, 30);
    assert_eq!(response.usage.cost, None);
    assert_eq!(
        response.message.content,
        vec![ContentBlock::Text {
            text: "Héllo".into()
        }]
    );
    assert_eq!(
        rx.try_iter()
            .find(|event| matches!(event, ProviderEvent::TextDelta { .. }))
            .unwrap(),
        ProviderEvent::TextDelta {
            text: "Héllo".into()
        }
    );
    let requests = task.await.unwrap();
    let (headers, request) = &requests[0];
    let headers = headers.to_lowercase();
    assert!(headers.starts_with("post /v1/messages "));
    assert!(headers.contains("x-api-key: test-key"));
    assert!(headers.contains("anthropic-version: 2023-06-01"));
    assert!(headers.contains("anthropic-beta: custom-beta,fast-mode-2026-02-01"));
    assert!(!headers.contains("authorization:"));
    assert_eq!(request["model"], "custom-model");
    assert_eq!(request["max_tokens"], 128);
    assert_eq!(request["system"], "Be helpful");
    assert_eq!(request["stream"], true);
    assert_eq!(request["thinking"], json!({"type":"disabled"}));
    assert_eq!(request["speed"], "fast");
    assert_eq!(request["tools"][0]["input_schema"], schema);
    assert_eq!(
        request["messages"][0]["content"][1]["source"],
        json!({"type":"base64","media_type":"image/png","data":"cG5n"})
    );
    assert_eq!(
        request["messages"][0]["content"][2]["source"],
        json!({"type":"url","url":"https://example.com/image.png"})
    );
}

#[tokio::test]
async fn thinking_signatures_redaction_and_interleaved_tools_survive_replay() {
    let mut events = vec![
        start(),
        json!({"type":"ping"}),
        json!({"type":"future_bookkeeping_event"}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"sig"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"nature"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"redacted_thinking","data":"opaque"}}),
        json!({"type":"content_block_stop","index":1}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"text","text":"Check "}}),
        json!({"type":"content_block_delta","index":2,"delta":{"type":"text_delta","text":"now"}}),
        json!({"type":"content_block_stop","index":2}),
        json!({"type":"content_block_start","index":3,"content_block":{"type":"tool_use","id":"call-a","name":"lookup","input":{}}}),
        json!({"type":"content_block_start","index":4,"content_block":{"type":"tool_use","id":"call-b","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"{\"q\":"}}),
        json!({"type":"content_block_delta","index":4,"delta":{"type":"input_json_delta","partial_json":"{\"q\":\"b\"}"}}),
        json!({"type":"content_block_delta","index":3,"delta":{"type":"input_json_delta","partial_json":"\"a\"}"}}),
        json!({"type":"content_block_stop","index":4}),
        json!({"type":"content_block_stop","index":3}),
        json!({"type":"message_delta","delta":{"stop_reason":null},"usage":{"output_tokens":10,"input_tokens":100,"cache_creation_input_tokens":12,"cache_read_input_tokens":40}}),
    ];
    events.extend(finish("tool_use"));
    let (url, task) = server(vec![(200, sse(events)), (200, text_stream("Done"))]).await;
    let provider = provider(&url);
    let (tx, rx) = flume::unbounded();
    let response = provider
        .stream_message(
            &model(provider.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(response.usage.input, 100);
    assert_eq!(response.usage.output, 20);
    assert_eq!(response.usage.cache_creation, 12);
    assert_eq!(response.usage.cache_read, 40);
    assert_eq!(
        response.message.content,
        vec![
            ContentBlock::Thinking {
                thinking: "Plan".into(),
                signature: Some("signature".into())
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into()
            },
            ContentBlock::Text {
                text: "Check now".into()
            },
            ContentBlock::ToolUse {
                id: "call-a".into(),
                name: "lookup".into(),
                input: json!({"q":"a"}),
                thought_signature: None
            },
            ContentBlock::ToolUse {
                id: "call-b".into(),
                name: "lookup".into(),
                input: json!({"q":"b"}),
                thought_signature: None
            },
        ]
    );
    let events: Vec<_> = rx
        .try_iter()
        .filter(|event| {
            !matches!(
                event,
                ProviderEvent::ContentComplete { .. } | ProviderEvent::Usage { .. }
            )
        })
        .collect();
    assert_eq!(events.len(), 8);
    assert!(matches!(&events[0], ProviderEvent::ThinkingDelta { text } if text == "Plan"));
    assert!(matches!(&events[6], ProviderEvent::ToolInputDelta { id, .. } if id == "call-b"));
    let results = Message::builder()
        .role(Role::User)
        .content(vec![
            ContentBlock::ToolResult {
                tool_use_id: "call-a".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            },
            ContentBlock::ToolResult {
                tool_use_id: "call-b".into(),
                content: "Failed".into(),
                is_error: true,
                loaded_tools: vec![],
            },
        ])
        .kind(MessageKind::Turn)
        .build();
    provider
        .stream_message(
            &model(provider.clone()),
            &[response.message, results],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    let requests = task.await.unwrap();
    let replay = &requests[1].1;
    assert_eq!(
        replay["messages"][0]["content"][0],
        json!({"type":"thinking","thinking":"Plan","signature":"signature"})
    );
    assert_eq!(
        replay["messages"][0]["content"][1],
        json!({"type":"redacted_thinking","data":"opaque"})
    );
    assert_eq!(
        replay["messages"][0]["content"][3]["input"],
        json!({"q":"a"})
    );
    assert_eq!(replay["messages"][1]["content"][0]["tool_use_id"], "call-a");
    assert!(
        replay["messages"][1]["content"][0]
            .get("is_error")
            .is_none()
    );
    assert_eq!(replay["messages"][1]["content"][1]["is_error"], true);
    assert!(replay.get("system").is_none());
    assert!(replay.get("tools").is_none());
}

#[tokio::test]
async fn budget_adaptive_and_effort_use_model_policy_not_model_names() {
    for (thinking, expected, effort) in [
        (
            ThinkingConfig::Budget(1024),
            json!({"type":"enabled","budget_tokens":1024}),
            None,
        ),
        (ThinkingConfig::Adaptive, json!({"type":"adaptive"}), None),
        (
            ThinkingConfig::Effort(Effort::High),
            json!({"type":"adaptive"}),
            Some("high"),
        ),
        (
            ThinkingConfig::Effort(Effort::XHigh),
            json!({"type":"adaptive"}),
            Some("xhigh"),
        ),
        (
            ThinkingConfig::Effort(Effort::Max),
            json!({"type":"adaptive"}),
            Some("max"),
        ),
    ] {
        let (url, task) = server(vec![(200, text_stream("ok"))]).await;
        let provider = provider(&url);
        let mut model = model(provider.clone());
        model.info.features.thinking.supported = true;
        model.info.features.thinking.efforts = vec![Effort::High, Effort::XHigh, Effort::Max];
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
        let requests = task.await.unwrap();
        assert_eq!(requests[0].1["thinking"], expected);
        assert_eq!(requests[0].1["max_tokens"], 4096);
        assert_eq!(requests[0].1["output_config"]["effort"].as_str(), effort);
        assert!(requests[0].1.get("speed").is_none());
        assert!(!requests[0].0.contains("fast-mode"));
    }
}

#[tokio::test]
async fn invalid_provider_policy_fails_before_transport() {
    let provider = provider("http://127.0.0.1:1");
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    for (thinking, support, required, expected) in [
        (
            ThinkingConfig::Off,
            true,
            true,
            ProviderError::ReasoningRequired {
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            ThinkingConfig::Adaptive,
            false,
            false,
            ProviderError::ThinkingUnsupported {
                model: "custom-model".into(),
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            ThinkingConfig::Budget(1023),
            true,
            false,
            ProviderError::InvalidThinkingBudget {
                budget: 1023,
                max_tokens: 4096,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            ThinkingConfig::Budget(4096),
            true,
            false,
            ProviderError::InvalidThinkingBudget {
                budget: 4096,
                max_tokens: 4096,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            ThinkingConfig::Effort(Effort::High),
            true,
            false,
            ProviderError::UnsupportedReasoningEffort {
                effort: Effort::High,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            ThinkingConfig::Effort(Effort::Minimal),
            true,
            false,
            ProviderError::UnsupportedReasoningEffort {
                effort: Effort::Minimal,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
    ] {
        let mut model = model(provider.clone());
        model.overrides.thinking = Some(if required {
            ThinkingSupport::Required
        } else if support {
            ThinkingSupport::Yes
        } else {
            ThinkingSupport::No
        });
        model.info.features.thinking.efforts = vec![Effort::Minimal];
        let (tx, _rx) = flume::unbounded();
        let error = provider
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
            .unwrap_err();
        assert_eq!(
            std::mem::discriminant(&error),
            std::mem::discriminant(&expected)
        );
    }
    let (tx, _rx) = flume::unbounded();
    let mut model = model(provider.clone());
    model.overrides.fast = Some(false);
    assert!(matches!(
        provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Off)
                    .fast(true)
                    .build()
            )
            .await,
        Err(ProviderError::FastModeDisabled { .. })
    ));
    model.provider = Provider::new(
        animist::provider::openai::OpenAi::new(
            animist::provider::openai::OpenAiConfig::builder()
                .api_key("test-key")
                .build(),
        )
        .unwrap(),
    )
    .into();
    assert!(matches!(
        provider
            .stream_message(&model, &[], "", &[], &tx, RequestOptions::default())
            .await,
        Err(ProviderError::ModelProviderMismatch { .. })
    ));
    assert!(matches!(
        Anthropic::new(AnthropicConfig::builder().api_key("").build()),
        Err(ProviderError::EmptyApiKey { .. })
    ));
    let error = Anthropic::new(
        AnthropicConfig::builder()
            .api_key_env("ANIMIST_ANTHROPIC_MISSING_TEST_KEY_936174")
            .build(),
    )
    .err()
    .unwrap();
    assert!(matches!(
        error,
        ProviderError::CredentialEnvironment {
            source: std::env::VarError::NotPresent,
            ..
        }
    ));
    let invalid_header = Provider::new(
        Anthropic::new(
            AnthropicConfig::builder()
                .api_key("invalid\nkey")
                .base_url("http://127.0.0.1:1")
                .build(),
        )
        .unwrap(),
    );
    assert!(matches!(
        invalid_header.list_models().await,
        Err(ProviderError::CredentialHeader { .. })
    ));
}

#[tokio::test]
async fn paginated_models_reuse_catalog_metadata_and_api_limits() {
    let (url, task) = server(vec![
        (200, json!({"data":[{"id":"model-a","max_tokens":1000,"max_input_tokens":10000}],"has_more":true,"last_id":"cursor/+ space"}).to_string()),
        (200, json!({"data":[
            {"id":"model-b","capabilities":{
                "thinking":{"supported":true},
                "image_input":{"supported":true},
                "effort":{"supported":true,"low":{"supported":true},"xhigh":{"supported":true},"max":{"supported":false}}
            }},
            {"id":"model-c"}
        ],"has_more":false,"last_id":"model-c"}).to_string()),
    ]).await;
    let info = ModelInfo::builder()
        .ids(vec!["catalog-alias".to_owned()])
        .tier(Tier::Medium)
        .features(
            Features::builder()
                .tool_examples(false)
                .thinking(Thinking::default())
                .vision(true)
                .deferred_tools(false)
                .build(),
        )
        .pricing(
            Pricing::builder()
                .input(2.0)
                .output(0.0)
                .cache_write(0.0)
                .cache_read(0.0)
                .build(),
        )
        .build();
    let mut catalog = ProviderCatalog::default();
    catalog.models.insert(
        "model-a".into(),
        CatalogModel::builder()
            .name("A")
            .info(info)
            .metadata(json!({}))
            .build(),
    );
    let provider = Provider::new(
        Anthropic::new(
            AnthropicConfig::builder()
                .api_key("test-key")
                .base_url(url)
                .catalog(catalog)
                .build(),
        )
        .unwrap(),
    );
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 3);
    assert_eq!(models[0].ids, ["model-a"]);
    assert!(models[0].features.vision);
    assert_eq!(models[0].pricing.input, Some(2.0));
    assert_eq!(models[0].config.max_output_tokens, Some(1000));
    assert_eq!(models[0].config.context_window, Some(10000));
    assert_eq!(models[1].ids, ["model-b"]);
    assert!(models[1].features.thinking.supported);
    assert!(models[1].features.vision);
    assert_eq!(
        models[1].features.thinking.efforts,
        [Effort::Low, Effort::XHigh]
    );
    assert_eq!(models[1].pricing, Pricing::default());
    assert!(!models[2].features.thinking.supported);
    assert!(!models[2].features.vision);
    assert_eq!(models[2].features.thinking.efforts, Vec::new());
    let requests = task.await.unwrap();
    assert!(requests[0].0.starts_with("GET /v1/models "));
    assert!(
        requests[1]
            .0
            .starts_with("GET /v1/models?after_id=cursor%2F%2B+space ")
    );
}

#[tokio::test]
async fn model_pagination_rejects_missing_and_repeated_cursors() {
    for (replies, repeated) in [
        (
            vec![(200, json!({"data":[],"has_more":true}).to_string())],
            false,
        ),
        (
            vec![
                (
                    200,
                    json!({"data":[],"has_more":true,"last_id":"same"}).to_string(),
                ),
                (
                    200,
                    json!({"data":[],"has_more":true,"last_id":"same"}).to_string(),
                ),
            ],
            true,
        ),
    ] {
        let (url, task) = server(replies).await;
        let error = provider(&url).list_models().await.unwrap_err();
        if repeated {
            assert!(
                matches!(error, ProviderError::RepeatedModelCursor { cursor ,
                ..} if cursor == "same")
            );
        } else {
            assert!(matches!(error, ProviderError::MissingModelCursor { .. }));
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn all_supported_stop_reasons_remain_distinct() {
    for (reason, expected) in [
        ("end_turn", StopReason::EndTurn),
        ("stop_sequence", StopReason::EndTurn),
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("pause_turn", StopReason::PauseTurn),
        ("refusal", StopReason::Refusal),
        (
            "model_context_window_exceeded",
            StopReason::ContextWindowExceeded,
        ),
    ] {
        let mut events = vec![start()];
        events.extend(finish(reason));
        let (url, task) = server(vec![(200, sse(events))]).await;
        let (tx, _rx) = flume::unbounded();
        let provider = provider(&url);
        let response = provider
            .stream_message(
                &model(provider.clone()),
                &[],
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(response.stop_reason, Some(expected.clone()));
        assert_eq!(
            serde_json::from_value::<StopReason>(serde_json::to_value(expected.clone()).unwrap())
                .unwrap(),
            expected
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn invalid_stream_transitions_have_specific_protocol_errors() {
    for (tail, expected) in [
        (
            vec![],
            AnthropicProtocolError::TruncatedStream {
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![start()],
            AnthropicProtocolError::UnexpectedEvent {
                event: "message_start",
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![json!({"type":"message_stop"})],
            AnthropicProtocolError::MissingStopReason {
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}),
            ],
            AnthropicProtocolError::UnexpectedBlockIndex {
                index: 1,
                expected: 0,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
            ],
            AnthropicProtocolError::UnknownBlock {
                index: 0,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"x"}}),
            ],
            AnthropicProtocolError::DeltaTypeMismatch {
                index: 0,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"content_block_stop","index":0}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"x"}}),
            ],
            AnthropicProtocolError::BlockAlreadyStopped {
                index: 0,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}),
            ],
            AnthropicProtocolError::UnclosedBlocks {
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"plan","signature":""}}),
                json!({"type":"content_block_stop","index":0}),
            ],
            AnthropicProtocolError::MissingThinkingSignature {
                index: 0,
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            vec![
                json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call","name":"f","input":{"x":1}}}),
                json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
            ],
            AnthropicProtocolError::ConflictingToolInput {
                id: "call".into(),
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
        (
            finish("future_reason"),
            AnthropicProtocolError::UnsupportedStopReason {
                reason: "future_reason".into(),
                backtrace: std::backtrace::Backtrace::capture(),
            },
        ),
    ] {
        let mut events = vec![start()];
        events.extend(tail);
        let (url, task) = server(vec![(200, sse(events))]).await;
        let (tx, _rx) = flume::unbounded();
        let provider = provider(&url);
        let error = provider
            .stream_message(
                &model(provider.clone()),
                &[],
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap_err();
        let ProviderError::Protocol {
            source: ProtocolError::Anthropic { source },
        } = error
        else {
            panic!("Wrong error layer: {error:?}");
        };
        assert_eq!(
            std::mem::discriminant(&source),
            std::mem::discriminant(&expected)
        );
        task.await.unwrap();
    }
}

#[tokio::test]
async fn malformed_tool_json_preserves_call_context_and_source() {
    use std::error::Error;
    let (url, task) = server(vec![(200, sse(vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"call-a","name":"lookup","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
        json!({"type":"content_block_stop","index":0}),
    ]))]).await;
    let (tx, _rx) = flume::unbounded();
    let provider = provider(&url);
    let error = provider
        .stream_message(
            &model(provider.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(error.source().unwrap().is::<serde_json::Error>());
    let ProviderError::Protocol {
        source:
            ProtocolError::Anthropic {
                source: AnthropicProtocolError::DecodeToolArguments { id, source },
            },
    } = error
    else {
        panic!("Wrong error layer: {error:?}");
    };
    assert_eq!(id, "call-a");
    assert!(source.is_eof());
    task.await.unwrap();
}

#[tokio::test]
async fn api_failures_preserve_payloads_and_preceding_events() {
    let payload = json!({"type":"overloaded_error","message":"busy","retry_after":7});
    let (url, task) = server(vec![(200, sse(vec![
        start(),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"partial"}}),
        json!({"type":"error","error":payload}),
    ]))]).await;
    let (tx, rx) = flume::unbounded();
    let client = provider(&url);
    let error = client
        .stream_message(
            &model(client.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Protocol {
        source: ProtocolError::Anthropic { source: AnthropicProtocolError::Api { error ,
        ..} }
    } if error == payload));
    assert_eq!(
        rx.try_iter()
            .find(|event| matches!(event, ProviderEvent::TextDelta { .. }))
            .unwrap(),
        ProviderEvent::TextDelta {
            text: "partial".into()
        }
    );
    task.await.unwrap();

    let (url, task) = server(vec![(401, payload.to_string())]).await;
    assert!(
        matches!(provider(&url).list_models().await, Err(ProviderError::Http { status: 401, body ,
        ..}) if serde_json::from_str::<Value>(&body).unwrap() == payload)
    );
    task.await.unwrap();

    let (url, task) = server(vec![(200, "data: {invalid}\n\n".into())]).await;
    let provider = provider(&url);
    let error = provider
        .stream_message(
            &model(provider.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::Anthropic {
                source: AnthropicProtocolError::DecodeEvent { .. }
            }
        }
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn closed_event_receiver_returns_through_protocol_errors() {
    let (url, task) = server(vec![(200, text_stream("hello"))]).await;
    let (tx, rx) = flume::unbounded();
    drop(rx);
    let provider = provider(&url);
    let error = provider
        .stream_message(
            &model(provider.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::Anthropic {
                source: AnthropicProtocolError::EventReceiver { .. }
            }
        }
    ));
    task.await.unwrap();
}

#[tokio::test]
async fn dropping_the_future_cancels_the_http_exchange() {
    use std::time::Duration;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n").await.unwrap();
        let body = sse(vec![
            start(),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"started"}}),
        ]);
        socket
            .write_all(format!("{:x}\r\n{body}\r\n", body.len()).as_bytes())
            .await
            .unwrap();
        let mut byte = [0; 1];
        let closed = tokio::time::timeout(Duration::from_secs(3), socket.read(&mut byte))
            .await
            .unwrap();
        assert!(matches!(closed, Ok(0) | Err(_)));
    });
    let provider = provider(&url);
    let model = model(provider.clone());
    let (tx, rx) = flume::bounded(1);
    {
        let stream = provider.stream_message(&model, &[], "", &[], &tx, RequestOptions::default());
        tokio::pin!(stream);
        tokio::select! {
            event = rx.recv_async() => assert!(matches!(event.unwrap(), ProviderEvent::TextDelta { .. } | ProviderEvent::Usage { .. })),
            result = &mut stream => panic!("Stream finished unexpectedly: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(3)) => panic!("No event received"),
        }
    }
    task.await.unwrap();
}

#[tokio::test]
async fn empty_tool_input_and_nullable_cache_usage_are_valid() {
    let mut initial = start();
    initial["message"]["usage"] = json!({
        "input_tokens":1,"output_tokens":0,"cache_creation_input_tokens":null
    });
    let mut events = vec![
        initial,
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"empty-call","name":"f","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":""}}),
        json!({"type":"content_block_stop","index":0}),
    ];
    events.extend(finish("tool_use"));
    let (url, task) = server(vec![(200, sse(events))]).await;
    let (tx, _rx) = flume::unbounded();
    let provider = provider(&url);
    let response = provider
        .stream_message(
            &model(provider.clone()),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.usage.input, 1);
    assert_eq!(response.usage.cache_creation, 0);
    assert_eq!(response.usage.cache_read, 0);
    assert!(
        matches!(&response.message.content[0], ContentBlock::ToolUse { id, input, .. } if id == "empty-call" && input == &json!({}))
    );
    task.await.unwrap();
}
