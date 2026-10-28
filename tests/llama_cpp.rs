#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::time::Duration;

use animist::{
    catalog::{CatalogModel, ProviderCatalog},
    history::{ContentBlock, Message, MessageKind, Role},
    protocol::{ProtocolError, openai::OpenAIProtocolError},
    provider::{
        Effort, Model, ModelInfo, Provider, ProviderError, ProviderEvent, RequestOptions,
        StopReason, ThinkingConfig, ThinkingSupport, Tool,
        llama_cpp::{LlamaCpp, LlamaCppConfig},
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
const TOOLS: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Check 🦀\"}}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-1\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"path\\\":\\\"src\\\"}\"}}]},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":5}}\n\ndata: [DONE]\n\n";

async fn server(
    status: u16,
    bodies: Vec<&str>,
) -> (String, JoinHandle<Vec<(String, Option<Value>)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/prefix/v1/", listener.local_addr().unwrap());
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
        .id("org/model:Q4_K_M")
        .info(info)
        .build()
}

fn provider(url: String) -> Provider {
    Provider::new(
        LlamaCpp::new(LlamaCppConfig::builder().base_url(url).build())
            .unwrap()
            .with_client(reqwest::Client::new()),
    )
}

#[test]
fn config_defaults_and_credentials_are_safe_to_serialize() {
    let config: LlamaCppConfig = serde_json::from_value(json!({})).unwrap();
    assert!(config == LlamaCppConfig::default());
    assert!(config == LlamaCppConfig::builder().build());
    assert_eq!(config.base_url, "http://localhost:9931/v1");
    assert_eq!(config.api_key_env, None);
    assert!(
        serde_json::from_value::<LlamaCppConfig>(serde_json::to_value(&config).unwrap()).unwrap()
            == config
    );
    let config = LlamaCppConfig::builder().api_key("not-a-real-key").build();
    let value = serde_json::to_value(config).unwrap();
    assert!(value.get("api_key").is_none());
    assert!(!value.to_string().contains("not-a-real-key"));
    assert!(LlamaCpp::new(LlamaCppConfig::default()).is_ok());
    for key in ["", "   ", "invalid\nheader"] {
        assert!(LlamaCpp::new(LlamaCppConfig::builder().api_key(key).build()).is_err());
    }
    let error = LlamaCpp::new(
        LlamaCppConfig::builder()
            .api_key_env("ANIMIST_TEST_LLAMA_CPP_MISSING_CREDENTIAL_7B962A")
            .build(),
    );
    assert!(matches!(
        error,
        Err(ProviderError::CredentialEnvironment { .. })
    ));
}

#[tokio::test]
async fn routes_model_ids_and_replays_thinking_through_a_tool_loop() {
    let (url, task) = server(200, vec![TOOLS, TEXT]).await;
    let provider = provider(url);
    assert_eq!(provider.name(), "llama-cpp");
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    let tool = Tool::builder()
        .name("lookup")
        .description("Look up a path")
        .input_schema(json!({"type":"object","properties":{"path":{"type":"string"}}}))
        .build();
    let (tx, rx) = flume::unbounded();
    let response = provider
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
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(response.usage.input, 10);
    assert_eq!(response.usage.output, 5);
    assert!(matches!(&response.message.content[0],
        ContentBlock::Thinking { thinking, signature: None } if thinking == "Check 🦀"));
    assert!(matches!(&response.message.content[1],
        ContentBlock::ToolUse { id, input, .. } if id == "call-1" && input == &json!({"path":"src"})));
    assert!(
        rx.drain().any(
            |event| matches!(event, ProviderEvent::ThinkingDelta { text } if text == "Check 🦀")
        )
    );
    let messages = [
        response.message,
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
    ];
    provider
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
    let requests = task.await.unwrap();
    for (headers, body) in &requests {
        assert!(headers.starts_with("post /prefix/v1/chat/completions "));
        assert!(!headers.contains("authorization:"));
        let body = body.as_ref().unwrap();
        assert_eq!(body["model"], "org/model:Q4_K_M");
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("service_tier").is_none());
        assert!(body.get("reasoning_budget_tokens").is_none());
        assert_eq!(body["stream_options"]["include_usage"], true);
    }
    let first = requests[0].1.as_ref().unwrap();
    assert_eq!(first["max_tokens"], 512);
    assert_eq!(first["reasoning_effort"], "max");
    let second = requests[1].1.as_ref().unwrap();
    assert_eq!(second["max_tokens"], 128);
    assert_eq!(second["reasoning_effort"], "none");
    assert_eq!(second["messages"][1]["reasoning_content"], "Check 🦀");
    assert!(second["messages"][1].get("reasoning").is_none());
    assert_eq!(second["messages"][2]["tool_call_id"], "call-1");
}

