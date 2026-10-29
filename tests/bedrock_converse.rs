#![cfg(feature = "bedrock")]
#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

#[path = "support/bedrock_eventstream.rs"]
mod wire;

use animist::{
    history::{ContentBlock, ImageSource, Message, MessageKind, Role, ToolContent},
    protocol::{
        Protocol, ProtocolError, ProtocolInput,
        bedrock::{
            BedrockProtocolError, EventStreamError,
            converse::{Converse, ConverseOptions},
        },
    },
    provider::{
        ExchangeEvent, InferenceExchange, Model, ModelInfo, Provider, ProviderEvent,
        RequestOptions, StopReason, SystemMessagePolicy, ThinkingConfig, Tool,
        bedrock::{
            Bedrock, BedrockConfig, Credentials, Region, SdkConfig, SharedCredentialsProvider,
        },
    },
};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::{Duration, timeout},
};

fn model() -> Model {
    let sdk = SdkConfig::builder()
        .region(Region::new("us-east-1"))
        .credentials_provider(SharedCredentialsProvider::new(Credentials::new(
            "fake", "fake", None, None, "test",
        )))
        .build();
    Model::builder()
        .id("example")
        .provider(Provider::new(
            Bedrock::from_sdk_config(BedrockConfig::default(), &sdk).unwrap(),
        ))
        .info(ModelInfo::default())
        .build()
}

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message::builder()
        .role(role)
        .content(content)
        .kind(MessageKind::Turn)
        .build()
}

async fn response(body: Vec<u8>, stalled: bool) -> (reqwest::Response, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0_u8; 4096];
            assert_ne!(socket.read(&mut buffer).await.unwrap(), 0);
            socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/vnd.amazon.eventstream\r\ntransfer-encoding: chunked\r\nconnection: close\r\n\r\n").await.unwrap();
            for chunk in body.chunks(if body.len() > 1024 * 1024 { 65536 } else { 7 }) {
                let header = format!("{:x}\r\n", chunk.len());
                if socket.write_all(header.as_bytes()).await.is_err()
                    || socket.write_all(chunk).await.is_err()
                    || socket.write_all(b"\r\n").await.is_err() {
                    return;
                }
            }
            if stalled {
                let mut byte = [0];
                assert!(matches!(socket.read(&mut byte).await, Ok(0) | Err(_)));
            } else {
                let _ = socket.write_all(b"0\r\n\r\n").await;
            }
        }).await.unwrap();
    });
    (reqwest::get(url).await.unwrap(), task)
}

#[test]
fn converse_translates_images_tools_signed_reasoning_and_model_specific_fields() {
    let image = ImageSource::from_bytes("image/png", &[0, 1, 2]);
    let messages = [
        message(
            Role::User,
            vec![ContentBlock::Image {
                source: image.clone(),
            }],
        ),
        message(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    thinking: "plan".into(),
                    signature: Some("signed".into()),
                },
                ContentBlock::RedactedThinking {
                    data: "AAE=".into(),
                },
                ContentBlock::ToolUse {
                    id: "call".into(),
                    name: "lookup".into(),
                    input: json!({"x":1}),
                    thought_signature: None,
                },
            ],
        ),
        message(
            Role::User,
            vec![ContentBlock::ToolResult {
                tool_use_id: "call".into(),
                name: Some("lookup".into()),
                content: vec![
                    ToolContent::Text {
                        text: "result".into(),
                    },
                    ToolContent::Image { source: image },
                ]
                .into(),
                is_error: true,
                loaded_tools: vec![],
            }],
        ),
    ];
    let tools = [Tool::builder()
        .name("lookup")
        .description("Find data")
        .input_schema(json!({"type":"object"}))
        .build()];
    let model = model();
    let request_options = RequestOptions::builder()
        .max_output_tokens(128_u32)
        .temperature(0.0)
        .top_p(0.8)
        .stop_sequences(vec![])
        .build();
    let options = ConverseOptions::builder()
        .additional_model_request_fields(json!({"thinking":{"type":"adaptive"}}))
        .build();
    let body = Converse
        .request(
            ProtocolInput::builder()
                .model(&model)
                .messages(messages.as_slice())
                .system("preamble")
                .tools(tools.as_slice())
                .options(&request_options)
                .build(),
            &options,
        )
        .unwrap();
    assert_eq!(body["system"], json!([{"text":"preamble"}]));
    assert_eq!(
        body["inferenceConfig"],
        json!({"maxTokens":128,"temperature":0.0,"topP":0.8,"stopSequences":[]})
    );
    assert_eq!(
        body["messages"][0]["content"][0],
        json!({"image":{"format":"png","source":{"bytes":"AAEC"}}})
    );
    assert_eq!(
        body["messages"][1]["content"],
        json!([
            {"reasoningContent":{"reasoningText":{"text":"plan","signature":"signed"}}},
            {"reasoningContent":{"redactedContent":"AAE="}},
            {"toolUse":{"toolUseId":"call","name":"lookup","input":{"x":1}}}
        ])
    );
    assert_eq!(
        body["messages"][2]["content"][0]["toolResult"],
        json!({
            "toolUseId":"call", "status":"error",
            "content":[{"text":"result"},{"image":{"format":"png","source":{"bytes":"AAEC"}}}]
        })
    );
    assert_eq!(
        body["toolConfig"]["tools"][0],
        json!({"toolSpec":{
            "name":"lookup","description":"Find data","inputSchema":{"json":{"type":"object"}}
        }})
    );
    assert_eq!(
        body["additionalModelRequestFields"],
        options.additional_model_request_fields.unwrap()
    );
    assert_eq!(body.get("model"), None);
}

