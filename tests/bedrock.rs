#![cfg(feature = "bedrock")]
#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

#[path = "support/bedrock_eventstream.rs"]
mod wire;

use std::fmt::Write as _;

use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use animist::{
    catalog::Catalog,
    history::{ContentBlock, Message, MessageKind, Role},
    protocol::{ProtocolError, anthropic::AnthropicProtocolError, openai::OpenAIProtocolError},
    provider::{
        Effort, Model, ModelInfo, Provider, ProviderError, ProviderEvent, ProviderImpl,
        RequestOptions, StopReason, ThinkingConfig, ThinkingSupport,
        bedrock::{
            Bedrock, BedrockConfig, BedrockError, Credentials, CredentialsError, CredentialsFuture,
            ListingMode, Protocol, ProvideCredentials, Region, SdkConfig,
            SharedCredentialsProvider,
        },
    },
};
use bon::Builder;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
    time::timeout,
};

#[derive(Debug, Clone, Builder)]
#[builder(on(_, into))]
struct Reply {
    status: u16,
    body: Vec<u8>,
    #[builder(default)]
    headers: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
struct Recorded {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

async fn read_request(socket: &mut TcpStream) -> Recorded {
    let mut bytes = Vec::new();
    loop {
        let mut buffer = [0; 4096];
        let count = socket.read(&mut buffer).await.unwrap();
        assert_ne!(count, 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            let header = std::str::from_utf8(&bytes[..end]).unwrap();
            let mut lines = header.lines();
            let mut start = lines.next().unwrap().split_whitespace();
            let method = start.next().unwrap().to_owned();
            let path = start.next().unwrap().to_owned();
            let headers: BTreeMap<_, _> = lines
                .map(|line| {
                    let (key, value) = line.split_once(':').unwrap();
                    (key.to_lowercase(), value.trim().to_owned())
                })
                .collect();
            let length: usize = headers
                .get("content-length")
                .map_or(0, |length| length.parse().unwrap());
            if bytes.len() >= end + 4 + length {
                return Recorded {
                    method,
                    path,
                    headers,
                    body: bytes[end + 4..end + 4 + length].to_vec(),
                };
            }
        }
    }
}

async fn server(replies: Vec<Reply>) -> (String, JoinHandle<Vec<Recorded>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut socket, _) = listener.accept().await.unwrap();
                requests.push(read_request(&mut socket).await);
                let mut headers = String::new();
                for (name, value) in reply.headers {
                    write!(headers, "{name}: {value}\r\n").expect("String formatting is infallible");
                }
                socket.write_all(format!(
                    "HTTP/1.1 {} Test\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n{headers}\r\n",
                    reply.status, reply.body.len(),
                ).as_bytes()).await.unwrap();
                for chunk in reply.body.chunks(7) {
                    if socket.write_all(chunk).await.is_err() { break; }
                    tokio::task::yield_now().await;
                }
            }
            requests
        }).await.unwrap()
    });
    (url, task)
}

fn sdk(credentials: impl ProvideCredentials + 'static) -> SdkConfig {
    SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .credentials_provider(SharedCredentialsProvider::new(credentials))
        .build()
}

fn static_sdk() -> SdkConfig {
    sdk(Credentials::new(
        "TESTACCESSKEY",
        "fake-test-secret",
        Some("fake-session-token".into()),
        None,
        "test",
    ))
}

fn model() -> Model {
    Model::builder()
        .provider(Provider::new(
            Bedrock::from_sdk_config(BedrockConfig::builder().build(), &static_sdk()).unwrap(),
        ))
        .id("global.example.model-profile")
        .info(ModelInfo::default())
        .build()
}

fn messages() -> Vec<Message> {
    vec![
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Text {
                text: "Hello".into(),
            }])
            .build(),
    ]
}

fn anthropic_text() -> String {
    [
        json!({"type":"message_start","message":{"role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}),
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
        json!({"type":"message_stop"}),
    ]
    .into_iter()
    .fold(String::new(), |mut body, event| {
        write!(body, "data: {event}\n\n").expect("String formatting is infallible");
        body
    })
}

fn chat_text() -> String {
    format!(
        "data: {}\n\ndata: [DONE]",
        json!({
            "choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":12,"completion_tokens":3}
        })
    )
}

fn responses_text() -> String {
    format!(
        "data: {}\n\ndata: {}\n\n",
        json!({"type":"response.output_text.delta","delta":"Hello"}),
        json!({
            "type":"response.completed","response":{"status":"completed","output":[
                {"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello"}]}
            ],"usage":{"input_tokens":12,"output_tokens":3}}
        })
    )
}

