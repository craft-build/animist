use arc_handle::arc_handle;
use async_trait::async_trait;

pub mod anthropic;
#[cfg(feature = "bedrock")]
pub mod bedrock;
mod decider;
mod embedding;
pub(crate) mod error;
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

pub type Result<T> = std::result::Result<T, ProviderError>;

#[arc_handle]
#[async_trait]
pub trait Provider {
    /// Stable provider identifier used for model ownership checks.
    fn name(&self) -> &str;

    /// Emits incremental events and returns the assembled response.
    /// Errors return through `Result`; already-emitted events remain available.
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

    async fn list_models(&self) -> Result<Vec<ModelInfo>>;

    /// Renews expiring credentials held in memory.
    async fn refresh_auth(&self) -> Result<()>;

    /// Re-reads credentials from their original source.
    async fn reload_auth(&self) -> Result<()>;

    async fn adjust_model(&self, _model: &mut Model) -> Result<()> {
        Ok(())
    }
}
