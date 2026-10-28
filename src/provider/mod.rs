//! Provider clients, credentials, model policy, and streaming exchange types.

use arc_handle::arc_handle;
use async_trait::async_trait;

pub mod anthropic;
#[cfg(feature = "bedrock")]
pub mod bedrock;
mod decider;
mod embedding;
pub(crate) mod error;
mod exchange;
pub mod llama_cpp;
mod model;
pub mod ollama;
pub mod omlx;
pub mod openai;
pub mod synthetic;
mod tools;
mod types;
pub mod zai_coding_plan;
use crate::history::Message;
pub use decider::{
    BooleanCriteria, DeciderModel, DeciderProvider, DeciderProviderImpl, DecisionAnswer,
    DecisionQuestion, DecisionRequest, DecisionResponse, DecisionUsage,
};
pub use embedding::{
    EmbeddingModel, EmbeddingProvider, EmbeddingProviderImpl, EmbeddingRequest, EmbeddingResponse,
    EmbeddingUsage,
};
pub use error::ProviderError;
pub use exchange::{ExchangeEvent, InferenceExchange, PartialResponse};
use flume::Sender;
pub use model::{
    Config, Effort, FastPricing, Features, Model, ModelInfo, Overrides, Pricing, Thinking,
    ThinkingSupport, Tier,
};
pub use tools::Tool;
pub use types::{
    InputTransformation, ProviderEvent, RequestOptions, StopReason, StreamResponse, ThinkingConfig,
    TokenUsage,
};

/// Result alias for provider operations.
pub type Result<T> = std::result::Result<T, ProviderError>;

/// Cap on failed-response bodies buffered into errors, in bytes.
pub(crate) const ERROR_BODY_LIMIT: usize = 64 * 1024;

/// Truncates a failed-response body to `ERROR_BODY_LIMIT` bytes,
/// backing off to the nearest char boundary.
pub(crate) fn clamp_error_body(mut body: String) -> String {
    let mut limit = ERROR_BODY_LIMIT.min(body.len());
    while !body.is_char_boundary(limit) {
        limit -= 1;
    }
    body.truncate(limit);
    body
}

/// Chat completion provider bound to a concrete transport and model policy.
#[arc_handle]
#[async_trait]
pub trait Provider {
    /// Stable provider identifier used for model ownership checks.
    fn name(&self) -> &'static str;

    /// Emits incremental events and returns the assembled response.
    /// Errors return through `Result`; already-emitted events remain available.
    /// Emit closed-content and cumulative-usage checkpoints for partial recovery.
    /// Dropping this future cancels the request; implementations must not detach it.
    async fn stream_message(
        &self,
        model: &Model,
        messages: &[Message],
        system: &str,
        tools: &[Tool],
        event_tx: &Sender<ProviderEvent>,
        options: RequestOptions,
    ) -> Result<StreamResponse>;

    /// Lists models the provider currently serves, merging catalog
    /// metadata where the source omits it.
    async fn list_models(&self) -> Result<Vec<ModelInfo>>;

    /// Renews expiring credentials held in memory.
    async fn refresh_auth(&self) -> Result<()>;

    /// Re-reads credentials from their original source.
    async fn reload_auth(&self) -> Result<()>;

    /// Adjusts a model before first use; the default leaves it unchanged.
    async fn adjust_model(&self, _model: &mut Model) -> Result<()> {
        Ok(())
    }
}