fn verify_wire_signature(request: &Recorded, region: &str) {
    use aws_sigv4::{
        http_request::{SignableBody, SignableRequest, SigningSettings, sign},
        sign::v4,
    };

    let date = &request.headers["x-amz-date"];
    let sent_at: std::time::SystemTime = time::Date::from_calendar_date(
        date[..4].parse().unwrap(),
        time::Month::try_from(date[4..6].parse::<u8>().unwrap()).unwrap(),
        date[6..8].parse().unwrap(),
    )
    .unwrap()
    .with_hms(
        date[9..11].parse().unwrap(),
        date[11..13].parse().unwrap(),
        date[13..15].parse().unwrap(),
    )
    .unwrap()
    .assume_utc()
    .into();
    let identity = Credentials::new(
        "TESTACCESSKEY",
        "fake-test-secret",
        Some("fake-session-token".into()),
        None,
        "test",
    )
    .into();
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(region)
        .name("bedrock")
        .time(sent_at)
        .settings(SigningSettings::default())
        .build()
        .unwrap()
        .into();
    let signed_headers = request.headers["authorization"]
        .split("SignedHeaders=")
        .nth(1)
        .unwrap()
        .split(',')
        .next()
        .unwrap()
        .split(';')
        .collect::<Vec<_>>();
    let headers = request
        .headers
        .iter()
        .filter(|(name, _)| {
            signed_headers.contains(&name.as_str())
                && !matches!(name.as_str(), "x-amz-date" | "x-amz-security-token")
        })
        .map(|(name, value)| (name.as_str(), value.as_str()));
    let url = format!("http://{}{}", request.headers["host"], request.path);
    let signable = SignableRequest::new(
        &request.method,
        &url,
        headers,
        SignableBody::Bytes(&request.body),
    )
    .unwrap();
    let (instructions, _) = sign(signable, &params).unwrap().into_parts();
    assert_eq!(
        instructions
            .headers()
            .find(|(name, _)| *name == "authorization")
            .unwrap()
            .1,
        request.headers["authorization"]
    );
}

#[test]
fn config_defaults_round_trip_and_sdk_types_are_public() {
    let defaults: BedrockConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(defaults, BedrockConfig::builder().build());
    assert_eq!(defaults, BedrockConfig::default());
    assert_eq!(defaults.protocol, Protocol::AnthropicMessages);
    for (protocol, wire) in [
        (Protocol::AnthropicMessages, "anthropic_messages"),
        (Protocol::ChatCompletions, "chat_completions"),
        (Protocol::Responses, "responses"),
    ] {
        let config = BedrockConfig::builder()
            .protocol(protocol)
            .region("us-west-2")
            .profile("development")
            .build();
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(value["protocol"], wire);
        assert!(value.get("credentials").is_none());
        assert_eq!(
            serde_json::from_value::<BedrockConfig>(value).unwrap(),
            config
        );
    }
}

#[test]
fn invalid_settings_fail_without_credential_discovery() {
    assert!(matches!(
        Bedrock::from_sdk_config(BedrockConfig::default(), &SdkConfig::builder().build()),
        Err(ProviderError::Bedrock {
            source: BedrockError::MissingRegion { .. }
        })
    ));
    assert!(matches!(
        Bedrock::from_sdk_config(
            BedrockConfig::default(),
            &SdkConfig::builder()
                .region(Region::new("us-east-1"))
                .build()
        ),
        Err(ProviderError::Bedrock {
            source: BedrockError::MissingCredentialsProvider { .. }
        })
    ));
    for attempts in [0_u32, 11] {
        assert!(matches!(
            Bedrock::from_sdk_config(
                BedrockConfig::builder().max_attempts(attempts).build(),
                &static_sdk()
            ),
            Err(ProviderError::Bedrock {
                source: BedrockError::InvalidMaxAttempts { .. }
            })
        ));
    }
    for endpoint in [
        "https://example.com/path",
        "https://example.com/?query=1",
        "https://example.com/#fragment",
        "https://user:password@example.com",
        "http://example.com",
    ] {
        assert!(matches!(
            Bedrock::from_sdk_config(
                BedrockConfig::builder().endpoint_url(endpoint).build(),
                &static_sdk()
            ),
            Err(ProviderError::Bedrock {
                source: BedrockError::InvalidEndpoint { .. }
            })
        ));
    }
    assert!(matches!(
        Bedrock::from_sdk_config(
            BedrockConfig::builder().region("invalid.region").build(),
            &static_sdk()
        ),
        Err(ProviderError::Bedrock {
            source: BedrockError::InvalidRegion { .. }
        })
    ));
}

