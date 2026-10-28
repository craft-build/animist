use animist::{
    history::{ContentBlock, ImageSource, Message, MessageKind, Role, ToolContent},
    protocol::{
        Protocol, ProtocolError, ProtocolInput, ReplayError, ReplayLocation,
        anthropic::{AnthropicOptions, Messages},
        openai::{ChatCompletions, OpenAiOptions, Responses},
        validate_replay,
    },
    provider::{
        Model, ModelInfo, Provider, ProviderError, RequestOptions, Tool,
        ollama::{Ollama, OllamaConfig},
    },
};
use serde_json::json;

fn message(role: Role, content: Vec<ContentBlock>) -> Message {
    Message::builder()
        .role(role)
        .content(content)
        .kind(MessageKind::Turn)
        .build()
}

fn call(id: &str) -> ContentBlock {
    ContentBlock::ToolUse {
        id: id.into(),
        name: "historical-tool".into(),
        input: json!({"anything": [1, 2]}),
        thought_signature: None,
    }
}

fn result(id: &str) -> ContentBlock {
    ContentBlock::ToolResult {
        tool_use_id: id.into(),
        content: "Interrupted; side effects may have occurred".into(),
        is_error: true,
        loaded_tools: vec![],
    }
}

fn location(message: usize, block: usize) -> ReplayLocation {
    ReplayLocation { message, block }
}

#[test]
fn invalid_exchanges_report_exact_positions_and_call_identity() {
    let a = Role::Assistant;
    let u = Role::User;
    let cases = [
        (
            vec![message(a.clone(), vec![call("")])],
            ReplayError::EmptyCallId {
                location: location(0, 0),
            },
        ),
        (
            vec![message(a.clone(), vec![call("a"), call("a")])],
            ReplayError::DuplicateCallId {
                location: location(0, 1),
                first: location(0, 0),
                id: "a".into(),
            },
        ),
        (
            vec![message(u.clone(), vec![result("")])],
            ReplayError::EmptyResultId {
                location: location(0, 0),
            },
        ),
        (
            vec![message(u.clone(), vec![result("unknown")])],
            ReplayError::OrphanResult {
                location: location(0, 0),
                id: "unknown".into(),
            },
        ),
        (
            vec![
                message(u.clone(), vec![result("a")]),
                message(a.clone(), vec![call("a")]),
            ],
            ReplayError::OrphanResult {
                location: location(0, 0),
                id: "a".into(),
            },
        ),
        (
            vec![
                message(a.clone(), vec![call("a")]),
                message(u.clone(), vec![result("a"), result("a")]),
            ],
            ReplayError::DuplicateResult {
                location: location(1, 1),
                first: location(1, 0),
                id: "a".into(),
            },
        ),
        (
            vec![message(a.clone(), vec![call("z"), call("a")])],
            ReplayError::MissingResult {
                location: location(0, 0),
                id: "z".into(),
            },
        ),
        (
            vec![
                message(a.clone(), vec![call("a")]),
                message(u, vec![result("a")]),
                message(a, vec![call("a")]),
            ],
            ReplayError::DuplicateCallId {
                location: location(2, 0),
                first: location(0, 0),
                id: "a".into(),
            },
        ),
    ];
    for (messages, expected) in cases {
        let before = serde_json::to_value(&messages).unwrap();
        assert_eq!(validate_replay(&messages, &[]), Err(expected));
        assert_eq!(serde_json::to_value(&messages).unwrap(), before);
    }
}

#[test]
fn role_checks_do_not_interpret_or_discard_opaque_payloads() {
    for block in [
        call("a"),
        ContentBlock::Thinking {
            thinking: "private".into(),
            signature: Some("opaque".into()),
        },
        ContentBlock::RedactedThinking {
            data: "redacted".into(),
        },
    ] {
        assert_eq!(
            validate_replay(&[message(Role::User, vec![block])], &[]),
            Err(ReplayError::AssistantContentInUser {
                location: location(0, 0)
            })
        );
    }
    assert_eq!(
        validate_replay(&[message(Role::Assistant, vec![result("a")])], &[]),
        Err(ReplayError::ResultInAssistant {
            location: location(0, 0)
        })
    );
    let mut empty_name = call("a");
    if let ContentBlock::ToolUse { name, .. } = &mut empty_name {
        name.clear();
    }
    assert_eq!(
        validate_replay(&[message(Role::Assistant, vec![empty_name])], &[]),
        Err(ReplayError::EmptyCallName {
            location: location(0, 0)
        })
    );
}

