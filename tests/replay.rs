#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

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
        name: None,
        content: "Interrupted; side effects may have occurred".into(),
        is_error: true,
        loaded_tools: vec![],
    }
}

#[test]
fn invalid_exchanges_report_exact_positions_and_call_identity() {
    type Case = (Vec<Message>, Box<dyn Fn(&ReplayError) -> bool>);
    let cases: Vec<Case> = vec![
        (
            vec![message(Role::Assistant, vec![call("")])],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::EmptyCallId { location ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 0 }
                )
            }),
        ),
        (
            vec![message(Role::Assistant, vec![call("a"), call("a")])],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::DuplicateCallId { location, first, id ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 1 }
                            && *first == ReplayLocation { message: 0, block: 0 }
                            && id == "a"
                )
            }),
        ),
        (
            vec![message(Role::User, vec![result("")])],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::EmptyResultId { location ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 0 }
                )
            }),
        ),
        (
            vec![message(Role::User, vec![result("unknown")])],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::OrphanResult { location, id ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 0 } && id == "unknown"
                )
            }),
        ),
        (
            vec![
                message(Role::User, vec![result("a")]),
                message(Role::Assistant, vec![call("a")]),
            ],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::OrphanResult { location, id ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 0 } && id == "a"
                )
            }),
        ),
        (
            vec![
                message(Role::Assistant, vec![call("a")]),
                message(Role::User, vec![result("a"), result("a")]),
            ],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::DuplicateResult { location, first, id ,
                    ..}
                        if *location == ReplayLocation { message: 1, block: 1 }
                            && *first == ReplayLocation { message: 1, block: 0 }
                            && id == "a"
                )
            }),
        ),
        (
            vec![message(Role::Assistant, vec![call("z"), call("a")])],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::MissingResult { location, id ,
                    ..}
                        if *location == ReplayLocation { message: 0, block: 0 } && id == "z"
                )
            }),
        ),
        (
            vec![
                message(Role::Assistant, vec![call("a")]),
                message(Role::User, vec![result("a")]),
                message(Role::Assistant, vec![call("a")]),
            ],
            Box::new(|error| {
                matches!(
                    error,
                    ReplayError::DuplicateCallId { location, first, id ,
                    ..}
                        if *location == ReplayLocation { message: 2, block: 0 }
                            && *first == ReplayLocation { message: 0, block: 0 }
                            && id == "a"
                )
            }),
        ),
    ];
    for (messages, expected) in cases {
        let before = serde_json::to_value(&messages).unwrap();
        let error = validate_replay(&messages, &[]).unwrap_err();
        assert!(expected(&error), "{error:?}");
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
        assert!(matches!(
            validate_replay(&[message(Role::User, vec![block])], &[]),
            Err(ReplayError::AssistantContentInUser { location, .. })
                if location == ReplayLocation { message: 0, block: 0 }
        ));
    }
    assert!(matches!(
        validate_replay(&[message(Role::Assistant, vec![result("a")])], &[]),
        Err(ReplayError::ResultInAssistant { location, .. })
            if location == ReplayLocation { message: 0, block: 0 }
    ));
    let mut empty_name = call("a");
    if let ContentBlock::ToolUse { name, .. } = &mut empty_name {
        name.clear();
    }
    assert!(matches!(
        validate_replay(&[message(Role::Assistant, vec![empty_name])], &[]),
        Err(ReplayError::EmptyCallName { location, .. })
            if location == ReplayLocation { message: 0, block: 0 }
    ));
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
        name: Some("historical-tool".into()),
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
    assert!(matches!(
        validate_replay(&[], &[tool("")]),
        Err(ReplayError::EmptyDeclaration { index, .. }) if index == 0
    ));
    assert!(matches!(
        validate_replay(&[], &[tool("x"), tool("x")]),
        Err(ReplayError::DuplicateDeclaration { index, first_index, name, .. })
            if index == 1 && first_index == 0 && name == "x"
    ));
    let empty_loaded: Box<dyn Fn(&ReplayError) -> bool> = Box::new(|error| {
        matches!(
            error,
            ReplayError::EmptyLoadedTool { location, index, .. }
                if *location == ReplayLocation { message: 1, block: 0 } && *index == 0
        )
    });
    let duplicate_loaded: Box<dyn Fn(&ReplayError) -> bool> = Box::new(|error| {
        matches!(
            error,
            ReplayError::DuplicateLoadedTool { location, index, first_index, name, .. }
                if *location == ReplayLocation { message: 1, block: 0 }
                    && *index == 1
                    && *first_index == 0
                    && name == "x"
        )
    });
    for (names, expected) in [
        (vec![String::new()], empty_loaded),
        (vec!["x".into(), "x".into()], duplicate_loaded),
    ] {
        let mut output = result("a");
        if let ContentBlock::ToolResult { loaded_tools, .. } = &mut output {
            *loaded_tools = names;
        }
        let error = validate_replay(
            &[
                message(Role::Assistant, vec![call("a")]),
                message(Role::User, vec![output]),
            ],
            &[],
        )
        .unwrap_err();
        assert!(expected(&error), "{error:?}");
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
            source: ProtocolError::Replay { source: ReplayError::OrphanResult { id, location, .. } }
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
            assert!(matches!(
                error,
                ReplayError::OrphanResult { location, id, .. }
                    if location == ReplayLocation { message: 0, block: 0 } && id == "a"
            ));
        } else {
            assert!(matches!(
                error,
                ReplayError::MissingResult { location, id, .. }
                    if location == ReplayLocation { message: 0, block: 0 } && id == "a"
            ));
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