#[tokio::test]
async fn all_native_routes_use_shared_adapters_and_sigv4() {
    for (protocol, path, body) in [
        (
            Protocol::AnthropicMessages,
            "/anthropic/v1/messages",
            anthropic_text(),
        ),
        (
            Protocol::ChatCompletions,
            "/openai/v1/chat/completions",
            chat_text(),
        ),
        (
            Protocol::Responses,
            "/openai/v1/responses",
            responses_text(),
        ),
    ] {
        let (url, task) = server(vec![
            Reply::builder()
                .status(200_u16)
                .body(body.into_bytes())
                .build(),
        ])
        .await;
        let provider = Provider::new(
            Bedrock::from_sdk_config(
                BedrockConfig::builder()
                    .protocol(protocol.clone())
                    .endpoint_url(url)
                    .region("us-west-2")
                    .anthropic_beta(vec!["example-beta".into()])
                    .build(),
                &static_sdk(),
            )
            .unwrap(),
        );
        assert_eq!(provider.name(), "amazon-bedrock");
        let (tx, rx) = flume::unbounded();
        let mut options = RequestOptions::builder().temperature(0.5).build();
        if protocol != Protocol::AnthropicMessages {
            options.top_p = Some(0.9);
        }
        if protocol != Protocol::Responses {
            options.stop_sequences = Some(vec!["END".into()]);
        }
        if protocol == Protocol::ChatCompletions {
            options.frequency_penalty = Some(0.25);
            options.presence_penalty = Some(-0.5);
        }
        let response = provider
            .stream_message(&model(), &messages(), "system", &[], &tx, options)
            .await
            .unwrap();
        assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
        assert_eq!(
            response.message.content,
            vec![ContentBlock::Text {
                text: "Hello".into()
            }]
        );
        assert_eq!(response.usage.input, 12);
        assert_eq!(response.usage.output, 3);
        assert!(
            rx.try_iter()
                .any(|event| matches!(event, ProviderEvent::TextDelta { text } if text == "Hello"))
        );
        let requests = task.await.unwrap();
        let request = &requests[0];
        assert_eq!(request.path, path);
        let authorization = &request.headers["authorization"];
        assert!(authorization.starts_with("AWS4-HMAC-SHA256 Credential=TESTACCESSKEY/"));
        assert!(authorization.contains("/us-west-2/bedrock/aws4_request"));
        assert!(authorization.contains("content-type"));
        assert!(authorization.contains("host"));
        assert!(authorization.contains("x-amz-date"));
        assert!(authorization.contains("x-amz-security-token"));
        assert_eq!(
            request.headers["x-amz-security-token"],
            "fake-session-token"
        );
        assert_eq!(request.headers["x-amz-date"].len(), 16);
        verify_wire_signature(request, "us-west-2");
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["model"], model().id);
        assert_eq!(body["temperature"], 0.5);
        if protocol != Protocol::AnthropicMessages {
            assert_eq!(body["top_p"], 0.9);
        }
        if protocol == Protocol::ChatCompletions {
            assert_eq!(body["frequency_penalty"], 0.25);
            assert_eq!(body["presence_penalty"], -0.5);
        }
        if protocol != Protocol::Responses {
            assert_eq!(
                body[if protocol == Protocol::AnthropicMessages {
                    "stop_sequences"
                } else {
                    "stop"
                }],
                json!(["END"])
            );
        }
        if protocol == Protocol::AnthropicMessages {
            assert_eq!(request.headers["anthropic-version"], "2023-06-01");
            assert_eq!(request.headers["anthropic-beta"], "example-beta");
            assert!(authorization.contains("anthropic-version"));
            assert_eq!(body["max_tokens"], 8192);
        } else {
            assert!(!request.headers.contains_key("anthropic-version"));
            if protocol == Protocol::Responses {
                assert_eq!(body["store"], false);
                assert_eq!(body["include"][0], "reasoning.encrypted_content");
            }
        }
    }
}

#[derive(Debug)]
struct Rotating {
    calls: Arc<AtomicUsize>,
}

impl ProvideCredentials for Rotating {
    fn provide_credentials<'a>(&'a self) -> CredentialsFuture<'a>
    where
        Self: 'a,
    {
        let number = self.calls.fetch_add(1, Ordering::SeqCst);
        CredentialsFuture::ready(Ok(Credentials::new(
            format!("TESTKEY{number}"),
            "fake-secret",
            Some(format!("fake-token-{number}")),
            None,
            "rotating-test",
        )))
    }
}

