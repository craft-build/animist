//! Protocol contracts independent of provider authentication and routing.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;

use crate::provider::{Message, Model, ProviderEvent, RequestOptions, StreamResponse, Tool};

pub mod anthropic;
mod error;
pub mod openai;
pub(crate) mod sse;

pub use error::ProtocolError;
pub use sse::SseError;
pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Shared inference input before translation into a protocol's wire format.
#[derive(Debug, Clone, Copy, Builder)]
#[builder(on(_, into))]
pub struct ProtocolInput<'a> {
    pub model: &'a Model,
    pub messages: &'a [Message],
    pub system: &'a str,
    pub tools: &'a [Tool],
    pub options: &'a RequestOptions,
}

/// Translates inference input and assembles a transport response.
/// Implementations keep per-request state in the returned future.
#[async_trait]
pub trait Protocol: Send + Sync {
    type Request: Send;
    type Response: Send;
    type Options: Send + Sync;

    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request>;

    /// Emits incremental events; dropping this future cancels stream assembly.
    async fn assemble(
        &self,
        response: Self::Response,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse>;
}
