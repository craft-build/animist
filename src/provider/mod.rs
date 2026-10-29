//! Provider clients, credentials, model policy, and streaming exchange types.

use arc_handle::arc_handle;
use async_trait::async_trait;

pub mod anthropic;
#[cfg(feature = "bedrock")]
pub mod bedrock;
mod credential;
mod decider;
mod embedding;
pub(crate) mod error;
mod exchange;
mod http;
pub mod llama_cpp;
mod model;
pub mod ollama;
pub mod omlx;
pub mod openai;
pub mod synthetic;
mod thinking;
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
    InputTransformation, ProviderEvent, RequestOptions, StopReason, StreamResponse,
    SystemMessagePolicy, ThinkingConfig, TokenUsage,
};

/// Result alias for provider operations.
pub type Result<T> = std::result::Result<T, ProviderError>;

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
