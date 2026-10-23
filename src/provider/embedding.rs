use arc_handle::arc_handle;
use async_trait::async_trait;
use bon::Builder;
use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

use super::Result;

/// Explicit embedding model identity, independent of chat configuration.
#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct EmbeddingModel {
    pub id: String,
    pub provider: String,
    /// Pins the output width; does not request server-side dimension reduction.
    #[serde(default)]
    pub expected_dimensions: Option<NonZeroU32>,
}

/// Text inputs are embedded without rewriting, truncation, or normalization.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct EmbeddingRequest {
    pub input: Vec<String>,
    pub dimensions: Option<u32>,
}

#[derive(crate::Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct EmbeddingUsage {
    pub input_tokens: u64,
    pub total_tokens: Option<u64>,
}

/// Vectors are returned in the same order as the request inputs.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct EmbeddingResponse {
    pub model: String,
    pub embeddings: Vec<Vec<f32>>,
    pub usage: Option<EmbeddingUsage>,
}

#[arc_handle]
#[async_trait]
pub trait EmbeddingProvider {
    fn name(&self) -> &str;

    /// Dropping this future cancels the exchange; implementations must not detach it.
    async fn embed(
        &self,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse>;

    async fn refresh_auth(&self) -> Result<()>;
    async fn reload_auth(&self) -> Result<()>;
}
