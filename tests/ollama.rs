use std::time::Duration;

use animist::{
    catalog::{CatalogModel, ProviderCatalog},
    history::{ContentBlock, Message, MessageKind, Role},
    provider::{
        Effort, Model, ModelInfo, Provider, ProviderError, ProviderEvent, RequestOptions,
        StopReason, ThinkingConfig, ThinkingSupport, Tool,
        ollama::{Ollama, OllamaConfig},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const TEXT: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";
const TOOLS: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning\":\"Check 🦀\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n";

async fn server(
    status: u16,
    bodies: Vec<&str>,
) -> (String, JoinHandle<Vec<(String, Option<Value>)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let bodies = bodies.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let task = tokio::spawn(async move {
        timeout(Duration::from_secs(5), async {
            let mut requests = Vec::new();
            for body in bodies {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buffer = [0; 4096];
                    let count = socket.read(&mut buffer).await.unwrap();
                    assert_ne!(count, 0);
                    bytes.extend_from_slice(&buffer[..count]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap().to_lowercase();
                        let length = headers.lines().find_map(|line|
                            line.strip_prefix("content-length: ").map(|n| n.parse::<usize>().unwrap())
                        ).unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            let value = (length > 0).then(|| serde_json::from_slice(
                                &bytes[end + 4..end + 4 + length]
                            ).unwrap());
                            requests.push((headers, value));
                            break;
                        }
                    }
                }
                socket.write_all(format!("HTTP/1.1 {status} Test\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", body.len()).as_bytes()).await.unwrap();
                for chunk in body.as_bytes().chunks(7) {
                    if socket.write_all(chunk).await.is_err() { break; }
                    tokio::task::yield_now().await;
                }
            }
            requests
        }).await.unwrap()
    });
    (url, task)
}

fn model() -> Model {
    let mut info = ModelInfo::default();
    info.features.thinking.supported = true;
    info.features.thinking.efforts = vec![Effort::Low, Effort::Max];
    info.config.max_output_tokens = Some(128);
    Model::builder()
        .provider(provider("http://127.0.0.1:1/v1".into()))
        .id("local-model:latest")
        .info(info)
        .build()
}

fn provider(url: String) -> Provider {
    Provider::new(
        Ollama::new(OllamaConfig::builder().base_url(url).build())
            .with_client(reqwest::Client::new()),
    )
}

#[test]
fn config_defaults_and_serde_agree() {
    let config: OllamaConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(config, OllamaConfig::default());
    assert_eq!(config, OllamaConfig::builder().build());
    assert_eq!(config.base_url, "http://localhost:11434/v1");
    assert_eq!(
        serde_json::from_value::<OllamaConfig>(serde_json::to_value(&config).unwrap()).unwrap(),
        config
    );
}

#[tokio::test]
async fn streams_and_replays_ollama_reasoning_with_tools() {
    let (url, task) = server(200, vec![TOOLS, TEXT]).await;
    let provider = provider(url);
    assert_eq!(provider.name(), "ollama");
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    let tool = Tool::builder()
        .name("lookup")
        .description("Look up a path")
        .input_schema(json!({"type":"object","properties":{"path":{"type":"string"}}}))
        .build();
    let (tx, rx) = flume::unbounded();
    let first = provider
        .stream_message(
            &model(),
            &[],
            "system",
            std::slice::from_ref(&tool),
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Effort(Effort::Max))
                .fast(false)
                .max_output_tokens(512_u32)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(first.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(first.usage.input, 10);
    assert_eq!(first.usage.output, 5);
    assert!(matches!(&first.message.content[0],
        ContentBlock::Thinking { thinking, signature: None } if thinking == "Check 🦀"));
    assert!(matches!(&first.message.content[1],
        ContentBlock::ToolUse { id, input, .. } if id == "call-1" && input == &json!({"path":"src"})));
    assert!(
        rx.drain().any(
            |event| matches!(event, ProviderEvent::ThinkingDelta { text } if text == "Check 🦀")
        )
    );
    let messages = [
        first.message,
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::ToolResult {
                tool_use_id: "call-1".into(),
                content: "Found".into(),
                is_error: false,
                loaded_tools: vec![],
            }])
            .build(),
        Message::builder()
            .role(Role::Assistant)
            .kind(MessageKind::Turn)
            .content(vec![
                ContentBlock::Thinking {
                    thinking: "Explain".into(),
                    signature: None,
                },
                ContentBlock::Text {
                    text: "Found ".into(),
                },
                ContentBlock::Text { text: "it".into() },
            ])
            .build(),
    ];
    let response = provider
        .stream_message(
            &model(),
            &messages,
            "system",
            &[tool],
            &tx,
            RequestOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::EndTurn));
    let requests = task.await.unwrap();
    for (headers, body) in &requests {
        assert!(headers.starts_with("post /v1/chat/completions "));
        assert!(!headers.contains("authorization:"));
        let body = body.as_ref().unwrap();
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("service_tier").is_none());
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
    let first = requests[0].1.as_ref().unwrap();
    assert_eq!(first["max_tokens"], 512);
    assert_eq!(first["reasoning_effort"], "max");
    let second = requests[1].1.as_ref().unwrap();
    assert_eq!(second["max_tokens"], 128);
    assert_eq!(second["reasoning_effort"], "none");
    assert_eq!(second["messages"][1]["reasoning"], "Check 🦀");
    assert_eq!(second["messages"][1]["content"], "");
    assert!(second["messages"][1].get("reasoning_content").is_none());
    assert_eq!(second["messages"][2]["tool_call_id"], "call-1");
    assert_eq!(second["messages"][3]["content"], "Found it");
    assert_eq!(second["messages"][3]["reasoning"], "Explain");
}

