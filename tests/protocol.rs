use animist::{
    protocol::{
        Protocol, ProtocolInput, Result,
        openai::{ChatCompletions, OpenAiOptions, Responses},
    },
    provider::{
        ContentBlock, Message, MessageKind, Model, ModelInfo, ProviderEvent, RequestOptions, Role,
        StopReason, StreamResponse, TokenUsage,
    },
};
use async_trait::async_trait;
use flume::Sender;
use serde_json::Value;

#[test]
fn protocol_errors_remain_typed_through_the_provider_boundary() {
    use animist::{
        protocol::{ProtocolError, openai::OpenAIProtocolError},
        provider::ProviderError,
    };
    use std::error::Error;

    let source = serde_json::from_str::<Value>("{").unwrap_err();
    let protocol_error = OpenAIProtocolError::DecodeEvent { source };
    let display = protocol_error.to_string();
    let error: ProtocolError = protocol_error.into();
    assert_eq!(error.to_string(), display);
    let error: ProviderError = error.into();
    assert_eq!(error.to_string(), display);
    assert!(error.source().unwrap().is::<serde_json::Error>());
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::DecodeEvent { .. }
            }
        }
    ));
}

struct PlainText;

#[async_trait]
impl Protocol for PlainText {
    type Request = String;
    type Response = String;
    type Options = ();

    fn request(&self, input: ProtocolInput<'_>, _options: &()) -> Result<String> {
        Ok(input.system.to_owned())
    }

    async fn assemble(
        &self,
        response: String,
        tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse> {
        tx.send_async(ProviderEvent::TextDelta {
            text: response.clone(),
        })
        .await
        .unwrap();
        Ok(StreamResponse::builder()
            .message(
                Message::builder()
                    .role(Role::Assistant)
                    .kind(MessageKind::Turn)
                    .content(vec![ContentBlock::Text { text: response }])
                    .build(),
            )
            .usage(TokenUsage::default())
            .stop_reason(StopReason::EndTurn)
            .input_transformations(Vec::new())
            .build())
    }
}

#[tokio::test]
async fn downstream_protocol_supports_non_http_transport_and_dynamic_dispatch() {
    let protocol: &dyn Protocol<Request = String, Response = String, Options = ()> = &PlainText;
    let model = Model::default();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(&[][..])
        .system("Hello")
        .tools(&[][..])
        .options(&options)
        .build();
    let request = protocol.request(input, &()).unwrap();
    let (tx, rx) = flume::unbounded();
    let response = protocol.assemble(request, &tx).await.unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    assert_eq!(
        rx.try_recv().unwrap(),
        ProviderEvent::TextDelta {
            text: "Hello".into()
        }
    );
}

#[test]
fn openai_adapters_share_the_contract_without_sharing_wire_shapes() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ChatCompletions>();
    assert_send_sync::<Responses>();

    let model = Model::builder()
        .id("compatible-model")
        .provider("another-provider")
        .info(ModelInfo::default())
        .build();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages(&[][..])
        .system("Instructions")
        .tools(&[][..])
        .options(&options)
        .build();
    let wire_options = OpenAiOptions::builder().reasoning_effort("high").build();
    let adapters: [&dyn Protocol<Request = Value, Response = reqwest::Response, Options = OpenAiOptions>;
        2] = [&ChatCompletions, &Responses];
    let chat = adapters[0].request(input, &wire_options).unwrap();
    let responses = adapters[1].request(input, &wire_options).unwrap();
    assert_eq!(chat["messages"][0]["content"], "Instructions");
    assert_eq!(chat["reasoning_effort"], "high");
    assert!(chat.get("input").is_none());
    assert_eq!(responses["instructions"], "Instructions");
    assert_eq!(responses["reasoning"]["effort"], "high");
    assert!(responses.get("messages").is_none());
}