#[test]
fn converse_rejects_unrepresentable_history_and_unresolved_calls_without_losing_data() {
    let model = model();
    let options = RequestOptions::default();
    let adapter_options = ConverseOptions::default();
    for (block, expected) in [
        (
            ContentBlock::Thinking {
                thinking: "plan".into(),
                signature: None,
            },
            "signature",
        ),
        (
            ContentBlock::Image {
                source: ImageSource::Url {
                    url: "https://example.com/image".into(),
                },
            },
            "role",
        ),
        (
            ContentBlock::RedactedThinking {
                data: "not base64".into(),
            },
            "base64",
        ),
        (
            ContentBlock::ToolUse {
                id: "call".into(),
                name: "lookup".into(),
                input: json!({}),
                thought_signature: None,
            },
            "replay",
        ),
    ] {
        let messages = [message(Role::Assistant, vec![block])];
        let original = messages.clone();
        let error = Converse
            .request(
                ProtocolInput::builder()
                    .model(&model)
                    .messages(messages.as_slice())
                    .system("")
                    .tools([].as_slice())
                    .options(&options)
                    .build(),
                &adapter_options,
            )
            .unwrap_err();
        match (expected, error) {
            (
                "signature",
                ProtocolError::Bedrock {
                    source: BedrockProtocolError::MissingReplaySignature { .. },
                },
            )
            | (
                "role",
                ProtocolError::Bedrock {
                    source: BedrockProtocolError::UnsupportedContent { .. },
                },
            )
            | (
                "base64",
                ProtocolError::Bedrock {
                    source: BedrockProtocolError::Base64 { .. },
                },
            )
            | ("replay", ProtocolError::Replay { .. }) => {}
            (_, error) => panic!("unexpected {expected} error: {error:?}"),
        }
        assert_eq!(messages, original);
    }
    for source in [
        ImageSource::Url {
            url: "https://example.com/image".into(),
        },
        ImageSource::Base64 {
            media_type: "image/bmp".into(),
            data: "AAE=".into(),
        },
        ImageSource::Base64 {
            media_type: "image/png".into(),
            data: "bad".into(),
        },
    ] {
        let messages = [message(Role::User, vec![ContentBlock::Image { source }])];
        assert!(matches!(
            Converse.request(
                ProtocolInput::builder()
                    .model(&model)
                    .messages(messages.as_slice())
                    .system("")
                    .tools([].as_slice())
                    .options(&options)
                    .build(),
                &adapter_options,
            ),
            Err(ProtocolError::Bedrock { .. })
        ));
    }
}

