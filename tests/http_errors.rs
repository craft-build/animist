#![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

use std::time::Duration;

use animist::provider::{
    Model, ModelInfo, Provider, ProviderError, RequestOptions,
    anthropic::{Anthropic, AnthropicConfig},
    llama_cpp::{LlamaCpp, LlamaCppConfig},
    ollama::{Ollama, OllamaConfig},
    omlx::{Omlx, OmlxConfig},
    openai::{OpenAi, OpenAiConfig},
    synthetic::{Synthetic, SyntheticConfig},
    zai_coding_plan::{ZaiCodingPlan, ZaiCodingPlanConfig},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
    time::timeout,
};

const LIMIT: usize = 64 * 1024;
const PROVIDERS: [&str; 7] = [
    "anthropic",
    "llama-cpp",
    "ollama",
    "omlx",
    "openai",
    "synthetic",
    "zai-coding-plan",
];

fn provider(name: &str, url: &str) -> Provider {
    match name {
        "anthropic" => Provider::new(
            Anthropic::new(
                AnthropicConfig::builder()
                    .base_url(url)
                    .api_key("test")
                    .build(),
            )
            .unwrap(),
        ),
        "llama-cpp" => {
            Provider::new(LlamaCpp::new(LlamaCppConfig::builder().base_url(url).build()).unwrap())
        }
        "ollama" => Provider::new(Ollama::new(OllamaConfig::builder().base_url(url).build())),
        "omlx" => Provider::new(Omlx::new(OmlxConfig::builder().base_url(url).build()).unwrap()),
        "openai" => Provider::new(
            OpenAi::new(
                OpenAiConfig::builder()
                    .base_url(url)
                    .api_key("test")
                    .build(),
            )
            .unwrap(),
        ),
        "synthetic" => Provider::new(
            Synthetic::new(
                SyntheticConfig::builder()
                    .base_url(url)
                    .api_key("test")
                    .build(),
            )
            .unwrap(),
        ),
        "zai-coding-plan" => Provider::new(
            ZaiCodingPlan::new(
                ZaiCodingPlanConfig::builder()
                    .base_url(url)
                    .api_key("test")
                    .build(),
            )
            .unwrap(),
        ),
        _ => panic!("unknown provider {name}"),
    }
}

async fn server(body: Vec<u8>, unfinished: bool, known_length: bool) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse::<usize>()
                    .unwrap();
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        let length_header = if known_length {
            format!(
                "content-length: {}\r\n",
                body.len() + usize::from(unfinished)
            )
        } else {
            String::new()
        };
        socket
            .write_all(
                format!(
                    "HTTP/1.1 503 Service Unavailable\r\n{length_header}connection: close\r\n\r\n"
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        for chunk in body.chunks(1021) {
            if socket.write_all(chunk).await.is_err() {
                return;
            }
        }
        if unfinished {
            assert_eq!(
                timeout(Duration::from_secs(5), socket.read(&mut buffer))
                    .await
                    .unwrap()
                    .unwrap(),
                0,
                "client must drop the unread response"
            );
        }
    });
    (url, task)
}

async fn error_body(
    name: &str,
    body: Vec<u8>,
    unfinished: bool,
    known_length: bool,
) -> (String, bool) {
    let (url, task) = server(body, unfinished, known_length).await;
    let provider = provider(name, &url);
    let model = Model::builder()
        .id("fixture")
        .provider(provider.clone())
        .info(ModelInfo::default())
        .build();
    let (events, _receiver) = flume::unbounded();
    let error = timeout(
        Duration::from_secs(5),
        provider.stream_message(&model, &[], "", &[], &events, RequestOptions::default()),
    )
    .await
    .expect("error reader must not wait for an oversized body to finish")
    .unwrap_err();
    timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap();
    let display = error.to_string();
    match error {
        ProviderError::Http {
            status: 503,
            body,
            body_truncated,
            ..
        } => {
            assert!(body.len() <= LIMIT, "{name}");
            assert!(body.capacity() <= LIMIT, "{name}: {}", body.capacity());
            assert_eq!(
                display.contains("(body capped at 64 KiB; may be incomplete)"),
                body_truncated,
                "{name}"
            );
            if !body_truncated {
                assert_eq!(display, format!("Provider returned HTTP 503: {body}"));
            }
            (body, body_truncated)
        }
        other => panic!("{name}: unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn all_http_providers_bound_error_prefixes_and_retained_allocations() {
    for name in PROVIDERS {
        assert_eq!(
            error_body(name, b"small error".to_vec(), false, true).await,
            ("small error".into(), false)
        );
        assert_eq!(
            error_body(name, vec![b'x'; LIMIT], false, true).await,
            ("x".repeat(LIMIT), false)
        );
        assert_eq!(
            error_body(name, vec![b'x'; 2 * 1024 * 1024], false, true).await,
            ("x".repeat(LIMIT), true)
        );
    }
}

#[tokio::test]
async fn all_http_providers_stop_at_the_limit_without_waiting_for_eof() {
    for name in PROVIDERS {
        assert_eq!(
            error_body(name, vec![b'x'; LIMIT], true, true).await,
            ("x".repeat(LIMIT), true)
        );
    }
}

#[tokio::test]
async fn all_http_providers_keep_error_prefixes_utf8_safe() {
    for name in PROVIDERS {
        let body = format!("{}é", "x".repeat(LIMIT - 1)).into_bytes();
        assert_eq!(
            error_body(name, body, false, true).await,
            ("x".repeat(LIMIT - 1), true)
        );
        assert_eq!(
            error_body(name, vec![0xff; LIMIT], false, true).await,
            ("�".repeat(LIMIT / 3), true)
        );
        assert_eq!(
            error_body(name, "短いエラー".as_bytes().to_vec(), false, true).await,
            ("短いエラー".into(), false)
        );
    }
}

#[tokio::test]
async fn unknown_lengths_are_flagged_conservatively_at_the_limit() {
    for name in PROVIDERS {
        assert_eq!(
            error_body(name, b"small error".to_vec(), false, false).await,
            ("small error".into(), false)
        );
        assert_eq!(
            error_body(name, vec![b'x'; LIMIT], false, false).await,
            ("x".repeat(LIMIT), true)
        );
        assert_eq!(
            error_body(name, vec![b'x'; LIMIT], true, false).await,
            ("x".repeat(LIMIT), true)
        );
    }
}