#[derive(Debug)]
struct MissingCredentials;

impl ProvideCredentials for MissingCredentials {
    fn provide_credentials<'a>(&'a self) -> CredentialsFuture<'a>
    where
        Self: 'a,
    {
        CredentialsFuture::ready(Err(CredentialsError::not_loaded(
            "mock credentials unavailable",
        )))
    }
}

#[tokio::test]
async fn credential_failure_preserves_the_aws_error_without_sending_a_request() {
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url("http://127.0.0.1:1")
            .build(),
        &sdk(MissingCredentials),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    assert!(matches!(
        provider
            .stream_message(
                &model(),
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::default()
            )
            .await,
        Err(ProviderError::Bedrock {
            source: BedrockError::Credentials {
                source: CredentialsError::CredentialsNotLoaded(_),
            }
        })
    ));
    assert!(matches!(
        provider.refresh_auth().await,
        Err(ProviderError::Bedrock {
            source: BedrockError::Credentials { .. }
        })
    ));
}

#[tokio::test]
async fn malformed_credential_headers_return_errors_instead_of_panicking() {
    for (access_key, token, expected_header) in [
        ("BAD\nACCESS", "token", "authorization"),
        ("TESTACCESS", "bad\ntoken", "x-amz-security-token"),
    ] {
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .endpoint_url("http://127.0.0.1:1")
                .build(),
            &sdk(Credentials::new(
                access_key,
                "fake-secret",
                Some(token.into()),
                None,
                "test",
            )),
        )
        .unwrap();
        let (tx, _rx) = flume::unbounded();
        let error = provider
            .stream_message(
                &model(),
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap_err();
        match error {
            ProviderError::Bedrock {
                source: BedrockError::SignedHeaderValue { header, .. },
            } => assert_eq!(header, expected_header),
            ProviderError::Bedrock {
                source: BedrockError::Signing { .. },
            } => {}
            error => panic!("unexpected credential failure: {error:?}"),
        }
    }
}

#[tokio::test]
async fn retry_exhaustion_returns_the_last_response_and_stops_at_the_limit() {
    let replies = (0..2)
        .map(|index| {
            Reply::builder()
                .status(429_u16)
                .body(format!("throttled-{index}").into_bytes())
                .headers(vec![("retry-after".into(), "0".into())])
                .build()
        })
        .collect();
    let (url, task) = server(replies).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url(url)
            .max_attempts(2_u32)
            .build(),
        &sdk(Rotating {
            calls: std::sync::Arc::clone(&calls),
        }),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(
            &model(),
            &messages(),
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Bedrock { source:BedrockError::Http { status:429, body, .. } }
        if body == b"throttled-1")
    );
    assert_eq!(task.await.unwrap().len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn oversized_throttle_bodies_are_still_retried() {
    let replies = (0..2)
        .map(|_| {
            Reply::builder()
                .status(429_u16)
                .body(vec![b'x'; 64 * 1024 + 1])
                .headers(vec![("retry-after".into(), "0".into())])
                .build()
        })
        .collect();
    let (url, task) = server(replies).await;
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url(url)
            .max_attempts(2_u32)
            .build(),
        &static_sdk(),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(
            &model(),
            &messages(),
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::Bedrock {
            source: BedrockError::ErrorBodyTooLarge { status: 429, .. }
        }
    ));
    assert_eq!(task.await.unwrap().len(), 2);
}

#[tokio::test]
async fn error_response_buffering_is_bounded_and_preserves_the_prefix() {
    let (url, task) = server(vec![
        Reply::builder()
            .status(403_u16)
            .body(vec![b'x'; 64 * 1024 + 1])
            .headers(vec![("x-amzn-requestid".into(), "oversized".into())])
            .build(),
    ])
    .await;
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder().endpoint_url(url).build(),
        &static_sdk(),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(
            &model(),
            &messages(),
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Bedrock { source:BedrockError::ErrorBodyTooLarge {
        status:403, request_id:Some(id), body_prefix, limit:65536, ..
    }} if id == "oversized" && body_prefix == vec![b'x'; 65536])
    );
    task.await.unwrap();
}

