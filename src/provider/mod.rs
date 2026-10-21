use arc_handle::arc_handle;
use async_trait::async_trait;

mod error;
mod message;
mod model;
mod types;

use flume::Sender;
pub use message::{ContentBlock, Message, MessageKind, Role};
pub use model::{Config, Effort, FastPricing, Features, Model, ModelInfo, Pricing, Thinking};
pub use types::{ProviderEvent, RequestOptions};

use serde_json::Value;

use crate::provider::types::StreamResponse;
type Result<T> = std::result::Result<T, error::Error>;

#[arc_handle]
#[async_trait]
pub trait Provider {
    async fn stream_message(
        &self,
        model: &ModelInfo,
        messages: &[Message],
        system: &str,
        tools: &Value,
        event_tx: &Sender<ProviderEvent>,
        options: RequestOptions,
    ) -> Result<StreamResponse>;

    async fn list_models(&self) -> Result<Vec<ModelInfo>>;
    async fn refresh_auth(&self) -> Result<()>;
    async fn reload_auth(&self) -> Result<()>;
    async fn adjust_model(&self, _model: &mut Model) -> Result<()>;
}
