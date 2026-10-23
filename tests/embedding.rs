use animist::{
    protocol::{EmbeddingProtocol, Result as ProtocolResult},
    provider::{
        EmbeddingModel, EmbeddingProvider, EmbeddingProviderImpl, EmbeddingRequest,
        EmbeddingResponse, Result,
    },
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{num::NonZeroU32, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};

struct LocalProtocol;

#[async_trait]
impl EmbeddingProtocol for LocalProtocol {
    type Request = Vec<String>;
    type Response = Vec<Vec<f32>>;

    fn request(
        &self,
        _model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> ProtocolResult<Self::Request> {
        Ok(request.input.clone())
    }

    async fn assemble(
        &self,
        response: Self::Response,
        _model: &EmbeddingModel,
        _request: &EmbeddingRequest,
    ) -> ProtocolResult<EmbeddingResponse> {
        Ok(EmbeddingResponse::builder()
            .model("local")
            .embeddings(response)
            .build())
    }
}

struct LocalProvider;

#[async_trait]
impl EmbeddingProviderImpl for LocalProvider {
    fn name(&self) -> &str {
        "local"
    }

    async fn embed(
        &self,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse> {
        let input = LocalProtocol.request(model, request)?;
        Ok(LocalProtocol
            .assemble(input.iter().map(|_| vec![1.0]).collect(), model, request)
            .await?)
    }

    async fn refresh_auth(&self) -> Result<()> {
        Ok(())
    }

    async fn reload_auth(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn downstream_embedding_provider_and_non_http_protocol_use_public_types() {
    let provider = EmbeddingProvider::new(LocalProvider).clone();
    let model = EmbeddingModel::builder()
        .id("local")
        .provider("local")
        .build();
    let request = EmbeddingRequest::builder()
        .input(vec!["one".into(), "two".into()])
        .build();
    let response = provider.embed(&model, &request).await.unwrap();
    assert_eq!(provider.name(), "local");
    assert_eq!(response.embeddings, [vec![1.0], vec![1.0]]);
    assert!(response.usage.is_none());
}

#[test]
fn expected_dimensions_round_trip_and_conflicts_fail_before_translation() {
    use animist::protocol::{
        ProtocolError,
        openai::{Embeddings, OpenAIProtocolError},
    };

    let legacy: EmbeddingModel = serde_json::from_value(json!({
        "id": "embedding", "provider": "synthetic"
    }))
    .unwrap();
    assert!(legacy.expected_dimensions.is_none());
    assert_eq!(
        legacy,
        EmbeddingModel::builder()
            .id("embedding")
            .provider("synthetic")
            .build()
    );
    assert!(
        serde_json::from_value::<EmbeddingModel>(json!({
            "id": "embedding", "provider": "synthetic", "expected_dimensions": 0
        }))
        .is_err()
    );
    let model = EmbeddingModel::builder()
        .id("embedding")
        .provider("synthetic")
        .expected_dimensions(NonZeroU32::new(2).unwrap())
        .build();
    let restored =
        serde_json::from_value::<EmbeddingModel>(serde_json::to_value(&model).unwrap()).unwrap();
    assert_eq!(model, restored);
    let request = EmbeddingRequest::builder()
        .input(vec!["text".into()])
        .build();
    let body = Embeddings.request(&model, &request).unwrap();
    assert!(body.get("dimensions").is_none());
    let request = EmbeddingRequest::builder()
        .input(vec!["text".into()])
        .dimensions(3_u32)
        .build();
    assert!(matches!(
        Embeddings.request(&model, &request),
        Err(ProtocolError::OpenAI {
            source: OpenAIProtocolError::EmbeddingDimensionConflict {
                expected: 2,
                requested: 3
            }
        })
    ));
}

fn local_provider(name: &str, url: &str, authenticated: bool) -> EmbeddingProvider {
    use animist::provider::{
        Provider,
        llama_cpp::{LlamaCpp, LlamaCppConfig},
        ollama::{Ollama, OllamaConfig},
        omlx::{Omlx, OmlxConfig},
    };

    let client = reqwest::Client::builder()
        .default_headers(
            [(
                reqwest::header::HeaderName::from_static("x-test-client"),
                reqwest::header::HeaderValue::from_static("shared"),
            )]
            .into_iter()
            .collect(),
        )
        .build()
        .unwrap();
    let (chat, embeddings) = match name {
        "llama-cpp" => {
            let concrete = Arc::new(
                LlamaCpp::new(
                    LlamaCppConfig::builder()
                        .base_url(url)
                        .maybe_api_key(authenticated.then(|| "test-key".to_owned()))
                        .build(),
                )
                .unwrap()
                .with_client(client),
            );
            (
                Provider::from_arc(concrete.clone()),
                EmbeddingProvider::from_arc(concrete),
            )
        }
        "ollama" => {
            let concrete = Arc::new(
                Ollama::new(OllamaConfig::builder().base_url(url).build()).with_client(client),
            );
            (
                Provider::from_arc(concrete.clone()),
                EmbeddingProvider::from_arc(concrete),
            )
        }
        "omlx" => {
            let concrete = Arc::new(
                Omlx::new(
                    OmlxConfig::builder()
                        .base_url(url)
                        .maybe_api_key(authenticated.then(|| "test-key".to_owned()))
                        .build(),
                )
                .unwrap()
                .with_client(client),
            );
            (
                Provider::from_arc(concrete.clone()),
                EmbeddingProvider::from_arc(concrete),
            )
        }
        _ => panic!("unknown provider {name}"),
    };
    assert_eq!(
        Arc::as_ptr(chat.inner()) as *const (),
        Arc::as_ptr(embeddings.inner()) as *const ()
    );
    embeddings
}

async fn server(status: &str, body: Value) -> (String, JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/prefix/v1/", listener.local_addr().unwrap());
    let body = body.to_string();
    let response = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            assert_ne!(count, 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8(bytes[..end].to_vec())
                    .unwrap()
                    .to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    let body = serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                    socket.write_all(response.as_bytes()).await.unwrap();
                    return (headers, body);
                }
            }
        }
    });
    (url, task)
}

#[tokio::test]
async fn local_providers_embed_batches_with_shared_clients_and_authentication() {
    for name in ["llama-cpp", "ollama", "omlx"] {
        for authenticated in [false, true] {
            if name == "ollama" && authenticated {
                continue;
            }
            let (url, task) = server(
                "200 OK",
                json!({
                    "model": "router/embedding",
                    "data": [
                        {"object":"embedding", "index":1, "embedding":[0.3, 0.4]},
                        {"object":"embedding", "index":0, "embedding":[0.1, 0.2]}
                    ],
                    "usage": {"prompt_tokens":8, "total_tokens":8}
                }),
            )
            .await;
            let provider = local_provider(name, &url, authenticated);
            assert_eq!(provider.name(), name);
            let model = EmbeddingModel::builder()
                .id("router/embedding")
                .provider(name)
                .expected_dimensions(NonZeroU32::new(2).unwrap())
                .build();
            let request = EmbeddingRequest::builder()
                .input(vec![
                    "search_query: unchanged".into(),
                    "search_document: unchanged".into(),
                ])
                .maybe_dimensions((name != "llama-cpp").then_some(2_u32))
                .build();
            let response = provider.embed(&model, &request).await.unwrap();
            assert_eq!(response.model, model.id);
            assert_eq!(response.embeddings, [vec![0.1, 0.2], vec![0.3, 0.4]]);
            assert_eq!(response.usage.unwrap().input_tokens, 8);
            provider.refresh_auth().await.unwrap();
            provider.reload_auth().await.unwrap();
            let (headers, body) = task.await.unwrap();
            assert!(headers.starts_with("post /prefix/v1/embeddings "));
            assert!(headers.contains("accept: application/json"));
            assert!(headers.contains("x-test-client: shared"));
            assert_eq!(
                headers.contains("authorization: bearer test-key"),
                authenticated
            );
            assert_eq!(body["model"], model.id);
            assert_eq!(body["input"], json!(request.input));
            assert!(body.get("expected_dimensions").is_none());
            assert_eq!(
                body.get("dimensions").cloned(),
                request.dimensions.map(Value::from)
            );
        }
    }
}

#[tokio::test]
async fn local_providers_preserve_http_errors_and_enforce_pinned_response_widths() {
    use animist::{
        protocol::{ProtocolError, openai::OpenAIProtocolError},
        provider::ProviderError,
    };

    for name in ["llama-cpp", "ollama", "omlx"] {
        for status in ["200 OK", "503 Service Unavailable"] {
            let (url, task) = server(
                status,
                json!({
                    "model":"embedding", "data":[{"index":0, "embedding":[0.1]}]
                }),
            )
            .await;
            let provider = local_provider(name, &url, false);
            let model = EmbeddingModel::builder()
                .id("embedding")
                .provider(name)
                .expected_dimensions(NonZeroU32::new(2).unwrap())
                .build();
            let error = provider
                .embed(
                    &model,
                    &EmbeddingRequest::builder()
                        .input(vec!["text".into()])
                        .build(),
                )
                .await
                .unwrap_err();
            if status == "200 OK" {
                assert!(matches!(
                    error,
                    ProviderError::Protocol {
                        source: ProtocolError::OpenAI {
                            source: OpenAIProtocolError::EmbeddingDimensions {
                                expected: 2,
                                actual: 1,
                                ..
                            }
                        }
                    }
                ));
            } else {
                assert!(
                    matches!(error, ProviderError::Http {status:503, body} if body.contains("embedding"))
                );
            }
            task.await.unwrap();
        }
    }
}

#[tokio::test]
async fn local_providers_reject_wrong_provider_and_invalid_requests_without_io() {
    use animist::provider::ProviderError;

    for name in ["llama-cpp", "ollama", "omlx"] {
        let provider = local_provider(name, "http://127.0.0.1:1", false);
        let mut model = EmbeddingModel::builder()
            .id("embedding")
            .provider("wrong")
            .build();
        let request = EmbeddingRequest::builder()
            .input(vec!["text".into()])
            .build();
        assert!(matches!(
            provider.embed(&model, &request).await,
            Err(ProviderError::ModelProviderMismatch { .. })
        ));
        model.provider = name.into();
        assert!(matches!(
            provider.embed(&model, &EmbeddingRequest::default()).await,
            Err(ProviderError::Protocol { .. })
        ));
        model.expected_dimensions = NonZeroU32::new(2);
        let request = EmbeddingRequest::builder()
            .input(vec!["text".into()])
            .dimensions(3_u32)
            .build();
        assert!(matches!(
            provider.embed(&model, &request).await,
            Err(ProviderError::Protocol { .. })
        ));
        if name == "llama-cpp" {
            let request = EmbeddingRequest::builder()
                .input(vec!["text".into()])
                .dimensions(2_u32)
                .build();
            assert!(matches!(
                provider.embed(&model, &request).await,
                Err(ProviderError::EmbeddingDimensionsUnsupported { .. })
            ));
        }
    }
}