#[test]
fn interleaved_multimodal_exchanges_and_interrupted_results_are_structurally_valid() {
    let mut signed_call = call("a");
    if let ContentBlock::ToolUse {
        thought_signature, ..
    } = &mut signed_call
    {
        *thought_signature = Some("vendor-specific-thought-signature".into());
    }
    let rich_result = ContentBlock::ToolResult {
        tool_use_id: "b".into(),
        content: vec![ToolContent::Image {
            source: ImageSource::from_bytes("image/png", &[1, 2]),
        }]
        .into(),
        is_error: false,
        loaded_tools: vec![],
    };
    let messages = vec![
        message(
            Role::Assistant,
            vec![
                ContentBlock::Thinking {
                    thinking: "private".into(),
                    signature: Some("opaque".into()),
                },
                ContentBlock::RedactedThinking {
                    data: "redacted".into(),
                },
                signed_call,
                call("b"),
            ],
        ),
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Observation)
            .content(vec![ContentBlock::Text {
                text: "Request-only context".into(),
            }])
            .build(),
        message(Role::User, vec![rich_result]),
        message(
            Role::Assistant,
            vec![ContentBlock::Text {
                text: "Interleaved text".into(),
            }],
        ),
        message(Role::User, vec![result("a")]),
    ];
    let before = serde_json::to_value(&messages).unwrap();
    validate_replay(&messages, &[]).unwrap();
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}

#[test]
fn declarations_and_activation_names_have_structural_checks_not_visibility_inference() {
    let tool = |name| {
        Tool::builder()
            .name(name)
            .description("")
            .input_schema(json!({}))
            .build()
    };
    assert_eq!(
        validate_replay(&[], &[tool("")]),
        Err(ReplayError::EmptyDeclaration { index: 0 })
    );
    assert_eq!(
        validate_replay(&[], &[tool("x"), tool("x")]),
        Err(ReplayError::DuplicateDeclaration {
            index: 1,
            first_index: 0,
            name: "x".into(),
        })
    );
    for (names, expected) in [
        (
            vec!["".into()],
            ReplayError::EmptyLoadedTool {
                location: location(1, 0),
                index: 0,
            },
        ),
        (
            vec!["x".into(), "x".into()],
            ReplayError::DuplicateLoadedTool {
                location: location(1, 0),
                index: 1,
                first_index: 0,
                name: "x".into(),
            },
        ),
    ] {
        let mut output = result("a");
        if let ContentBlock::ToolResult { loaded_tools, .. } = &mut output {
            *loaded_tools = names;
        }
        assert_eq!(
            validate_replay(
                &[
                    message(Role::Assistant, vec![call("a")]),
                    message(Role::User, vec![output])
                ],
                &[]
            ),
            Err(expected)
        );
    }
    let mut output = result("a");
    if let ContentBlock::ToolResult { loaded_tools, .. } = &mut output {
        *loaded_tools = vec!["previously-loaded".into()];
    }
    validate_replay(
        &[
            message(Role::Assistant, vec![call("a")]),
            message(Role::User, vec![output]),
        ],
        &[tool("current-tool")],
    )
    .unwrap();
}

#[test]
fn all_chat_protocols_share_validation_and_preserve_typed_provider_errors() {
    let model = Model::builder()
        .id("fixture")
        .info(ModelInfo::default())
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .build();
    let messages = [message(Role::User, vec![result("orphan")])];
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(messages.as_slice())
        .system("")
        .tools(&[][..])
        .options(&options)
        .build();
    for error in [
        ChatCompletions
            .request(input, &OpenAiOptions::default())
            .unwrap_err(),
        Responses
            .request(input, &OpenAiOptions::default())
            .unwrap_err(),
        Messages
            .request(input, &AnthropicOptions::default())
            .unwrap_err(),
    ] {
        let error: ProviderError = error.into();
        assert!(matches!(error, ProviderError::Protocol {
            source: ProtocolError::Replay { source: ReplayError::OrphanResult { id, location } }
        } if id == "orphan" && location == ReplayLocation { message: 0, block: 0 }));
    }
}