#[test]
fn converse_preserves_system_authority_only_with_explicit_relocation_and_validates_controls() {
    let model = model();
    let messages = [message(
        Role::System,
        vec![ContentBlock::Text {
            text: "instruction".into(),
        }],
    )];
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(messages.as_slice())
        .system("preamble")
        .tools([].as_slice())
        .options(&options)
        .build();
    assert!(matches!(
        Converse.request(input, &ConverseOptions::default()),
        Err(ProtocolError::Bedrock {
            source: BedrockProtocolError::UnsupportedSystemPlacement { message: 0, .. }
        })
    ));
    let relocated = RequestOptions::builder()
        .system_message_policy(SystemMessagePolicy::MoveToPreamble)
        .build();
    let body = Converse
        .request(
            ProtocolInput {
                options: &relocated,
                ..input
            },
            &ConverseOptions::default(),
        )
        .unwrap();
    assert_eq!(
        body["system"],
        json!([{"text":"preamble"},{"text":"instruction"}])
    );
    assert_eq!(body["messages"], json!([]));
    for options in [
        RequestOptions::builder().temperature(f64::NAN).build(),
        RequestOptions::builder().top_p(1.1).build(),
        RequestOptions::builder().presence_penalty(0.0).build(),
        RequestOptions::builder().frequency_penalty(0.0).build(),
        RequestOptions::builder()
            .stop_sequences(vec![String::new()])
            .build(),
        RequestOptions::builder().max_output_tokens(0_u32).build(),
        RequestOptions::builder()
            .thinking(ThinkingConfig::Adaptive)
            .build(),
        RequestOptions::builder().fast(true).build(),
    ] {
        assert!(
            Converse
                .request(
                    ProtocolInput {
                        messages: &[],
                        options: &options,
                        ..input
                    },
                    &ConverseOptions::default()
                )
                .is_err()
        );
    }
    assert!(matches!(
        Converse.request(
            ProtocolInput {
                messages: &[],
                ..input
            },
            &ConverseOptions::builder()
                .additional_model_request_fields(json!([]))
                .build()
        ),
        Err(ProtocolError::Bedrock {
            source: BedrockProtocolError::InvalidAdditionalFields { .. }
        })
    ));
}

