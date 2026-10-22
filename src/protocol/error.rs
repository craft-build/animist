use snafu::Snafu;

use super::openai::OpenAIProtocolError;

#[derive(Debug, Snafu)]
pub enum ProtocolError {
    #[snafu(transparent)]
    OpenAI { source: OpenAIProtocolError },
    #[snafu(transparent)]
    Sse { source: super::SseError },
}