#[tokio::test]
async fn lists_server_models_without_inventing_capabilities() {
    let (url, task) = server(
        200,
        vec![r#"{"data":[{"id":"local-model:latest"},{"id":"other:7b"}]}"#],
    )
    .await;
    let models = provider(url).list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].ids, ["local-model:latest"]);
    assert!(!models[0].features.thinking.supported);
    assert_eq!(models[0].config.context_window, None);
    let requests = task.await.unwrap();
    assert!(requests[0].0.starts_with("get /v1/models "));
    assert!(requests[0].1.is_none());
}

#[tokio::test]
async fn listing_attaches_only_matching_catalog_metadata() {
    let (url, task) = server(200, vec![r#"{"data":[{"id":"local-model:latest"}]}"#]).await;
    let info = model().info;
    let mut catalog = ProviderCatalog::default();
    catalog.models.insert(
        "local-model:latest".into(),
        CatalogModel::builder()
            .name("Local model")
            .info(info.clone())
            .metadata(json!({}))
            .build(),
    );
    catalog
        .models
        .insert("absent".into(), CatalogModel::default());
    let provider = Provider::new(Ollama::new(
        OllamaConfig::builder()
            .base_url(url)
            .catalog(catalog)
            .build(),
    ));
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].features, info.features);
    assert_eq!(models[0].config, info.config);
    task.await.unwrap();
}

#[tokio::test]
async fn off_is_explicit_without_metadata_and_replays_reasoning_only_history() {
    let (url, task) = server(200, vec![TEXT]).await;
    let model = Model::builder()
        .id("unknown")
        .provider(provider("http://127.0.0.1:1/v1".into()))
        .info(ModelInfo::default())
        .build();
    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "Prior reasoning".into(),
            signature: None,
        }])
        .build()];
    let (tx, _rx) = flume::unbounded();
    provider(url)
        .stream_message(&model, &messages, "", &[], &tx, RequestOptions::default())
        .await
        .unwrap();
    let requests = task.await.unwrap();
    let body = requests[0].1.as_ref().unwrap();
    assert_eq!(body["reasoning_effort"], "none");
    assert_eq!(body["messages"][0]["content"], "");
    assert_eq!(body["messages"][0]["reasoning"], "Prior reasoning");
    assert!(body.get("max_tokens").is_none());
}

#[tokio::test]
async fn signed_reasoning_is_rejected_before_transport() {
    use animist::protocol::{ProtocolError, openai::OpenAIProtocolError};

    let provider = provider("http://127.0.0.1:1/v1".into());
    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "Other protocol".into(),
            signature: Some("opaque".into()),
        }])
        .build()];
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(&model(), &messages, "", &[], &tx, RequestOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::UnsupportedChatContent,
            },
        }
    ));
}

#[tokio::test]
async fn adaptive_leaves_server_default_thinking_unchanged() {
    let (url, task) = server(200, vec![TEXT]).await;
    let (tx, _rx) = flume::unbounded();
    provider(url)
        .stream_message(
            &model(),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Adaptive)
                .fast(false)
                .build(),
        )
        .await
        .unwrap();
    assert!(
        task.await.unwrap()[0]
            .1
            .as_ref()
            .unwrap()
            .get("reasoning_effort")
            .is_none()
    );
}

#[tokio::test]
async fn rejects_unsupported_policy_before_sending() {
    let provider = provider("http://127.0.0.1:1/v1".into());
    let (tx, _rx) = flume::unbounded();
    for thinking in [
        ThinkingConfig::Budget(100),
        ThinkingConfig::Effort(Effort::High),
    ] {
        let error = provider
            .stream_message(
                &model(),
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
        assert!(matches!(
            error,
            ProviderError::ReasoningBudgetUnsupported
                | ProviderError::UnsupportedReasoningEffort { .. }
        ));
    }
    let error = provider
        .stream_message(
            &model(),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Off)
                .fast(true)
                .build(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::FastModeUnsupported { .. }));
    let mut required = model();
    required.overrides.thinking = Some(ThinkingSupport::Required);
    assert!(matches!(
        provider
            .stream_message(&required, &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err(),
        ProviderError::ReasoningRequired
    ));
    let mut wrong = model();
    wrong.provider = Provider::new(
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
            .stream_message(&wrong, &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err(),
        ProviderError::ModelProviderMismatch { .. }
    ));
    assert!(matches!(
        provider
            .stream_message(
                &model(),
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Off)
                    .fast(false)
                    .max_output_tokens(0_u32)
                    .build()
            )
            .await
            .unwrap_err(),
        ProviderError::ZeroOutputTokenLimit
    ));
}

#[tokio::test]
async fn preserves_http_error_body() {
    let body = r#"{"error":{"message":"model not found"}}"#;
    let (url, task) = server(404, vec![body]).await;
    let (tx, _rx) = flume::unbounded();
    let error = provider(url)
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Http { status: 404, body: payload } if payload == body));
    task.await.unwrap();
}