#[test]
fn protocols_accept_complete_interleaved_views_without_rewriting_multimodal_context() {
    let model = Model::builder()
        .id("fixture")
        .info(ModelInfo::default())
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .build();
    let messages = [
        message(Role::Assistant, vec![call("a"), call("b")]),
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Observation)
            .content(vec![
                ContentBlock::Text {
                    text: "Temporary context".into(),
                },
                ContentBlock::Image {
                    source: ImageSource::from_bytes("image/png", &[1]),
                },
            ])
            .build(),
        message(Role::User, vec![result("b")]),
        message(Role::User, vec![result("a")]),
    ];
    let before = serde_json::to_value(&messages).unwrap();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(messages.as_slice())
        .system("")
        .tools(&[][..])
        .options(&options)
        .build();
    let chat = ChatCompletions
        .request(input, &OpenAiOptions::default())
        .unwrap();
    let responses = Responses.request(input, &OpenAiOptions::default()).unwrap();
    let anthropic = Messages
        .request(input, &AnthropicOptions::default())
        .unwrap();
    assert_eq!(
        chat["messages"][1]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AQ=="
    );
    assert_eq!(chat["messages"][2]["tool_call_id"], "b");
    assert_eq!(chat["messages"][3]["tool_call_id"], "a");
    assert_eq!(
        responses["input"][2]["content"][1]["image_url"],
        "data:image/png;base64,AQ=="
    );
    assert_eq!(responses["input"][3]["call_id"], "b");
    assert_eq!(responses["input"][4]["call_id"], "a");
    assert_eq!(
        anthropic["messages"][1]["content"][1]["source"]["data"],
        "AQ=="
    );
    assert_eq!(anthropic["messages"][2]["content"][0]["tool_use_id"], "b");
    assert_eq!(anthropic["messages"][3]["content"][0]["tool_use_id"], "a");
    assert_eq!(serde_json::to_value(&messages).unwrap(), before);
}

struct RemoveEntry(usize);

#[async_trait::async_trait]
impl animist::history::Compactor for RemoveEntry {
    async fn call(
        &self,
        _history: &animist::history::HistorySnapshot,
    ) -> Result<animist::history::CompactionPlan, animist::Error> {
        Ok(animist::history::CompactionPlan::builder()
            .edits(vec![animist::history::CompactionEdit::Remove {
                index: self.0,
            }])
            .build())
    }
}

#[tokio::test]
async fn restored_revisions_and_explicit_compaction_are_validated_without_rewriting_archives() {
    use animist::history::{History, HistoryEntry, StoredMessage};
    let stored = |message| {
        StoredMessage::builder()
            .timestamp(jiff::Timestamp::now())
            .message(message)
            .build()
    };
    let entries = vec![
        HistoryEntry::builder()
            .source(stored(message(Role::User, vec![call("invalid-archived")])))
            .active(false)
            .build(),
        HistoryEntry::builder()
            .source(stored(message(Role::Assistant, vec![call("a")])))
            .build(),
        HistoryEntry::builder()
            .source(stored(message(Role::User, vec![result("old-source")])))
            .revisions(vec![stored(message(Role::User, vec![result("a")]))])
            .build(),
    ];
    let saved = serde_json::to_vec(&entries).unwrap();
    for removed in [1, 2] {
        let history = History::from_entries(serde_json::from_slice(&saved).unwrap());
        validate_replay(&history.current().await, &[]).unwrap();
        history.compact(RemoveEntry(removed)).await.unwrap();
        let before_validation = history.snapshot().await;
        let error = validate_replay(&history.current().await, &[]).unwrap_err();
        if removed == 1 {
            assert_eq!(
                error,
                ReplayError::OrphanResult {
                    location: location(0, 0),
                    id: "a".into()
                }
            );
        } else {
            assert_eq!(
                error,
                ReplayError::MissingResult {
                    location: location(0, 0),
                    id: "a".into()
                }
            );
        }
        assert_eq!(
            before_validation.entries(),
            history.snapshot().await.entries()
        );
        history
            .compact(RemoveEntry(if removed == 1 { 2 } else { 1 }))
            .await
            .unwrap();
        validate_replay(&history.current().await, &[]).unwrap();
        let snapshot = history.snapshot().await;
        for (before, after) in entries.iter().zip(snapshot.entries()) {
            assert_eq!(before.verbatim(), after.verbatim());
            assert_eq!(before.revisions(), after.revisions());
        }
    }
}