#[tokio::test]
async fn thinking_options_are_resolved_for_the_selected_protocol() {
    for (protocol, body) in [
        (Protocol::AnthropicMessages, anthropic_text()),
        (Protocol::ChatCompletions, chat_text()),
        (Protocol::Responses, responses_text()),
    ] {
        let (url, task) = server(vec![
            Reply::builder()
                .status(200_u16)
                .body(body.into_bytes())
                .build(),
        ])
        .await;
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .protocol(protocol.clone())
                .endpoint_url(url)
                .build(),
            &static_sdk(),
        )
        .unwrap();
        let mut model = model();
        model.info.features.thinking.supported = true;
        model.info.features.thinking.efforts = vec![Effort::High];
        let info = model.info.clone();
        let (tx, _rx) = flume::unbounded();
        provider
            .stream_message(
                &model,
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Effort(Effort::High))
                    .fast(false)
                    .max_output_tokens(4096_u32)
                    .build(),
            )
            .await
            .unwrap();
        let requests = task.await.unwrap();
        let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
        match protocol {
            Protocol::AnthropicMessages => {
                assert_eq!(body["thinking"]["type"], "adaptive");
                assert_eq!(body["output_config"]["effort"], "high");
            }
            Protocol::ChatCompletions => assert_eq!(body["reasoning_effort"], "high"),
            Protocol::Responses => assert_eq!(body["reasoning"]["effort"], "high"),
            Protocol::Converse => unreachable!("this test exercises native thinking dialects"),
        }
        assert_eq!(model.info, info);
    }
}

#[tokio::test]
async fn signed_messages_keep_reasoning_signatures_redaction_and_tool_results() {
    let (url, task) = server(vec![
        Reply::builder()
            .status(200_u16)
            .body(anthropic_text().into_bytes())
            .build(),
    ])
    .await;
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder().endpoint_url(url).build(),
        &static_sdk(),
    )
    .unwrap();
    let history = [
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: Some("opaque-signature".into()),
                },
                ContentBlock::RedactedThinking {
                    data: "opaque-redaction".into(),
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
                name: Some("lookup".into()),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            }])
            .build(),
    ];
    let before = serde_json::to_value(&history).unwrap();
    let (tx, _rx) = flume::unbounded();
    provider
        .stream_message(&model(), &history, "", &[], &tx, RequestOptions::default())
        .await
        .unwrap();
    let requests = task.await.unwrap();
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({
            "type":"thinking","thinking":"plan","signature":"opaque-signature"
        })
    );
    assert_eq!(
        body["messages"][0]["content"][1],
        json!({
            "type":"redacted_thinking","data":"opaque-redaction"
        })
    );
    assert_eq!(body["messages"][1]["content"][0]["tool_use_id"], "call-1");
    assert_eq!(serde_json::to_value(&history).unwrap(), before);
    verify_wire_signature(&requests[0], "us-east-1");
}

#[tokio::test]
async fn retries_are_bounded_and_resolve_and_sign_with_current_credentials() {
    let (url, task) = server(vec![
        Reply::builder()
            .status(429_u16)
            .body(b"throttled".to_vec())
            .headers(vec![("retry-after".into(), "0".into())])
            .build(),
        Reply::builder()
            .status(503_u16)
            .body(b"unavailable".to_vec())
            .headers(vec![("retry-after".into(), "0".into())])
            .build(),
        Reply::builder()
            .status(200_u16)
            .body(chat_text().into_bytes())
            .build(),
    ])
    .await;
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url(url)
            .protocol(Protocol::ChatCompletions)
            .build(),
        &sdk(Rotating {
            calls: std::sync::Arc::clone(&calls),
        }),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    provider
        .stream_message(
            &model(),
            &messages(),
            "",
            &[],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    let requests = task.await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert_eq!(requests.len(), 3);
    for (index, request) in requests.iter().enumerate() {
        assert_eq!(request.body, requests[0].body);
        assert!(request.headers["authorization"].contains(&format!("Credential=TESTKEY{index}/")));
        assert_eq!(
            request.headers["x-amz-security-token"],
            format!("fake-token-{index}")
        );
    }
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[tokio::test]
async fn converse_routes_encoded_model_ids_and_signs_the_exact_request_without_native_headers() {
    for (id, fixture, path) in [
        (
            "example.model:0",
            wire::TEXT,
            "/model/example.model:0/converse-stream",
        ),
        (
            "arn:aws:bedrock:us-east-1:123456789012:inference-profile/example",
            wire::TOOLS,
            "/model/arn:aws:bedrock:us-east-1:123456789012:inference-profile%2Fexample/converse-stream",
        ),
        (
            "global.example",
            wire::REASONING,
            "/model/global.example/converse-stream",
        ),
    ] {
        let (url, task) = server(vec![
            Reply::builder()
                .status(200_u16)
                .body(wire::fixture(fixture))
                .build(),
        ])
        .await;
        let fields = json!({"thinking":{"type":"adaptive"}});
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .protocol(Protocol::Converse)
                .endpoint_url(url)
                .converse_additional_model_request_fields(fields.clone())
                .build(),
            &static_sdk(),
        )
        .unwrap();
        let mut model = model();
        model.id = id.into();
        let (tx, rx) = flume::unbounded();
        let result = provider
            .stream_message(&model, &messages(), "", &[], &tx, RequestOptions::default())
            .await
            .unwrap();
        assert_ne!(result.message.content.len(), 0);
        assert!(
            rx.drain()
                .any(|event| matches!(event, ProviderEvent::ContentComplete { .. }))
        );
        let requests = task.await.unwrap();
        assert_eq!(requests.len(), 1);
        let request = &requests[0];
        assert_eq!(request.path, path);
        assert_eq!(
            request.headers["accept"],
            "application/vnd.amazon.eventstream"
        );
        assert!(!request.headers.contains_key("anthropic-version"));
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body.get("model"), None);
        assert_eq!(body["additionalModelRequestFields"], fields);
        assert_eq!(body["inferenceConfig"]["maxTokens"], 8192);
        verify_wire_signature(request, "us-east-1");
    }
}

