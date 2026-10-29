//! Vendor-neutral protocol error layering.

use snafu::Snafu;

use super::openai::OpenAIProtocolError;

/// Failures surfaced by every protocol adapter through `protocol::Result`.
#[derive(Debug, Snafu)]
pub enum ProtocolError {
    #[cfg(feature = "bedrock")]
    #[snafu(transparent)]
    Bedrock {
        source: super::bedrock::BedrockProtocolError,
    },
    #[snafu(transparent)]
    Sampling { source: super::SamplingError },
    #[snafu(transparent)]
    Replay { source: super::ReplayError },
    #[snafu(transparent)]
    SystemOne {
        source: super::system_one::SystemOneError,
    },
    #[snafu(transparent)]
    Anthropic {
        source: super::anthropic::AnthropicProtocolError,
    },
    #[snafu(transparent)]
    OpenAI { source: OpenAIProtocolError },
    #[snafu(transparent)]
    Sse { source: super::SseError },
    #[snafu(transparent)]
    Body { source: super::BodyError },
}
