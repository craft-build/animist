//! Protocol contracts independent of provider authentication and routing.

use async_trait::async_trait;
use bon::Builder;
use flume::Sender;

use crate::history::Message;
use crate::provider::{Model, ProviderEvent, RequestOptions, StreamResponse, Tool};

pub mod anthropic;
#[cfg(feature = "bedrock")]
pub mod bedrock;
pub(crate) mod body;
mod decider;
mod embedding;
mod error;
pub mod openai;
mod replay;
pub(crate) mod sampling;
pub(crate) mod sse;
pub mod system_one;

pub use body::BodyError;
pub use decider::DeciderProtocol;
pub use embedding::EmbeddingProtocol;
pub use error::ProtocolError;
pub use replay::{ReplayError, ReplayLocation, validate_replay};
pub use sampling::SamplingError;
pub use sse::SseError;

/// Result alias for protocol translation and assembly.
pub type Result<T> = std::result::Result<T, ProtocolError>;

/// Shared inference input before translation into a protocol's wire format.
#[derive(Clone, Copy, Debug, Builder)]
#[builder(on(_, into))]
pub struct ProtocolInput<'a> {
    pub model: &'a Model,
    pub messages: &'a [Message],
    pub system: &'a str,
    pub tools: &'a [Tool],
    pub options: &'a RequestOptions,
}

/// Vendor-neutral adapter contract: implementations translate inference input
/// into wire requests and assemble streamed responses into history messages.
/// Per-request stream state lives in the returned future, so dropping it
/// cancels the exchange; checkpoints are emitted as in-order closed content
/// prefixes with cumulative usage.
#[async_trait]
pub trait Protocol: Send + Sync {
    type Request: Send;
    type Response: Send;
    type Options: Send + Sync;

    /// Translates a validated inference view into this protocol's wire format.
    fn request(&self, input: ProtocolInput<'_>, options: &Self::Options) -> Result<Self::Request>;

    /// Emits incremental events; dropping this future cancels stream assembly.
    /// Checkpoint closed content in order and cumulative usage when reported.
    async fn assemble(
        &self,
        response: Self::Response,
        event_tx: &Sender<ProviderEvent>,
    ) -> Result<StreamResponse>;
}
