#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use animist::{
    history::{
        Alteration, CompactionEdit, CompactionPlan, Compactor, ContentBlock, History,
        HistorySnapshot, Message, Role, StoredMessage, VccCompactor,
    },
    protocol::{
        Protocol, ProtocolError, ProtocolInput, ReplayError, ReplayLocation,
        anthropic::{AnthropicOptions, AnthropicProtocolError, Messages},
        openai::{ChatCompletions, OpenAiOptions, Responses},
        validate_replay,
    },
    provider::{
        Model, ModelInfo, Provider, ProviderError, RequestOptions, SystemMessagePolicy,
        ollama::{Ollama, OllamaConfig},
    },
};
use async_trait::async_trait;
use serde_json::json;

fn message(role: Role, text: &str) -> Message {
    Message::builder()
        .role(role)
        .kind(animist::history::MessageKind::Turn)
        .content(vec![ContentBlock::Text { text: text.into() }])
        .build()
}

fn model() -> Model {
    Model::builder()
        .id("fixture")
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .info(ModelInfo::default())
        .build()
}

fn input<'a>(
    model: &'a Model,
    messages: &'a [Message],
    options: &'a RequestOptions,
) -> ProtocolInput<'a> {
    ProtocolInput::builder()
        .model(model)
        .messages(messages)
        .system("preamble")
        .tools(&[][..])
        .options(options)
        .build()
}

#[test]
fn openai_preserves_exact_instruction_order_and_roles() {
    let model = model();
    let options = RequestOptions::default();
    let mut instructions = message(Role::System, "lore");
    instructions.content.push(ContentBlock::Text {
        text: "steer".into(),
    });
    let messages = vec![
        message(Role::User, "first"),
        instructions,
        message(Role::Assistant, "reply"),
        message(Role::System, "last instruction"),
        message(Role::User, "last"),
    ];
    let input = input(&model, &messages, &options);
    let chat = ChatCompletions
        .request(input, &OpenAiOptions::default())
        .unwrap();
    assert_eq!(
        chat["messages"],
        json!([
            {"role": "system", "content": "preamble"},
            {"role": "user", "content": [{"type": "text", "text": "first"}]},
            {"role": "system", "content": [
                {"type": "text", "text": "lore"}, {"type": "text", "text": "steer"}
            ]},
            {"role": "assistant", "content": [{"type": "text", "text": "reply"}]},
            {"role": "system", "content": [{"type": "text", "text": "last instruction"}]},
            {"role": "user", "content": [{"type": "text", "text": "last"}]}
        ])
    );
    let responses = Responses.request(input, &OpenAiOptions::default()).unwrap();
    assert_eq!(responses["instructions"], "preamble");
    assert_eq!(
        responses["input"],
        json!([
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "first"}]},
            {"type": "message", "role": "system", "content": [
                {"type": "input_text", "text": "lore"}, {"type": "input_text", "text": "steer"}
            ]},
            {"type": "message", "role": "assistant", "content": [{"type": "input_text", "text": "reply"}]},
            {"type": "message", "role": "system", "content": [{"type": "input_text", "text": "last instruction"}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "last"}]}
        ])
    );
}

#[test]
fn anthropic_rejects_placement_unless_relocation_is_explicit() {
    let model = model();
    let messages = vec![
        message(Role::User, "first"),
        message(Role::System, "lore"),
        message(Role::Assistant, "reply"),
        message(Role::System, "steer"),
        message(Role::User, "last"),
    ];
    let options = RequestOptions::default();
    assert_eq!(options.system_message_policy, SystemMessagePolicy::Reject);
    let wire_options = AnthropicOptions::builder()
        .max_tokens(100_u32)
        .fast(false)
        .build();
    let error = Messages
        .request(input(&model, &messages, &options), &wire_options)
        .unwrap_err();
    assert!(error.to_string().contains("MoveToPreamble"));
    let error: ProviderError = error.into();
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::Anthropic {
                source: AnthropicProtocolError::UnsupportedSystemPlacement { message: 1, .. }
            }
        }
    ));
    let options = RequestOptions::builder()
        .system_message_policy(SystemMessagePolicy::MoveToPreamble)
        .build();
    let wire = Messages
        .request(input(&model, &messages, &options), &wire_options)
        .unwrap();
    assert_eq!(wire["system"], "preamble\n\nlore\n\nsteer");
    assert_eq!(
        wire["messages"],
        json!([
            {"role": "user", "content": [{"type": "text", "text": "first"}]},
            {"role": "assistant", "content": [{"type": "text", "text": "reply"}]},
            {"role": "user", "content": [{"type": "text", "text": "last"}]}
        ])
    );
    let no_instructions = vec![message(Role::User, "hello")];
    let wire = Messages
        .request(
            input(&model, &no_instructions, &RequestOptions::default()),
            &wire_options,
        )
        .unwrap();
    assert_eq!(wire["system"], "preamble");
}

