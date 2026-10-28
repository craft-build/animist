use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

use super::Result;

/// Explicit embedding model identity, independent of chat configuration.
#[derive(crate::Struct!, PartialEq, Eq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct EmbeddingModel {
    /// Wire model ID sent in requests.
    pub id: String,
    /// Provider name that owns and validates this model.
    pub provider: String,
    /// Pins the output width; does not request server-side dimension reduction.
    #[serde(default)]
    pub expected_dimensions: Option<NonZeroU32>,
}

/// Text inputs are embedded without rewriting, truncation, or normalization.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct EmbeddingRequest {
    /// Inputs embedded as one batch, preserving their order.
    pub input: Vec<String>,
    /// Output width requested from the server; `None` uses the native width.
    pub dimensions: Option<NonZeroU32>,
}

/// Token accounting for one embedding batch.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct EmbeddingUsage {
    /// Tokens consumed across the batch inputs.
    pub input_tokens: u64,
    /// Total tokens when the server reports a separate aggregate.
    pub total_tokens: Option<u64>,
}

/// Vectors are returned in the same order as the request inputs.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct EmbeddingResponse {
    /// Model ID that produced the vectors.
    pub model: String,
    /// One vector per request input, in request order.
    pub embeddings: Vec<Vec<f32>>,
    /// Usage for the batch, when the server reports it.
    pub usage: Option<EmbeddingUsage>,
}

/// Provider specialized for batched text embeddings.
#[arc_handle]
#[async_trait]
pub trait EmbeddingProvider {
    /// Stable provider identifier used for model ownership checks.
    fn name(&self) -> &'static str;

    /// Dropping this future cancels the exchange; implementations must not detach it.
    async fn embed(
        &self,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse>;

    /// Renews expiring credentials held in memory.
    async fn refresh_auth(&self) -> Result<()>;

    /// Re-reads credentials from their original source.
    async fn reload_auth(&self) -> Result<()>;
}
