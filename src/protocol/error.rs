use snafu::Snafu;

use super::openai::OpenAIProtocolError;

#[derive(Debug, Snafu)]
pub enum ProtocolError {
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
}