#[test]
fn placement_policy_handles_leading_adjacent_and_empty_instructions() {
    let model = model();
    let options = RequestOptions::builder()
        .system_message_policy(SystemMessagePolicy::MoveToPreamble)
        .build();
    let mut leading = message(Role::System, "first");
    leading.content.push(ContentBlock::Text {
        text: "second".into(),
    });
    let messages = vec![
        leading,
        Message::builder()
            .role(Role::System)
            .kind(animist::history::MessageKind::Observation)
            .content(vec![])
            .build(),
        message(Role::System, ""),
        message(Role::User, "hello"),
    ];
    let view = ProtocolInput {
        system: "",
        ..input(&model, &messages, &options)
    };
    let wire = Messages
        .request(
            view,
            &AnthropicOptions::builder()
                .max_tokens(100_u32)
                .fast(false)
                .build(),
        )
        .unwrap();
    assert_eq!(wire["system"], "first\n\nsecond\n\n");
    assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
    for (wire, field) in [
        (
            ChatCompletions
                .request(view, &OpenAiOptions::default())
                .unwrap(),
            "messages",
        ),
        (
            Responses.request(view, &OpenAiOptions::default()).unwrap(),
            "input",
        ),
    ] {
        let items = wire[field].as_array().unwrap();
        assert_eq!(items.len(), messages.len());
        assert_eq!(items[0]["role"], "system");
        assert_eq!(items[1]["role"], "system");
        assert_eq!(items[1]["content"], json!([]));
        assert_eq!(items[2]["role"], "system");
        assert_eq!(items[3]["role"], "user");
    }
    let options = RequestOptions::default();
    assert!(matches!(
        Messages.request(
            input(&model, &messages, &options),
            &AnthropicOptions::default()
        ),
        Err(ProtocolError::Anthropic {
            source: AnthropicProtocolError::UnsupportedSystemPlacement { message: 0, .. }
        })
    ));
}

#[test]
fn system_replay_is_text_only_and_reports_original_locations() {
    let invalid_blocks = vec![
        ContentBlock::Image {
            source: animist::history::ImageSource::Url {
                url: "https://example.test/image".into(),
            },
        },
        ContentBlock::Thinking {
            thinking: "private".into(),
            signature: None,
        },
        ContentBlock::RedactedThinking {
            data: "opaque".into(),
        },
        ContentBlock::ToolUse {
            id: "call".into(),
            name: "tool".into(),
            input: json!({}),
            thought_signature: None,
        },
        ContentBlock::ToolResult {
            tool_use_id: "call".into(),
            content: "result".into(),
            is_error: false,
            loaded_tools: vec![],
        },
    ];
    for block in invalid_blocks {
        let mut system = message(Role::System, "valid");
        system.content.push(block);
        assert!(matches!(
            validate_replay(&[message(Role::User, "hello"), system], &[]),
            Err(ReplayError::NonTextSystemContent {
                location: ReplayLocation {
                    message: 1,
                    block: 1
                },
                ..
            })
        ));
    }
    validate_replay(
        &[message(Role::User, "hello"), message(Role::System, "lore")],
        &[],
    )
    .unwrap();
}

#[test]
fn legacy_messages_and_system_messages_round_trip() {
    for role in ["user", "assistant", "system"] {
        let legacy = json!({"role": role, "content": [{"type": "text", "text": "hello"}]});
        let decoded: Message = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), legacy);
    }
}

struct ReviseInstructions;

#[async_trait]
impl Alteration for ReviseInstructions {
    async fn call(&self, stored: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        if stored.message().role != Role::System {
            return Ok(None);
        }
        let mut revised = stored.clone();
        revised.message_mut().content = message(Role::System, "revised").content;
        Ok(Some(revised))
    }
}

struct EditInstructions;

#[async_trait]
impl Compactor for EditInstructions {
    async fn call(&self, _history: &HistorySnapshot) -> Result<CompactionPlan, animist::Error> {
        let stored = |text| {
            StoredMessage::builder()
                .timestamp(animist::jiff::Timestamp::now())
                .message(message(Role::System, text))
                .build()
        };
        Ok(CompactionPlan::builder()
            .edits(vec![
                CompactionEdit::Replace {
                    index: 1,
                    message: stored("replacement"),
                },
                CompactionEdit::Insert {
                    before: 2,
                    message: stored("inserted"),
                },
            ])
            .build())
    }
}

#[tokio::test]
async fn history_edits_preserve_instruction_roles_positions_and_revisions() {
    let history = History::new();
    history.add(message(Role::User, "hello")).await;
    history.add(message(Role::System, "original")).await;
    history.add(message(Role::Assistant, "reply")).await;
    history.alter(ReviseInstructions).await.unwrap();
    assert_eq!(history.current().await[1], message(Role::System, "revised"));
    assert_eq!(
        history.unaltered().await[1],
        message(Role::System, "original")
    );
    history.compact(EditInstructions).await.unwrap();
    let expected = vec![
        message(Role::User, "hello"),
        message(Role::System, "replacement"),
        message(Role::System, "inserted"),
        message(Role::Assistant, "reply"),
    ];
    assert_eq!(history.current().await, expected);
    let snapshot = history.snapshot().await;
    assert_eq!(snapshot.entries()[1].revisions().len(), 2);
    let saved = serde_json::to_string(snapshot.entries()).unwrap();
    let restored = History::from_entries(serde_json::from_str(&saved).unwrap());
    assert_eq!(restored.current().await, expected);
    assert_eq!(
        restored.verbatim().await[1],
        message(Role::System, "original")
    );
}