#[tokio::test]
async fn lists_unloaded_router_models_with_optional_catalog_metadata() {
    let (url, task) = server(200, vec![r#"{"data":[{"id":"org/model:Q4_K_M","status":{"value":"unloaded"}},{"id":"unknown","status":{"value":"loaded"}}]}"#]).await;
    let info = model().info;
    let mut catalog = ProviderCatalog::default();
    catalog.models.insert(
        "org/model:Q4_K_M".into(),
        CatalogModel::builder()
            .name("Local model")
            .info(info.clone())
            .metadata(json!({}))
            .build(),
    );
    catalog
        .models
        .insert("absent".into(), CatalogModel::default());
    let provider = Provider::new(
        LlamaCpp::new(
            LlamaCppConfig::builder()
                .base_url(url)
                .catalog(catalog)
                .build(),
        )
        .unwrap(),
    );
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].ids, ["org/model:Q4_K_M"]);
    assert_eq!(models[0].features, info.features);
    assert_eq!(models[0].config, info.config);
    assert!(!models[1].features.thinking.supported);
    assert_eq!(models[1].config.context_window, None);
    let requests = task.await.unwrap();
    assert!(requests[0].0.starts_with("get /prefix/v1/models "));
    assert!(requests[0].1.is_none());
}

#[tokio::test]
async fn explicit_credentials_apply_to_discovery_and_inference() {
    let (url, task) = server(200, vec![r#"{"data":[]}"#, TEXT]).await;
    let provider = Provider::new(
        LlamaCpp::new(
            LlamaCppConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .api_key_env("ANIMIST_TEST_LLAMA_CPP_MISSING_CREDENTIAL_7B962A")
                .build(),
        )
        .unwrap(),
    );
    provider.reload_auth().await.unwrap();
    provider.list_models().await.unwrap();
    let (tx, _rx) = flume::unbounded();
    provider
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap();
    for (headers, _) in task.await.unwrap() {
        assert!(headers.contains("authorization: bearer test-key"));
    }
}

#[tokio::test]
async fn environment_credentials_can_be_loaded_and_reloaded() {
    const VARIABLE: &str = "ANIMIST_TEST_LLAMA_CPP_CHILD_KEY";
    if std::env::var_os(VARIABLE).is_none() {
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "environment_credentials_can_be_loaded_and_reloaded",
            ])
            .env(VARIABLE, "test-env-key")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (url, task) = server(200, vec![r#"{"data":[]}"#, r#"{"data":[]}"#]).await;
    let provider = Provider::new(
        LlamaCpp::new(
            LlamaCppConfig::builder()
                .base_url(url)
                .api_key_env(VARIABLE)
                .build(),
        )
        .unwrap(),
    );
    provider.list_models().await.unwrap();
    provider.reload_auth().await.unwrap();
    provider.list_models().await.unwrap();
    for (headers, _) in task.await.unwrap() {
        assert!(headers.contains("authorization: bearer test-env-key"));
    }
}

#[tokio::test]
async fn thinking_modes_and_reasoning_only_history_use_native_policy() {
    let (url, task) = server(200, vec![TEXT, TEXT, TEXT, TEXT]).await;
    let provider = provider(url);
    let (tx, _rx) = flume::unbounded();
    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "Prior reasoning".into(),
            signature: None,
        }])
        .build()];
    let unknown = Model::builder()
        .provider(provider.clone())
        .id("unknown")
        .info(ModelInfo::default())
        .build();
    provider
        .stream_message(&unknown, &messages, "", &[], &tx, RequestOptions::default())
        .await
        .unwrap();
    for thinking in [
        ThinkingConfig::Adaptive,
        ThinkingConfig::Budget(0),
        ThinkingConfig::Budget(2048),
    ] {
        provider
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
            .unwrap();
    }
    let requests = task.await.unwrap();
    let off = requests[0].1.as_ref().unwrap();
    assert_eq!(off["reasoning_effort"], "none");
    assert_eq!(off["messages"][0]["reasoning_content"], "Prior reasoning");
    assert!(off.get("max_tokens").is_none());
    let adaptive = requests[1].1.as_ref().unwrap();
    assert!(adaptive.get("reasoning_effort").is_none());
    assert!(adaptive.get("reasoning_budget_tokens").is_none());
    for (index, budget) in [(2, 0), (3, 2048)] {
        let body = requests[index].1.as_ref().unwrap();
        assert_eq!(body["reasoning_budget_tokens"], budget);
        assert!(body.get("reasoning_effort").is_none());
    }
}

