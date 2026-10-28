use animist::{
    history::{
        Alteration, ContentBlock, History, HistoryEntry, ImageSource, Message, MessageKind, Role,
        StoredMessage, ToolContent, ToolContents, VccCompactor,
    },
    protocol::{
        Protocol, ProtocolError, ProtocolInput,
        anthropic::{AnthropicOptions, AnthropicProtocolError, Messages},
        openai::{ChatCompletions, OpenAIProtocolError, OpenAiOptions, Responses},
    },
    provider::{
        Model, ModelInfo, Provider, ProviderError, RequestOptions,
        ollama::{Ollama, OllamaConfig},
    },
};
use async_trait::async_trait;
use serde_json::{Value, json};

fn rich_content() -> ToolContents {
    vec![
        ToolContent::Text {
            text: "Screenshot".into(),
        },
        ToolContent::Image {
            source: ImageSource::from_bytes("image/png", &[1, 2, 3]),
        },
        ToolContent::Image {
            source: ImageSource::Url {
                url: "https://example.com/image.png".into(),
            },
        },
        ToolContent::Text { text: "End".into() },
    ]
    .into()
}

fn result(content: ToolContents, loaded_tools: Vec<String>) -> Message {
    Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::ToolResult {
            tool_use_id: "call-1".into(),
            content,
            is_error: true,
            loaded_tools,
        }])
        .build()
}

fn request<P: Protocol<Request = Value>>(
    protocol: P,
    messages: &[Message],
    options: &P::Options,
) -> animist::protocol::Result<Value> {
    let mut conversation = vec![
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "screenshot".into(),
                input: json!({}),
                thought_signature: None,
            }])
            .build(),
    ];
    conversation.extend_from_slice(messages);
    let model = Model::builder()
        .id("fixture")
        .info(ModelInfo::default())
        .provider(Provider::new(Ollama::new(OllamaConfig::default())))
        .build();
    protocol.request(
        ProtocolInput::builder()
            .model(&model)
            .messages(conversation.as_slice())
            .system("")
            .tools(&[][..])
            .options(&RequestOptions::default())
            .build(),
        options,
    )
}

#[test]
fn rich_results_use_native_anthropic_and_responses_wire_content() {
    let messages = [result(rich_content(), vec![])];
    let anthropic = request(Messages, &messages, &AnthropicOptions::default()).unwrap();
    assert_eq!(
        anthropic["messages"][1]["content"][0],
        json!({
            "type":"tool_result", "tool_use_id":"call-1", "is_error":true,
            "content":[
                {"type":"text","text":"Screenshot"},
                {"type":"image","source":{"type":"base64","media_type":"image/png","data":"AQID"}},
                {"type":"image","source":{"type":"url","url":"https://example.com/image.png"}},
                {"type":"text","text":"End"}
            ]
        })
    );
    let responses = request(Responses, &messages, &OpenAiOptions::default()).unwrap();
    assert_eq!(
        responses["input"][1],
        json!({
            "type":"function_call_output", "call_id":"call-1",
            "output":[
                {"type":"input_text","text":"Screenshot"},
                {"type":"input_image","image_url":"data:image/png;base64,AQID"},
                {"type":"input_image","image_url":"https://example.com/image.png"},
                {"type":"input_text","text":"End"}
            ]
        })
    );
    let error: ProviderError = request(ChatCompletions, &messages, &OpenAiOptions::default())
        .unwrap_err()
        .into();
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::UnsupportedChatToolImage { id }
            }
        } if id == "call-1"
    ));
}

#[test]
fn text_parts_and_empty_results_keep_their_boundaries() {
    for content in [
        ToolContents::default(),
        "single".into(),
        vec![
            ToolContent::Text { text: "a".into() },
            ToolContent::Text { text: "b".into() },
        ]
        .into(),
    ] {
        let messages = [result(content.clone(), vec![])];
        let anthropic = request(Messages, &messages, &AnthropicOptions::default()).unwrap();
        assert_eq!(
            anthropic["messages"][1]["content"][0]["content"],
            serde_json::to_value(&content).unwrap()
        );
        let chat = request(ChatCompletions, &messages, &OpenAiOptions::default()).unwrap();
        let responses = request(Responses, &messages, &OpenAiOptions::default()).unwrap();
        if let [ToolContent::Text { text }] = content.0.as_slice() {
            assert_eq!(chat["messages"][1]["content"], text.as_str());
            assert_eq!(responses["input"][1]["output"], text.as_str());
        } else {
            assert_eq!(
                chat["messages"][1]["content"],
                serde_json::to_value(&content).unwrap()
            );
            assert_eq!(
                responses["input"][1]["output"].as_array().unwrap().len(),
                content.0.len()
            );
        }
    }
}

