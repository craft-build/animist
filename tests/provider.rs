use animist::{
    history::{ContentBlock, ImageSource, Message, MessageKind, Role},
    provider::{
        FastPricing, InputTransformation, Model, ModelInfo, Overrides, Pricing, Provider,
        ProviderEvent, ProviderImpl, RequestOptions, Result, StopReason, StreamResponse,
        ThinkingConfig, ThinkingSupport, Tier, TokenUsage, Tool,
    },
};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use flume::Sender;
use serde_json::json;

#[test]
fn messages_round_trip_structured_content() {
    let message = Message::builder()
        .role(Role::Assistant)
        .content(vec![
            ContentBlock::Text {
                text: "Checking".into(),
            },
            ContentBlock::Thinking {
                thinking: "Plan".into(),
                signature: Some("signature".into()),
            },
            ContentBlock::RedactedThinking {
                data: "opaque".into(),
            },
            ContentBlock::ToolUse {
                id: "call-1".into(),
                name: "lookup".into(),
                input: json!({"query": "rust"}),
                thought_signature: None,
            },
        ])
        .kind(MessageKind::Turn)
        .build();
    let value = serde_json::to_value(&message).unwrap();
    assert_eq!(value["role"], "assistant");
    assert_eq!(value["content"][3]["type"], "tool_use");
    assert!(value.get("kind").is_none());
    assert!(value.get("display_text").is_none());
    assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);

    let result = json!({
        "role": "user",
        "content": [{
            "type": "tool_result",
            "tool_use_id": "call-1",
            "content": "Found it"
        }]
    });
    let result: Message = serde_json::from_value(result).unwrap();
    assert!(result.kind.is_turn());
    assert!(matches!(
        &result.content[0],
        ContentBlock::ToolResult { is_error: false, loaded_tools, .. } if loaded_tools.is_empty()
    ));
}

#[test]
fn image_sources_round_trip() {
    let bytes = b"\x89PNG\r\n\x1a\n";
    let source = ImageSource::from_bytes("image/png", bytes);
    let ImageSource::Base64 { media_type, data } = &source else {
        panic!("expected base64 image");
    };
    assert_eq!(media_type, "image/png");
    assert_eq!(STANDARD.decode(data).unwrap(), bytes);
    assert_eq!(data, "iVBORw0KGgo=");
    assert_eq!(source.to_url(), "data:image/png;base64,iVBORw0KGgo=");
    assert_eq!(
        ImageSource::Url {
            url: "https://example.com/image.png".into()
        }
        .to_url(),
        "https://example.com/image.png"
    );

    for source in [
        source,
        ImageSource::Url {
            url: "https://example.com/image.png".into(),
        },
    ] {
        let message = Message {
            role: Role::User,
            content: vec![ContentBlock::Image { source }],
            ..Message::default()
        };
        let value = serde_json::to_value(&message).unwrap();
        assert_eq!(value["content"][0]["type"], "image");
        assert_eq!(serde_json::from_value::<Message>(value).unwrap(), message);
    }
}

#[test]
fn tools_preserve_arbitrary_schemas() {
    let schema = json!({
        "$defs": {"path": {"type": "string", "x-harness": "workspace"}},
        "type": "object",
        "properties": {"paths": {"type": "array", "items": {"$ref": "#/$defs/path"}}},
        "additionalProperties": false
    });
    let tool = Tool::builder()
        .name("custom_lookup")
        .description("A harness-defined tool")
        .input_schema(schema.clone())
        .build();
    let value = serde_json::to_value(&tool).unwrap();
    assert_eq!(value["input_schema"], schema);
    assert_eq!(serde_json::from_value::<Tool>(value).unwrap(), tool);
}

#[test]
fn model_overrides_stay_separate_from_catalog_data() {
    let info = ModelInfo {
        tier: Tier::Strong,
        ..ModelInfo::default()
    };
    let mut model = Model::builder()
        .id("example")
        .provider("test")
        .info(info.clone())
        .build();
    assert_eq!(model.overrides, Overrides::default());
    model.overrides.thinking = Some(ThinkingSupport::Required);
    assert_eq!(model.info, info);

    let pricing = Pricing {
        fast: Some(FastPricing {
            input: 1.0,
            output: 2.0,
        }),
        ..Pricing::ZERO
    };
    assert!(pricing.is_zero());
}

struct TestProvider;

#[async_trait]
impl ProviderImpl for TestProvider {
    fn name(&self) -> &str {
        "test"
    }

    async fn stream_message(
        &self,
        model: &Model,
        _messages: &[Message],
        _system: &str,
        tools: &[Tool],
        event_tx: &Sender<ProviderEvent>,
        options: RequestOptions,
    ) -> Result<StreamResponse> {
        assert_eq!(model.id, "example-alias");
        assert_eq!(tools[0].name, "lookup");
        assert_eq!(options.max_output_tokens, Some(128));
        event_tx
            .send_async(ProviderEvent::ToolInputDelta {
                id: "call-1".into(),
                partial_json: r#"{"query":"rust"}"#.into(),
            })
            .await
            .unwrap();
        Ok(StreamResponse {
            message: Message {
                role: Role::Assistant,
                content: vec![ContentBlock::ToolUse {
                    id: "call-1".into(),
                    name: "lookup".into(),
                    input: json!({"query": "rust"}),
                    thought_signature: None,
                }],
                ..Message::default()
            },
            usage: TokenUsage::default(),
            stop_reason: Some(StopReason::ToolUse),
            input_transformations: Vec::<InputTransformation>::new(),
        })
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(vec![])
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn downstream_provider_uses_public_types_and_default_adjustment() {
    let provider = Provider::new(TestProvider);
    assert_eq!(provider.name(), "test");
    let mut model = Model {
        id: "example-alias".into(),
        ..Model::default()
    };
    let original = model.clone();
    provider.adjust_model(&mut model).await.unwrap();
    assert_eq!(model, original);

    let (tx, rx) = flume::bounded(1);
    let response = provider
        .stream_message(
            &model,
            &[],
            "",
            &[Tool {
                name: "lookup".into(),
                ..Tool::default()
            }],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Off)
                .fast(false)
                .max_output_tokens(128_u32)
                .build(),
        )
        .await
        .unwrap();
    assert!(matches!(
        rx.recv_async().await.unwrap(),
        ProviderEvent::ToolInputDelta { .. }
    ));
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert!(matches!(
        response.message.content[0],
        ContentBlock::ToolUse { .. }
    ));
}
