use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use snafu::{OptionExt, ResultExt, ensure};

use crate::{
    protocol::{EmbeddingProtocol, Result},
    provider::{EmbeddingModel, EmbeddingRequest, EmbeddingResponse, EmbeddingUsage},
};

use super::error;

pub struct Embeddings;

#[derive(Serialize)]
struct Request<'a> {
    model: &'a str,
    input: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    dimensions: Option<u32>,
}

#[derive(Deserialize)]
struct Response {
    model: String,
    data: Vec<Entry>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Entry {
    index: usize,
    embedding: Vec<f32>,
}

#[derive(Deserialize)]
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
        ensure!(
            request.dimensions != Some(0),
            error::ZeroEmbeddingDimensionsSnafu
        );
        if let (Some(expected), Some(requested)) = (model.expected_dimensions, request.dimensions) {
            ensure!(
                expected.get() == requested,
                error::EmbeddingDimensionConflictSnafu {
                    expected: expected.get(),
                    requested,
                }
            );
        }
        Ok(serde_json::to_value(Request {
            model: &model.id,
            input: &request.input,
            dimensions: request.dimensions,
        })
        .context(error::EncodeRequestSnafu)?)
    }

    async fn assemble(
        &self,
        response: Self::Response,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse> {
        let bytes = response.bytes().await.context(error::ReadEmbeddingsSnafu)?;
        let response: Response =
            serde_json::from_slice(&bytes).context(error::DecodeEmbeddingsSnafu)?;
        let mut embeddings = vec![None; request.input.len()];
        let mut dimensions = model
            .expected_dimensions
            .map(|value| value.get())
            .or(request.dimensions)
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