#[test]
fn unsupported_activation_metadata_and_media_are_not_silently_dropped() {
    let messages = [result("loaded".into(), vec!["deferred".into()])];
    assert!(matches!(
        request(Messages, &messages, &AnthropicOptions::default()),
        Err(ProtocolError::Anthropic {
            source: AnthropicProtocolError::UnsupportedLoadedTools
        })
    ));
    for error in [
        request(ChatCompletions, &messages, &OpenAiOptions::default()).unwrap_err(),
        request(Responses, &messages, &OpenAiOptions::default()).unwrap_err(),
    ] {
        assert!(matches!(
            error,
            ProtocolError::OpenAI {
                source: OpenAIProtocolError::UnsupportedLoadedTools { id }
            } if id == "call-1"
        ));
    }
    let messages = [result(
        vec![ToolContent::Image {
            source: ImageSource::from_bytes("image/tiff", &[0]),
        }]
        .into(),
        vec![],
    )];
    assert!(matches!(
        request(Messages, &messages, &AnthropicOptions::default()),
        Err(ProtocolError::Anthropic {
            source: AnthropicProtocolError::UnsupportedImageMediaType { media_type }
        }) if media_type == "image/tiff"
    ));
}

#[test]
fn legacy_strings_migrate_but_recursive_blocks_are_rejected() {
    let block: ContentBlock = serde_json::from_value(json!({
        "type":"tool_result", "tool_use_id":"legacy", "content":"old text",
        "is_error":true, "loaded_tools":["retained"]
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(&block).unwrap(),
        json!({
            "type":"tool_result", "tool_use_id":"legacy",
            "content":[{"type":"text","text":"old text"}],
            "is_error":true, "loaded_tools":["retained"]
        })
    );
    for forbidden in [
        json!({"type":"tool_use","id":"nested","name":"tool","input":{}}),
        json!({"type":"tool_result","tool_use_id":"nested","content":"bad"}),
        json!({"type":"thinking","thinking":"private","signature":"opaque"}),
        json!({"type":"redacted_thinking","data":"opaque"}),
    ] {
        assert!(serde_json::from_value::<ToolContents>(json!([forbidden])).is_err());
    }
}

struct ReviseResult;

#[async_trait]
impl Alteration for ReviseResult {
    async fn call(&self, message: &StoredMessage) -> Result<Option<StoredMessage>, animist::Error> {
        if matches!(
            message.message().content[0],
            ContentBlock::ToolResult { .. }
        ) {
            let mut revised = message.clone();
            revised.message_mut().content = result(rich_content(), vec!["retained".into()]).content;
            Ok(Some(revised))
        } else {
            Ok(None)
        }
    }
}

#[tokio::test]
async fn history_revisions_restore_and_vcc_keeps_rich_sources_and_tail() {
    let history = History::new();
    history
        .add(
            &Message::builder()
                .role(Role::Assistant)
                .kind(MessageKind::Turn)
                .content(vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "screenshot".into(),
                    input: json!({}),
                    thought_signature: None,
                }])
                .build(),
        )
        .await;
    let original = result("original".into(), vec![]);
    history.add(&original).await;
    history.alter(ReviseResult).await.unwrap();
    let revised = result(rich_content(), vec!["retained".into()]);
    assert_eq!(history.unaltered().await[1], original);
    assert_eq!(history.current().await[1], revised);
    let tail = Message::builder()
        .role(Role::User)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Text {
            text: "Next".into(),
        }])
        .build();
    history.add(&tail).await;
    let saved = serde_json::to_vec(history.snapshot().await.entries()).unwrap();
    let entries: Vec<HistoryEntry> = serde_json::from_slice(&saved).unwrap();
    let restored = History::from_entries(entries.clone());
    assert_eq!(restored.current().await, history.current().await);
    restored
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(1_usize)
                .build(),
        )
        .await
        .unwrap();
    let current = restored.current().await;
    let ContentBlock::Text { text } = &current[0].content[0] else {
        panic!("missing VCC summary");
    };
    assert!(text.contains("Screenshot Image omitted Image omitted End"));
    assert!(!text.contains("AQID"));
    assert!(!text.contains("example.com"));
    assert_eq!(current[1], tail);
    let after = restored.snapshot().await;
    assert_eq!(after.entries()[2].verbatim(), entries[1].verbatim());
    assert_eq!(after.entries()[2].revisions(), entries[1].revisions());
    assert_eq!(after.entries()[2].latest().message(), &revised);

    let tail_history = History::from_entries(entries);
    tail_history
        .compact(
            VccCompactor::builder()
                .keep_recent_messages(2_usize)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(tail_history.current().await[1], revised);
}