#[tokio::test]
async fn an_injected_clients_timeout_cancels_a_stalled_request_without_retrying() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url(format!("http://{}", listener.local_addr().unwrap()))
            .build(),
        &static_sdk(),
    )
    .unwrap()
    .with_client(
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_millis(100))
            .build()
            .unwrap(),
    );
    let (tx, _rx) = flume::unbounded();
    let request = async {
        provider
            .stream_message(
                &model(),
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap_err()
    };
    let stalled = async {
        let (mut socket, _) = listener.accept().await.unwrap();
        let recorded = read_request(&mut socket).await;
        let mut byte = [0];
        assert_eq!(socket.read(&mut byte).await.unwrap(), 0);
        assert!(
            timeout(Duration::from_millis(150), listener.accept())
                .await
                .is_err()
        );
        recorded
    };
    let (error, recorded) = timeout(Duration::from_secs(3), async {
        tokio::join!(request, stalled)
    })
    .await
    .unwrap();
    assert!(matches!(
        error,
        ProviderError::Bedrock {
            source: BedrockError::Transport { source }
        } if source.is_timeout()
    ));
    assert_eq!(recorded.path, "/anthropic/v1/messages");
}

#[tokio::test]
async fn http_errors_preserve_request_ids_types_and_raw_bodies_without_redirects() {
    for status in [302_u16, 403, 429, 503] {
        let body = b"{\"error\":\"details\"}\xff".to_vec();
        let (url, task) = server(vec![
            Reply::builder()
                .status(status)
                .body(body.clone())
                .headers(vec![
                    ("x-amzn-requestid".into(), "request-123".into()),
                    ("x-amzn-errortype".into(), "ExampleException".into()),
                    ("location".into(), "http://127.0.0.1:1/never-follow".into()),
                ])
                .build(),
        ])
        .await;
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .endpoint_url(url)
                .max_attempts(if matches!(status, 429 | 503) {
                    1_u32
                } else {
                    3_u32
                })
                .build(),
            &static_sdk(),
        )
        .unwrap();
        let (tx, _rx) = flume::unbounded();
        let error = provider
            .stream_message(
                &model(),
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(error, ProviderError::Bedrock { source:BedrockError::Http {
            status:actual, request_id:Some(id), error_type:Some(kind), body:actual_body, ..
        }} if actual == status && id == "request-123" && kind == "ExampleException" && actual_body == body)
        );
        assert_eq!(task.await.unwrap().len(), 1);
    }
}

