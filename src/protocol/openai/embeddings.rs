//! `OpenAI` Embeddings request translation and response validation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    protocol::{EmbeddingProtocol, Result},
    provider::{EmbeddingModel, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage},
};

use super::error;

/// Translates embedding requests and validates batched responses.
#[derive(crate::Struct!)]
pub struct Embeddings;

#[derive(crate::Struct!, Serialize)]
struct Request<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(crate::Struct!, Deserialize)]
struct Response {
    model: String,
    data: Vec<Entry>,
    usage: Option<Usage>,
}

#[derive(crate::Struct!, Deserialize)]
struct Entry {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(crate::Struct!, Deserialize)]
struct Usage {
    prompt_tokens: u64,
    total_tokens: Option<u64>,
}

#[async_trait]
impl EmbeddingProtocol for Embeddings {
    type Request = serde_json::Value;
    type Response = reqwest::Response;

    fn request(&self, model: &EmbeddingModel, request: &EmbeddingRequest) -> Result<Self::Request> {
        ensure!(!request.input.is_empty(), error::EmptyEmbeddingInputSnafu);
        if let (Some(expected), Some(requested)) = (model.expected_dimensions, request.dimensions) {
            ensure!(
                expected == requested,
                error::EmbeddingDimensionConflictSnafu {
                    expected: expected.get(),
                    requested: requested.get(),
                }
            );
        }
        Ok(serde_json::to_value(Request {
            model: &model.id,
            input: &request.input,
            dimensions: request.dimensions.map(std::num::NonZeroU32::get),
        })
        .context(error::EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse> {
        let bytes = crate::protocol::body::read_body(response)
            .await
            .context(error::ReadEmbeddingsSnafu)?;
        let response: Response =
            serde_json::from_slice(&bytes).context(error::DecodeEmbeddingsSnafu)?;
        let mut embeddings = vec![None; request.input.len()];
        #[expect(clippy::as_conversions, reason = "u32 dimensions fit usize")]
        let mut dimensions = model
            .expected_dimensions
            .map(std::num::NonZeroU32::get)
            .or(request.dimensions.map(std::num::NonZeroU32::get))
            .map(|value| value as usize);
        for entry in response.data {
            let slot =
                embeddings
                    .get_mut(entry.index)
                    .context(error::EmbeddingIndexOutOfRangeSnafu {
                        index: entry.index,
                        count: request.input.len(),
                    })?;
            ensure!(
                slot.is_none(),
                error::DuplicateEmbeddingIndexSnafu { index: entry.index }
            );
            let expected = *dimensions.get_or_insert(entry.embedding.len());
            ensure!(
                expected > 0 && entry.embedding.len() == expected,
                error::EmbeddingDimensionsSnafu {
                    index: entry.index,
                    expected,
                    actual: entry.embedding.len(),
                }
            );
            ensure!(
                entry.embedding.iter().all(|value| value.is_finite()),
                error::NonFiniteEmbeddingSnafu { index: entry.index }
            );
            *slot = Some(entry.embedding);
        }
        let embeddings = embeddings
            .into_iter()
            .enumerate()
            .map(|(index, vector)| vector.context(error::MissingEmbeddingSnafu { index }))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(EmbeddingResponse::builder()
            .model(response.model)
            .embeddings(embeddings)
            .maybe_usage(response.usage.map(|usage| {
                EmbeddingUsage::builder()
                    .input_tokens(usage.prompt_tokens)
                    .maybe_total_tokens(usage.total_tokens)
                    .build()
            }))
            .build())
    }
}

#[cfg(test)]
mod tests {
    #![expect(clippy::unwrap_used, reason = "panicking assertions fail the test")]

    use super::*;

    use std::num::NonZeroU32;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    use crate::protocol::ProtocolError;

    type Expected = fn(&ProtocolError) -> bool;

    async fn serve(body: &str) -> reqwest::Response {
        let body = body.to_owned();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0; 4096];
            assert_ne!(socket.read(&mut request).await.unwrap(), 0);
            socket
                .write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len()).as_bytes(),
                )
                .await
                .unwrap();
            socket.write_all(body.as_bytes()).await.unwrap();
        });
        let response = reqwest::get(format!("http://{address}")).await.unwrap();
        task.await.unwrap();
        response
    }

    fn model(expected_dimensions: Option<NonZeroU32>) -> EmbeddingModel {
        EmbeddingModel::builder()
            .id("embedding-model")
            .provider("fixture")
            .maybe_expected_dimensions(expected_dimensions)
            .build()
    }

    fn request(input: usize) -> EmbeddingRequest {
        EmbeddingRequest::builder()
            .input(vec!["first".into(); input])
            .build()
    }

    #[tokio::test]
    async fn assembles_embeddings_in_input_order_with_usage() {
        let body = r#"{"model":"text-embedding-3-small","data":[{"index":1,"embedding":[0.5,2.5]},{"index":0,"embedding":[1.0,2.0]}],"usage":{"prompt_tokens":7,"total_tokens":9}}"#;
        let response = serve(body).await;
        let assembled = Embeddings
            .assemble(response, &model(None), &request(2))
            .await
            .unwrap();
        assert_eq!(assembled.model, "text-embedding-3-small");
        assert_eq!(assembled.embeddings, vec![vec![1.0, 2.0], vec![0.5, 2.5]]);
        let usage = assembled.usage.unwrap();
        assert_eq!(usage.input_tokens, 7);
        assert_eq!(usage.total_tokens, Some(9));
    }

    #[tokio::test]
    async fn assemble_rejects_inconsistent_index_and_dimension_data() {
        let cases: Vec<(&str, Option<NonZeroU32>, usize, Expected)> = vec![
            (
                r#"{"model":"m","data":[{"index":2,"embedding":[1.0]}]}"#,
                None,
                2,
                |error| {
                    matches!(
                        error,
                        ProtocolError::OpenAI {
                            source: error::OpenAIProtocolError::EmbeddingIndexOutOfRange {
                                index: 2,
                                count: 2,
                                ..
                            }
                        }
                    )
                },
            ),
            (
                r#"{"model":"m","data":[{"index":0,"embedding":[1.0]},{"index":0,"embedding":[2.0]}]}"#,
                None,
                2,
                |error| {
                    matches!(
                        error,
                        ProtocolError::OpenAI {
                            source: error::OpenAIProtocolError::DuplicateEmbeddingIndex {
                                index: 0,
                                ..
                            }
                        }
                    )
                },
            ),
            (
                r#"{"model":"m","data":[{"index":1,"embedding":[2.0,3.0]}]}"#,
                None,
                2,
                |error| {
                    matches!(
                        error,
                        ProtocolError::OpenAI {
                            source: error::OpenAIProtocolError::MissingEmbedding { index: 0, .. }
                        }
                    )
                },
            ),
            (
                r#"{"model":"m","data":[{"index":0,"embedding":[1.0,2.0]},{"index":1,"embedding":[3.0]}]}"#,
                None,
                2,
                |error| {
                    matches!(
                        error,
                        ProtocolError::OpenAI {
                            source: error::OpenAIProtocolError::EmbeddingDimensions {
                                index: 1,
                                expected: 2,
                                actual: 1,
                                ..
                            }
                        }
                    )
                },
            ),
            (
                r#"{"model":"m","data":[{"index":0,"embedding":[1.0,2.0]}]}"#,
                NonZeroU32::new(3),
                1,
                |error| {
                    matches!(
                        error,
                        ProtocolError::OpenAI {
                            source: error::OpenAIProtocolError::EmbeddingDimensions {
                                index: 0,
                                expected: 3,
                                actual: 2,
                                ..
                            }
                        }
                    )
                },
            ),
        ];
        for (body, expected_dimensions, input, expected) in cases {
            let response = serve(body).await;
            let error = Embeddings
                .assemble(response, &model(expected_dimensions), &request(input))
                .await
                .unwrap_err();
            assert!(expected(&error));
        }
    }
}
