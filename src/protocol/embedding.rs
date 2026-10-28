//! Embedding protocol contract independent of chat streaming.

use async_trait::async_trait;

use crate::provider::{EmbeddingModel, EmbeddingRequest, EmbeddingResponse};

use super::Result;

/// Embedding translation is independent of chat messages and streaming events.
#[async_trait]
pub trait EmbeddingProtocol: Send + Sync {
    type Request: Send;
    type Response: Send;

    fn request(&self, model: &EmbeddingModel, request: &EmbeddingRequest) -> Result<Self::Request>;

    async fn assemble(
        &self,
        response: Self::Response,
        model: &EmbeddingModel,
        request: &EmbeddingRequest,
    ) -> Result<EmbeddingResponse>;
}
