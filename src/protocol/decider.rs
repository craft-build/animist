use async_trait::async_trait;

use crate::provider::{DeciderModel, DecisionRequest, DecisionResponse};

use super::Result;

/// Decision translation is independent of chat messages and streaming events.
#[async_trait]
pub trait DeciderProtocol: Send + Sync {
    type Request: Send;
    type Response: Send;

    fn request(&self, model: &DeciderModel, request: &DecisionRequest) -> Result<Self::Request>;

    async fn assemble(
        &self,
        response: Self::Response,
        request: &DecisionRequest,
    ) -> Result<DecisionResponse>;
}
