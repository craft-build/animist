#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::time::Duration;

use animist::{
    catalog::Catalog,
    history::{ContentBlock, Message, MessageKind, Role},
    protocol::{ProtocolError, openai::OpenAIProtocolError},
    provider::{
        Effort, Model, ModelInfo, Provider, ProviderError, ProviderEvent, ProviderImpl,
        RequestOptions, StopReason, ThinkingConfig, ThinkingSupport, Tool,
        zai_coding_plan::{ZaiCodingPlan, ZaiCodingPlanConfig},
    },
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const TOOLS: &str = include_str!("fixtures/zai-coding-plan.sse");
const TEXT: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Done\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\ndata: [DONE]\n\n";

#[test]
fn shared_protocol_defaults_do_not_send_coding_plan_extensions() {
    use animist::protocol::{
        Protocol, ProtocolInput,
        openai::{ChatCompletions, OpenAiOptions},
    };

    let model = model();
    let options = RequestOptions::builder()
        .thinking(ThinkingConfig::Off)
        .fast(false)
        .max_output_tokens(100_u32)
        .build();
    let input = ProtocolInput::builder()
        .model(&model)
        .messages([].as_slice())
        .system("")
        .tools([].as_slice())
        .options(&options)
        .build();
    assert_eq!(OpenAiOptions::builder().build(), OpenAiOptions::default());
    let body = ChatCompletions
        .request(input, &OpenAiOptions::default())
        .unwrap();
    assert_eq!(body["max_completion_tokens"], 100);
    for field in ["thinking", "tool_stream", "max_tokens"] {
        assert!(body.get(field).is_none());
    }
}

#[tokio::test]
async fn dropping_the_future_cancels_the_coding_plan_exchange() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .api_key("test-key")
            .base_url(format!(
                "http://{}/api/coding/paas/v4",
                listener.local_addr().unwrap()
            ))
            .build(),
    )
    .unwrap();
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        loop {
            let mut buffer = [0; 4096];
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8(request[..end].to_vec())
                    .unwrap()
                    .to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length: ")
                            .map(|value| value.parse::<usize>().unwrap())
                    })
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n").await.unwrap();
        ready_tx.send(()).unwrap();
        let mut buffer = [0];
        match timeout(Duration::from_secs(5), socket.read(&mut buffer))
            .await
            .unwrap()
        {
            Ok(0) => {}
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            result => panic!("expected a closed connection, got {result:?}"),
        }
    });
    let (tx, _rx) = flume::unbounded();
    let model = model();
    let mut future =
        Box::pin(provider.stream_message(&model, &[], "", &[], &tx, RequestOptions::default()));
    tokio::select! {
        result = &mut future => panic!("request unexpectedly finished: {result:?}"),
        _ = ready_rx => {}
    }
    drop(future);
    timeout(Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
}

async fn server(status: u16, bodies: Vec<&str>) -> (String, JoinHandle<Vec<(String, Value)>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/api/coding/paas/v4",
        listener.local_addr().unwrap()
    );
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
                    if let Some(end) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                        let headers = String::from_utf8(bytes[..end].to_vec()).unwrap().to_lowercase();
                        let length = headers.lines().find_map(|line|
                            line.strip_prefix("content-length: ").map(|value| value.parse::<usize>().unwrap())).unwrap();
                        if bytes.len() >= end + 4 + length {
                            requests.push((headers, serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap()));
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
    info.features.thinking.efforts = vec![Effort::Low, Effort::High, Effort::Max];
    Model::builder()
        .provider(Provider::new(
            ZaiCodingPlan::new(ZaiCodingPlanConfig::builder().api_key("test-key").build()).unwrap(),
        ))
        .id("glm-5.3")
        .info(info)
        .build()
}

#[test]
fn config_defaults_round_trip_without_serializing_credentials() {
    let config: ZaiCodingPlanConfig = serde_json::from_value(json!({})).unwrap();
    assert!(config == ZaiCodingPlanConfig::default());
    assert!(config == ZaiCodingPlanConfig::builder().build());
    assert_eq!(config.api_key_env, "ZHIPU_API_KEY");
    assert_eq!(config.base_url, "https://api.z.ai/api/coding/paas/v4");
    let config = ZaiCodingPlanConfig::builder()
        .api_key("not-a-real-key")
        .build();
    let value = serde_json::to_value(config).unwrap();
    assert!(value.get("api_key").is_none());
    assert!(!value.to_string().contains("not-a-real-key"));
    assert!(
        serde_json::from_value::<ZaiCodingPlanConfig>(value).unwrap()
            == ZaiCodingPlanConfig::default()
    );
}