#[tokio::test]
async fn policy_and_unreplayable_content_fail_before_transport() {
    let provider = provider("http://127.0.0.1:1/v1".into());
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(
            &model(),
            &[],
            "",
            &[],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Effort(Effort::High))
                .fast(false)
                .build(),
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        ProviderError::UnsupportedReasoningEffort { .. }
    ));
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
        ProviderError::ReasoningRequired { .. }
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
    for (thinking, limit) in [
        (ThinkingConfig::Off, Some(0_u32)),
        (ThinkingConfig::Budget(u32::MAX), None),
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
                    .maybe_max_output_tokens(limit)
                    .build(),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ProviderError::ZeroOutputTokenLimit { .. }
                | ProviderError::LlamaCppThinkingBudgetOutOfRange { .. }
        ));
    }
    let mut disabled = model();
    disabled.overrides.thinking = Some(ThinkingSupport::No);
    assert!(matches!(
        provider
            .stream_message(
                &disabled,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Budget(1024))
                    .fast(false)
                    .build()
            )
            .await
            .unwrap_err(),
        ProviderError::ThinkingUnsupported { .. }
    ));
    let messages = [Message::builder()
        .role(Role::Assistant)
        .kind(MessageKind::Turn)
        .content(vec![ContentBlock::Thinking {
            thinking: "Other protocol".into(),
            signature: Some("opaque".into()),
        }])
        .build()];
    assert!(matches!(
        provider
            .stream_message(&model(), &messages, "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err(),
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::UnsupportedChatContent { .. }
            },
        }
    ));
}

#[tokio::test]
async fn preserves_http_errors_and_rejects_truncated_streams() {
    let (url, task) = server(503, vec![r#"{"error":{"message":"model load failed"}}"#]).await;
    let (tx, _rx) = flume::unbounded();
    let error = provider(url)
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Http { status: 503, body ,
    ..}
        if body.contains("model load failed")));
    task.await.unwrap();
    let (url, task) = server(200, vec!["data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Partial\"},\"finish_reason\":\"stop\"}]}\n\n"]).await;
    assert!(matches!(
        provider(url)
            .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
            .await
            .unwrap_err(),
        ProviderError::Protocol {
            source: ProtocolError::OpenAI {
                source: OpenAIProtocolError::TruncatedChatStream { .. }
            },
        }
    ));
    task.await.unwrap();
}

#[test]
fn native_budget_is_opt_in_for_the_shared_adapter() {
    use animist::protocol::{
        Protocol, ProtocolInput,
        openai::{ChatCompletions, OpenAiOptions},
    };
    let model = model();
    let options = RequestOptions::default();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages([].as_slice())
        .system("")
        .tools([].as_slice())
        .options(&options)
        .build();
    let body = ChatCompletions
        .request(input, &OpenAiOptions::default())
        .unwrap();
    assert!(body.get("reasoning_budget_tokens").is_none());
}