#[tokio::test]
async fn stream_failures_do_not_restart_inference_after_events_are_emitted() {
    for (protocol, body) in [
        (
            Protocol::ChatCompletions,
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n",
        ),
        (
            Protocol::AnthropicMessages,
            "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"failed\"}}\n\n",
        ),
    ] {
        let (url, task) = server(vec![
            Reply::builder()
                .status(200_u16)
                .body(body.as_bytes())
                .build(),
        ])
        .await;
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .endpoint_url(url)
                .protocol(protocol.clone())
                .build(),
            &sdk(Rotating {
                calls: std::sync::Arc::clone(&calls),
            }),
        )
        .unwrap();
        let (tx, rx) = flume::unbounded();
        let error = provider
            .stream_message(
                &model(),
                &messages(),
                "",
                &[],
                &tx,
                RequestOptions::default(),
            )
            .await
            .unwrap_err();
        match (protocol, error) {
            (
                Protocol::ChatCompletions,
                ProviderError::Protocol {
                    source:
                        ProtocolError::OpenAI {
                            source: OpenAIProtocolError::TruncatedChatStream { .. },
                        },
                },
            ) => assert_eq!(rx.len(), 1),
            (
                Protocol::AnthropicMessages,
                ProviderError::Protocol {
                    source:
                        ProtocolError::Anthropic {
                            source: AnthropicProtocolError::Api { error, .. },
                        },
                },
            ) => assert_eq!(error["type"], "overloaded_error"),
            (_, error) => panic!("unexpected error: {error:?}"),
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn control_plane_listing_signs_gets_and_merges_catalog_over_paginated_profiles() {
    let catalog = Catalog::from_slice(
        &serde_json::to_vec(&json!({"amazon-bedrock":{
            "id":"amazon-bedrock","name":"Amazon Bedrock","models":{
                "us.example":{"name":"Example","reasoning":true,"cost":{"input":2}},
                "catalog-only":{"name":"Not discovered"}
            }
        }}))
        .unwrap(),
    )
    .unwrap();
    let source = catalog.provider("amazon-bedrock").unwrap().clone();
    let (url, task) = server(
        [
            json!({"modelSummaries":[{"modelId":"foundation"}]}),
            json!({"inferenceProfileSummaries":[{"inferenceProfileId":"us.example"}],
                "nextToken":"a+/= ?&"}),
            json!({"inferenceProfileSummaries":[{"inferenceProfileId":"us.example"},
                {"inferenceProfileId":"global.example"}]}),
        ]
        .into_iter()
        .map(|body| {
            Reply::builder()
                .status(200_u16)
                .body(serde_json::to_vec(&body).unwrap())
                .build()
        })
        .collect(),
    )
    .await;
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .listing_mode(ListingMode::ControlPlane)
            .control_plane_endpoint_url(url)
            .endpoint_url("http://127.0.0.1:1")
            .catalog(source.clone())
            .build(),
        &static_sdk(),
    )
    .unwrap();
    let models = provider.list_models().await.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|info| info.ids[0].as_str())
            .collect::<Vec<_>>(),
        ["foundation", "global.example", "us.example"]
    );
    assert_eq!(models[2], source.models["us.example"].info);
    assert_eq!(models[0].pricing.input, None);
    assert!(!models[0].features.thinking.supported);
    let requests = task.await.unwrap();
    assert_eq!(requests[0].path, "/foundation-models");
    assert_eq!(requests[1].path, "/inference-profiles");
    assert_eq!(
        requests[2].path,
        "/inference-profiles?nextToken=a%2B%2F%3D+%3F%26"
    );
    for request in &requests {
        assert_eq!(request.method, "GET");
        assert_eq!(request.body, [0_u8; 0]);
        assert!(!request.headers.contains_key("anthropic-version"));
        verify_wire_signature(request, "us-east-1");
    }
}