#[tokio::test]
async fn coding_endpoint_preserves_reasoning_through_streaming_tool_round_trips() {
    let (url, task) = server(200, vec![TOOLS, TEXT]).await;
    let provider = Provider::new(
        ZaiCodingPlan::new(
            ZaiCodingPlanConfig::builder()
                .base_url(url)
                .api_key("test-key")
                .build(),
        )
        .unwrap()
        .with_client(reqwest::Client::new()),
    );
    assert_eq!(provider.name(), "zai-coding-plan");
    let tool = Tool::builder()
        .name("lookup")
        .description("Look up a path")
        .input_schema(json!({"type":"object","properties":{"path":{"type":"string"}}}))
        .build();
    let mut messages = vec![
        Message::builder()
            .role(Role::User)
            .kind(MessageKind::Turn)
            .content(vec![ContentBlock::Text {
                text: "Inspect src".into(),
            }])
            .build(),
    ];
    let (tx, rx) = flume::unbounded();
    let response = provider
        .stream_message(
            &model(),
            &messages,
            "system",
            std::slice::from_ref(&tool),
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Adaptive)
                .fast(false)
                .max_output_tokens(512_u32)
                .build(),
        )
        .await
        .unwrap();
    assert_eq!(response.stop_reason, Some(StopReason::ToolUse));
    assert_eq!(response.usage.input, 60);
    assert_eq!(response.usage.cache_read, 40);
    assert_eq!(response.usage.output, 30);
    assert!(
        matches!(&response.message.content[0], ContentBlock::Thinking { thinking, signature:None }
        if thinking == "Check the repository 🦀.")
    );
    assert!(
        matches!(&response.message.content[1], ContentBlock::ToolUse { id, input, .. }
        if id == "call-1" && input == &json!({"path":"src"}))
    );
    assert_eq!(
        rx.drain()
            .filter(|event| matches!(event, ProviderEvent::ThinkingDelta { .. }))
            .count(),
        2
    );
    messages.push(response.message);
    messages.push(
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
    );
    provider
        .stream_message(
            &model(),
            &messages,
            "system",
            &[tool],
            &tx,
            RequestOptions::builder()
                .thinking(ThinkingConfig::Effort(Effort::Max))
                .fast(false)
                .build(),
        )
        .await
        .unwrap();
    let requests = task.await.unwrap();
    for (headers, body) in &requests {
        assert!(headers.starts_with("post /api/coding/paas/v4/chat/completions "));
        assert!(headers.contains("authorization: bearer test-key"));
        assert!(!headers.contains("anthropic-version"));
        assert_eq!(
            body["thinking"],
            json!({"type":"enabled","clear_thinking":false})
        );
        assert_eq!(body["tool_stream"], true);
        assert!(body.get("max_completion_tokens").is_none());
        assert!(body.get("service_tier").is_none());
        for field in [
            "reasoning_budget_tokens",
            "enable_thinking",
            "thinking_budget",
        ] {
            assert!(body.get(field).is_none(), "unexpected field: {field}");
        }
    }
    assert_eq!(requests[0].1["max_tokens"], 512);
    assert_eq!(requests[1].1["reasoning_effort"], "max");
    assert_eq!(
        requests[1].1["messages"][2]["reasoning_content"],
        "Check the repository 🦀."
    );
    assert!(requests[1].1["messages"][2].get("reasoning").is_none());
    assert_eq!(requests[1].1["messages"][2]["content"], Value::Null);
    assert_eq!(requests[1].1["messages"][3]["role"], "tool");
    assert_eq!(requests[1].1["messages"][3]["tool_call_id"], "call-1");
}

#[tokio::test]
async fn thinking_toggle_effort_and_output_precedence_are_explicit() {
    let (url, task) = server(200, vec![TEXT, TEXT, TEXT]).await;
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .base_url(url)
            .api_key("test-key")
            .max_output_tokens(333_u32)
            .build(),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    let mut model = model();
    for (index, thinking) in [
        ThinkingConfig::Off,
        ThinkingConfig::Effort(Effort::High),
        ThinkingConfig::Adaptive,
    ]
    .into_iter()
    .enumerate()
    {
        if index > 0 {
            model.info.config.max_output_tokens = Some(222);
        }
        let options = RequestOptions::builder()
            .thinking(thinking)
            .fast(false)
            .maybe_max_output_tokens((index == 2).then_some(111_u32))
            .build();
        provider
            .stream_message(&model, &[], "", &[], &tx, options)
            .await
            .unwrap();
    }
    let requests = task.await.unwrap();
    assert_eq!(requests[0].1["thinking"]["type"], "disabled");
    assert!(requests[0].1.get("reasoning_effort").is_none());
    assert_eq!(requests[1].1["reasoning_effort"], "high");
    for (index, limit) in [333, 222, 111].into_iter().enumerate() {
        assert_eq!(requests[index].1["max_tokens"], limit);
        assert!(requests[index].1.get("tool_stream").is_none());
    }
}