#[tokio::test]
async fn vcc_never_summarizes_instructions_or_crosses_their_boundary() {
    for instruction_index in [0, 2, 4] {
        let history = History::new();
        let mut original = Vec::new();
        for index in 0..10 {
            let next = if index == instruction_index {
                message(Role::System, "must remain instructions")
            } else {
                message(
                    if index % 2 == 0 {
                        Role::User
                    } else {
                        Role::Assistant
                    },
                    "dialogue",
                )
            };
            history.add(next.clone()).await;
            original.push(next);
        }
        for _ in 0..2 {
            history
                .compact(
                    VccCompactor::builder()
                        .keep_recent_messages(2_usize)
                        .max_summary_chars(100_usize)
                        .build(),
                )
                .await
                .unwrap();
            let current = history.current().await;
            let position = current.iter().position(|m| m.role == Role::System).unwrap();
            assert_eq!(&current[position..], &original[instruction_index..]);
            assert!(current[..position].iter().all(|m| {
                !serde_json::to_string(m)
                    .unwrap()
                    .contains("must remain instructions")
            }));
        }
    }
}

#[tokio::test]
async fn providers_send_instruction_roles_and_forward_relocation_policy_over_http() {
    use animist::provider::{
        anthropic::{Anthropic, AnthropicConfig},
        openai::{OpenAi, OpenAiConfig, Protocol as OpenAiProtocol},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        time::{Duration, timeout},
    };

    for name in ["openai-chat", "openai-responses", "ollama", "anthropic"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/v1", listener.local_addr().unwrap());
        let provider = match name {
            "openai-chat" | "openai-responses" => Provider::new(
                OpenAi::new(
                    OpenAiConfig::builder()
                        .api_key("fixture")
                        .base_url(url)
                        .protocol(if name == "openai-chat" {
                            OpenAiProtocol::ChatCompletions
                        } else {
                            OpenAiProtocol::Responses
                        })
                        .build(),
                )
                .unwrap(),
            ),
            "ollama" => Provider::new(Ollama::new(OllamaConfig::builder().base_url(url).build())),
            "anthropic" => Provider::new(
                Anthropic::new(
                    AnthropicConfig::builder()
                        .api_key("fixture")
                        .base_url(url)
                        .build(),
                )
                .unwrap(),
            ),
            _ => unreachable!(),
        };
        let model = Model::builder()
            .id("fixture")
            .provider(provider.clone())
            .info(ModelInfo::default())
            .build();
        let messages = vec![
            message(Role::User, "first"),
            message(Role::System, "lore"),
            message(Role::Assistant, "reply"),
            message(Role::User, "last"),
        ];
        let (tx, _rx) = flume::unbounded();
        if name == "anthropic" {
            let error = provider
                .stream_message(
                    &model,
                    &messages,
                    "preamble",
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
                        source: AnthropicProtocolError::UnsupportedSystemPlacement {
                            message: 1,
                            ..
                        }
                    }
                }
            ));
            assert!(
                timeout(Duration::from_millis(20), listener.accept())
                    .await
                    .is_err()
            );
        }
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let body = loop {
                let mut chunk = [0; 4096];
                let count = socket.read(&mut chunk).await.unwrap();
                assert_ne!(count, 0);
                bytes.extend_from_slice(&chunk[..count]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|line| {
                            line.to_lowercase()
                                .strip_prefix("content-length: ")
                                .map(|length| length.parse().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice::<serde_json::Value>(
                            &bytes[end + 4..end + 4 + length],
                        )
                        .unwrap();
                    }
                }
            };
            socket
                .write_all(
                    b"HTTP/1.1 400 Fixture\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                )
                .await
                .unwrap();
            body
        });
        let options = RequestOptions::builder()
            .system_message_policy(SystemMessagePolicy::MoveToPreamble)
            .build();
        let error = timeout(
            Duration::from_secs(5),
            provider.stream_message(&model, &messages, "preamble", &[], &tx, options),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, ProviderError::Http { .. }));
        let wire = server.await.unwrap();
        let field = if name == "openai-responses" {
            "input"
        } else {
            "messages"
        };
        let roles: Vec<_> = wire[field]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["role"].as_str().unwrap())
            .collect();
        if name == "anthropic" {
            assert_eq!(roles, ["user", "assistant", "user"]);
            assert_eq!(wire["system"], "preamble\n\nlore");
        } else if name == "openai-responses" {
            assert_eq!(roles, ["user", "system", "assistant", "user"]);
            assert_eq!(wire["instructions"], "preamble");
        } else {
            assert_eq!(roles, ["system", "user", "system", "assistant", "user"]);
            assert_eq!(
                wire["messages"][2]["content"],
                json!([{"type": "text", "text": "lore"}])
            );
        }
    }
}