#[tokio::test]
async fn control_plane_listing_rejects_cycles_and_preserves_errors() {
    for (replies, expected) in [
        (
            vec![
                (200_u16, json!({"modelSummaries":[]})),
                (
                    200,
                    json!({"inferenceProfileSummaries":[],"nextToken":"repeat"}),
                ),
                (
                    200,
                    json!({"inferenceProfileSummaries":[],"nextToken":"repeat"}),
                ),
            ],
            "token",
        ),
        (vec![(403, json!({"message":"denied"}))], "http"),
        (
            vec![(200, json!({"modelSummaries":"not an array"}))],
            "decode",
        ),
    ] {
        let (url, task) = server(
            replies
                .into_iter()
                .map(|(status, body)| {
                    Reply::builder()
                        .status(status)
                        .body(serde_json::to_vec(&body).unwrap())
                        .build()
                })
                .collect(),
        )
        .await;
        let provider = Bedrock::from_sdk_config(
            BedrockConfig::builder()
                .listing_mode(ListingMode::ControlPlane)
                .control_plane_endpoint_url(url)
                .build(),
            &static_sdk(),
        )
        .unwrap();
        match (expected, provider.list_models().await.unwrap_err()) {
            (
                "token",
                ProviderError::Bedrock {
                    source: BedrockError::InvalidListingToken { token, .. },
                },
            ) => assert_eq!(token, "repeat"),
            (
                "http",
                ProviderError::Bedrock {
                    source:
                        BedrockError::Http {
                            status: 403, body, ..
                        },
                },
            ) => assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap()["message"],
                "denied"
            ),
            (
                "decode",
                ProviderError::Bedrock {
                    source: BedrockError::DecodeListing { .. },
                },
            ) => {}
            (_, error) => panic!("unexpected listing error: {error:?}"),
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn model_metadata_listing_is_explicit_and_does_not_resolve_credentials() {
    let catalog = Catalog::from_slice(&serde_json::to_vec(&json!({"amazon-bedrock":{
        "id":"amazon-bedrock","name":"Amazon Bedrock","models":{"global.example.model-profile":{
            "name":"Example","reasoning":true,"reasoning_options":[{"type":"effort","values":["high"]}],
            "extra_source_metadata":{"preserved":true}
        }}
    }})).unwrap()).unwrap();
    let source = catalog.provider("amazon-bedrock").unwrap().clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder().catalog(source.clone()).build(),
        &sdk(Rotating {
            calls: std::sync::Arc::clone(&calls),
        }),
    )
    .unwrap();
    assert_eq!(
        provider.list_models().await.unwrap(),
        vec![source.models["global.example.model-profile"].info.clone()]
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        Bedrock::from_sdk_config(BedrockConfig::default(), &static_sdk())
            .unwrap()
            .list_models()
            .await
            .unwrap(),
        Vec::new()
    );
}

#[tokio::test]
async fn unsupported_model_policy_and_cross_protocol_history_fail_before_signing() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url("http://127.0.0.1:1")
            .protocol(Protocol::Responses)
            .build(),
        &sdk(Rotating {
            calls: std::sync::Arc::clone(&calls),
        }),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    for options in [
        RequestOptions::builder()
            .thinking(ThinkingConfig::Off)
            .fast(true)
            .build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Budget(1024))
            .fast(false)
            .build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Effort(Effort::High))
            .fast(false)
            .build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Off)
            .fast(false)
            .max_output_tokens(0_u32)
            .build(),
    ] {
        let error = provider
            .stream_message(&model(), &messages(), "", &[], &tx, options)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::FastModeUnsupported { .. }
                | ProviderError::ReasoningBudgetUnsupported { .. }
                | ProviderError::UnsupportedReasoningEffort { .. }
                | ProviderError::ZeroOutputTokenLimit { .. }
        ));
    }
    let mut model = model();
    model.overrides.thinking = Some(ThinkingSupport::Required);
    assert!(matches!(
        provider
            .stream_message(&model, &messages(), "", &[], &tx, RequestOptions::default())
            .await,
        Err(ProviderError::ReasoningRequired { .. })
    ));
    model.overrides.thinking = None;
    let history = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "do not discard".into(),
            signature: None,
        }])
        .build()];
    assert!(matches!(
        provider
            .stream_message(&model, &history, "", &[], &tx, RequestOptions::default())
            .await,
        Err(ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::MissingResponsesReasoningItem { .. },
            }
        })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn mismatched_provider_identity_fails_before_credentials_or_transport() {
    let calls = Arc::new(AtomicUsize::new(0));
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url("http://127.0.0.1:1")
            .build(),
        &sdk(Rotating {
            calls: std::sync::Arc::clone(&calls),
        }),
    )
    .unwrap();
    let mut model = model();
    model.provider = Provider::new(animist::provider::ollama::Ollama::new(
        animist::provider::ollama::OllamaConfig::default(),
    ))
    .into();
    let (tx, rx) = flume::unbounded();
    assert!(matches!(
        provider.stream_message(&model, &messages(), "", &[], &tx, RequestOptions::default()).await,
        Err(ProviderError::ModelProviderMismatch { expected, actual, .. })
            if expected == "amazon-bedrock" && actual == "ollama"
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(rx.is_empty());
}

#[tokio::test]
async fn dropping_the_future_cancels_the_signed_http_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = Bedrock::from_sdk_config(
        BedrockConfig::builder()
            .endpoint_url(format!("http://{}", listener.local_addr().unwrap()))
            .protocol(Protocol::ChatCompletions)
            .build(),
        &static_sdk(),
    )
    .unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"partial\"}}]}\n\n").await.unwrap();
        ready_tx.send(()).unwrap();
        let mut byte = [0];
        match timeout(Duration::from_secs(5), socket.read(&mut byte))
            .await
            .unwrap()
        {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("expected closed connection, got {result:?}"),
        }
    });
    let (tx, _rx) = flume::unbounded();
    let model = model();
    let mut future =
        Box::pin(provider.stream_message(&model, &[], "", &[], &tx, RequestOptions::default()));
    tokio::select! {
        result = &mut future => panic!("unexpected completion: {result:?}"),
        _ = ready_rx => {}
    }
    drop(future);
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
}