#[tokio::test]
async fn catalog_listing_is_explicit_and_does_not_make_network_requests() {
    let catalog = Catalog::from_slice(&serde_json::to_vec(&json!({"zai-coding-plan":{
        "id":"zai-coding-plan","name":"Z.AI Coding Plan","models":{"glm-5.3":{
            "name":"GLM","reasoning":true,"reasoning_options":[{"type":"effort","values":["low","high","max"]}],
            "limit":{"context":1_000_000,"output":131_072},"extra_source_data":"preserved"
        }}
    }})).unwrap()).unwrap();
    let source = catalog.provider("zai-coding-plan").unwrap().clone();
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .api_key("test-key")
            .base_url("http://127.0.0.1:1")
            .catalog(source.clone())
            .build(),
    )
    .unwrap();
    let models = provider.list_models().await.unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0], source.models["glm-5.3"].info);
    assert!(!models[0].features.thinking.required);
    let provider =
        ZaiCodingPlan::new(ZaiCodingPlanConfig::builder().api_key("test-key").build()).unwrap();
    assert_eq!(provider.list_models().await.unwrap(), Vec::new());
}

#[tokio::test]
async fn credentials_and_unsupported_policy_fail_before_transport() {
    assert!(matches!(
        ZaiCodingPlan::new(ZaiCodingPlanConfig::builder().api_key("").build()),
        Err(ProviderError::EmptyApiKey { .. })
    ));
    assert!(matches!(
        ZaiCodingPlan::new(ZaiCodingPlanConfig::builder().api_key("bad\nkey").build()),
        Err(ProviderError::CredentialHeader { .. })
    ));
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .api_key("test-key")
            .api_key_env("ANIMIST_ZAI_UNUSED_KEY")
            .base_url("http://127.0.0.1:1")
            .build(),
    )
    .unwrap();
    provider.refresh_auth().await.unwrap();
    provider.reload_auth().await.unwrap();
    let (tx, _rx) = flume::unbounded();
    let mut model = model();
    for thinking in [
        ThinkingConfig::Budget(1024),
        ThinkingConfig::Effort(Effort::Medium),
    ] {
        let error = provider
            .stream_message(
                &model,
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
            ProviderError::ReasoningBudgetUnsupported { .. }
                | ProviderError::UnsupportedReasoningEffort { .. }
        ));
    }
    model.overrides.thinking = Some(ThinkingSupport::Required);
    assert!(matches!(
        provider
            .stream_message(&model, &[], "", &[], &tx, RequestOptions::default())
            .await,
        Err(ProviderError::ReasoningRequired { .. })
    ));
    model.overrides.thinking = Some(ThinkingSupport::No);
    assert!(matches!(
        provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Adaptive)
                    .fast(false)
                    .build()
            )
            .await,
        Err(ProviderError::ThinkingUnsupported { .. })
    ));
    assert!(matches!(
        provider
            .stream_message(
                &model,
                &[],
                "",
                &[],
                &tx,
                RequestOptions::builder()
                    .thinking(ThinkingConfig::Off)
                    .fast(true)
                    .build()
            )
            .await,
        Err(ProviderError::FastModeUnsupported { .. })
    ));
}

#[tokio::test]
async fn failures_keep_http_payloads_and_protocol_error_types() {
    let (url, task) = server(
        429,
        vec!["{\"error\":{\"code\":\"1302\",\"message\":\"limit\"}}"],
    )
    .await;
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .base_url(url)
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let (tx, _rx) = flume::unbounded();
    let error = provider
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Http { status:429, body ,
    ..}
        if serde_json::from_str::<Value>(&body).unwrap()["error"]["code"] == "1302"));
    task.await.unwrap();
    let (url, task) = server(
        200,
        vec!["data: {\"error\":{\"code\":\"stream-error\"}}\n\n"],
    )
    .await;
    let provider = ZaiCodingPlan::new(
        ZaiCodingPlanConfig::builder()
            .base_url(url)
            .api_key("test-key")
            .build(),
    )
    .unwrap();
    let error = provider
        .stream_message(&model(), &[], "", &[], &tx, RequestOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Protocol { source:ProtocolError::OpenAI {
        source:OpenAIProtocolError::Api { error ,
        ..},
    }} if error["code"] == "stream-error")
    );
    task.await.unwrap();
}