#[tokio::test]
async fn converse_assembles_fragmented_text_tools_and_reasoning_with_ordered_checkpoints() {
    for fixture in [wire::TEXT, wire::TOOLS, wire::REASONING] {
        let (response, server) = response(wire::fixture(fixture), false).await;
        let (tx, rx) = flume::unbounded();
        let result = Converse.assemble(response, &tx).await.unwrap();
        server.await.unwrap();
        let events = rx.drain().collect::<Vec<_>>();
        let closed = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::ContentComplete { block } => Some(block.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(closed, result.message.content);
        assert_eq!(
            events.last(),
            Some(&ProviderEvent::Usage {
                usage: result.usage.clone()
            })
        );
        match fixture {
            wire::TEXT => {
                assert_eq!(
                    result.message.content,
                    vec![ContentBlock::Text {
                        text: "Hello 🦀".into()
                    }]
                );
                assert_eq!(result.usage.input, 12);
                assert_eq!(result.usage.cache_creation, 7);
                assert_eq!(result.usage.cache_read, 5);
                assert_eq!(result.stop_reason, Some(StopReason::EndTurn));
            }
            wire::TOOLS => {
                assert!(
                    matches!(&closed[0], ContentBlock::ToolUse { id, input, .. } if id == "a" && *input == json!({"x":1}))
                );
                assert!(
                    matches!(&closed[1], ContentBlock::ToolUse { id, input, .. } if id == "b" && *input == json!({}))
                );
                assert_eq!(result.stop_reason, Some(StopReason::ToolUse));
            }
            _ => assert_eq!(
                closed,
                vec![
                    ContentBlock::Thinking {
                        thinking: "A plan".into(),
                        signature: Some("signed-opaque".into())
                    },
                    ContentBlock::RedactedThinking {
                        data: "AAECAw==".into()
                    },
                    ContentBlock::Text {
                        text: "Done".into()
                    },
                ]
            ),
        }
    }
}

#[tokio::test]
async fn converse_checks_frame_crcs_lengths_headers_and_truncation() {
    let valid = wire::frame(
        &[(":message-type", "event"), (":event-type", "messageStart")],
        br#"{"role":"assistant"}"#,
    );
    let mut bad_prelude = valid.clone();
    bad_prelude[8] ^= 1;
    let mut bad_message = valid.clone();
    *bad_message.last_mut().unwrap() ^= 1;
    let mut bad_length = valid.clone();
    bad_length[..4].copy_from_slice(&15_u32.to_be_bytes());
    let crc = crc32fast::hash(&bad_length[..8]);
    bad_length[8..12].copy_from_slice(&crc.to_be_bytes());
    let mut oversized = bad_length.clone();
    oversized[..4].copy_from_slice(&(17_u32 * 1024 * 1024).to_be_bytes());
    let crc = crc32fast::hash(&oversized[..8]);
    oversized[8..12].copy_from_slice(&crc.to_be_bytes());
    for (body, expected) in [
        (bad_prelude, "prelude"),
        (bad_message, "message"),
        (bad_length, "length"),
        (oversized, "limit"),
        (valid[..5].to_vec(), "truncated"),
        (valid[..valid.len() - 1].to_vec(), "truncated"),
        (wire::raw_frame(&[5, b'a'], b"{}"), "header"),
        (
            wire::frame(
                &[(":message-type", "event"), (":message-type", "event")],
                b"{}",
            ),
            "duplicate",
        ),
        (wire::raw_frame(&[1, b'a', 255], b"{}"), "type"),
        (wire::frame(&[], b"{}"), "missing"),
    ] {
        let (response, server) = response(body, false).await;
        let (tx, _rx) = flume::unbounded();
        let error = Converse.assemble(response, &tx).await.unwrap_err();
        server.await.unwrap();
        let ProtocolError::Bedrock {
            source: BedrockProtocolError::EventStream { source },
        } = error
        else {
            panic!("expected framing error for {expected}, got {error:?}");
        };
        match (expected, source) {
            (
                "prelude",
                EventStreamError::Crc {
                    section: "prelude", ..
                },
            )
            | (
                "message",
                EventStreamError::Crc {
                    section: "message", ..
                },
            )
            | ("length", EventStreamError::InvalidLengths { .. })
            | ("limit", EventStreamError::FrameTooLarge { .. })
            | ("truncated", EventStreamError::TruncatedFrame { .. })
            | ("header", EventStreamError::TruncatedHeader { .. })
            | ("duplicate", EventStreamError::DuplicateHeader { .. })
            | ("type", EventStreamError::UnknownHeaderType { .. })
            | ("missing", EventStreamError::RequiredHeader { .. }) => {}
            (_, error) => panic!("unexpected {expected} framing error: {error:?}"),
        }
    }
}

#[tokio::test]
async fn converse_preserves_exception_payloads_and_rejects_invalid_event_sequences() {
    for kind in ["exception", "error"] {
        let body = br#"{"message":"failure","originalStatusCode":424,"opaque":{"a":1}}"#;
        let bytes = wire::frame(
            &[
                (":message-type", kind),
                (
                    if kind == "exception" {
                        ":exception-type"
                    } else {
                        ":error-code"
                    },
                    "modelStreamErrorException",
                ),
            ],
            body,
        );
        let (response, server) = response(bytes, false).await;
        let (tx, _rx) = flume::unbounded();
        assert!(
            matches!(Converse.assemble(response, &tx).await.unwrap_err(),
            ProtocolError::Bedrock { source: BedrockProtocolError::Api { kind, body: actual, .. } }
                if kind == "modelStreamErrorException" && actual == body)
        );
        server.await.unwrap();
    }
    for events in [
        json!([]),
        json!([{"event":"contentBlockDelta","payload":{"contentBlockIndex":0,"delta":{"text":"before start"}}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"messageStart","payload":{"role":"assistant"}}]),
        json!([{"event":"messageStart","payload":{"role":"user"}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"contentBlockDelta","payload":{"contentBlockIndex":99,"delta":{"text":"gap"}}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"contentBlockDelta","payload":{"contentBlockIndex":0,"delta":{"reasoningContent":{"text":"unsigned"}}}},{"event":"contentBlockStop","payload":{"contentBlockIndex":0}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"contentBlockDelta","payload":{"contentBlockIndex":0,"delta":{"text":"open"}}},{"event":"messageStop","payload":{"stopReason":"end_turn"}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"messageStop","payload":{"stopReason":"unknown"}}]),
        json!([{"event":"messageStart","payload":{"role":"assistant"}},{"event":"contentBlockDelta","payload":{"contentBlockIndex":0,"delta":{"text":"text","reasoningContent":{"text":"ambiguous"}}}}]),
    ] {
        let (response, server) = response(wire::fixture(&events.to_string()), false).await;
        let (tx, rx) = flume::unbounded();
        assert!(
            Converse.assemble(response, &tx).await.is_err(),
            "events: {events}"
        );
        assert!(
            !rx.drain()
                .any(|event| matches!(event, ProviderEvent::ContentComplete { .. })),
            "events: {events}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn converse_cancellation_keeps_only_closed_ordered_blocks_and_latest_usage() {
    let events: Vec<wire::Event> = serde_json::from_str(wire::REASONING).unwrap();
    for cut in [2, 4, 5, 8, 12] {
        let body: Vec<_> = events[..cut]
            .iter()
            .flat_map(|event| {
                wire::frame(
                    &[(":message-type", "event"), (":event-type", &event.event)],
                    &serde_json::to_vec(&event.payload).unwrap(),
                )
            })
            .collect();
        let (response, server) = response(body, true).await;
        let mut exchange = InferenceExchange::new(move |tx| async move {
            Converse.assemble(response, &tx).await.map_err(Into::into)
        });
        let expected = match cut {
            2 | 4 => 1,
            5 => 2,
            8 => 3,
            _ => 6,
        };
        for _ in 0..expected {
            assert!(matches!(
                timeout(Duration::from_secs(3), exchange.next())
                    .await
                    .unwrap(),
                Some(ExchangeEvent::Provider(_))
            ));
        }
        assert!(
            timeout(Duration::from_millis(25), exchange.next())
                .await
                .is_err()
        );
        let partial = exchange.cancel();
        assert_eq!(partial.display_thinking, "A plan");
        assert_eq!(
            partial.content.len(),
            match cut {
                2 | 4 => 0,
                5 => 1,
                8 => 2,
                _ => 3,
            }
        );
        assert_eq!(partial.usage.is_some(), cut == 12);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn converse_checkpoints_do_not_cross_open_block_gaps() {
    let events: Vec<wire::Event> = serde_json::from_str(wire::TOOLS).unwrap();
    let bytes: Vec<_> = events[..6]
        .iter()
        .flat_map(|event| {
            wire::frame(
                &[(":message-type", "event"), (":event-type", &event.event)],
                &serde_json::to_vec(&event.payload).unwrap(),
            )
        })
        .collect();
    let (response, server) = response(bytes, true).await;
    let mut exchange = InferenceExchange::new(move |tx| async move {
        Converse.assemble(response, &tx).await.map_err(Into::into)
    });
    for _ in 0..4 {
        assert!(matches!(
            exchange.next().await,
            Some(ExchangeEvent::Provider(_))
        ));
    }
    assert!(
        timeout(Duration::from_millis(25), exchange.next())
            .await
            .is_err()
    );
    assert_eq!(exchange.cancel().content, Vec::new());
    server.await.unwrap();
}

#[tokio::test]
async fn converse_bounds_total_wire_bytes_even_across_individually_valid_frames() {
    let mut bytes = wire::frame(
        &[(":message-type", "event"), (":event-type", "messageStart")],
        br#"{"role":"assistant"}"#,
    );
    let delta = wire::frame(
        &[
            (":message-type", "event"),
            (":event-type", "contentBlockDelta"),
        ],
        &serde_json::to_vec(&json!({
            "contentBlockIndex":0, "delta":{"text":"a".repeat(1024 * 1024)}
        }))
        .unwrap(),
    );
    for _ in 0..257 {
        bytes.extend_from_slice(&delta);
    }
    let (response, server) = response(bytes, false).await;
    let (tx, rx) = flume::unbounded();
    assert!(matches!(
        Converse.assemble(response, &tx).await.unwrap_err(),
        ProtocolError::Bedrock {
            source: BedrockProtocolError::EventStream {
                source: EventStreamError::ExchangeTooLarge {
                    limit: 268_435_456,
                    ..
                }
            }
        }
    ));
    assert!(
        !rx.drain()
            .any(|event| matches!(event, ProviderEvent::ContentComplete { .. }))
    );
    server.await.unwrap();
}

#[tokio::test]
async fn converse_maps_known_stop_reasons_and_rejects_missing_usage_or_trailing_frames() {
    for (reason, expected) in [
        ("end_turn", StopReason::EndTurn),
        ("stop_sequence", StopReason::EndTurn),
        ("tool_use", StopReason::ToolUse),
        ("max_tokens", StopReason::MaxTokens),
        ("guardrail_intervened", StopReason::Refusal),
        ("content_filtered", StopReason::Refusal),
        (
            "model_context_window_exceeded",
            StopReason::ContextWindowExceeded,
        ),
    ] {
        let body = wire::fixture(&json!([
            {"event":"messageStart","payload":{"role":"assistant"}},
            {"event":"messageStop","payload":{"stopReason":reason}},
            {"event":"metadata","payload":{"usage":{"inputTokens":1,"outputTokens":0,"totalTokens":1}}}
        ]).to_string());
        let (response, server) = response(body, false).await;
        let (tx, _rx) = flume::unbounded();
        assert_eq!(
            Converse.assemble(response, &tx).await.unwrap().stop_reason,
            Some(expected)
        );
        server.await.unwrap();
    }
    let mut trailing = wire::fixture(wire::TEXT);
    trailing.push(0);
    for body in [
        trailing,
        wire::fixture(
            &json!([
                {"event":"messageStart","payload":{"role":"assistant"}},
                {"event":"messageStop","payload":{"stopReason":"end_turn"}}
            ])
            .to_string(),
        ),
    ] {
        let (response, server) = response(body, false).await;
        let (tx, _rx) = flume::unbounded();
        assert!(Converse.assemble(response, &tx).await.is_err());
        server.await.unwrap();
    }
}
