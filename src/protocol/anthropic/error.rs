//! Anthropic Messages protocol failures.

use flume::SendError;
use serde_json::Value;
use snafu::Snafu;

use crate::provider::ProviderEvent;

/// Failures raised by the Anthropic Messages adapter.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum AnthropicProtocolError {
    #[snafu(display("Failed to serialize Anthropic request: {source}"))]
    EncodeRequest { source: serde_json::Error },
    #[snafu(display("Invalid Anthropic stream event: {source}"))]
    DecodeEvent { source: serde_json::Error },
    #[snafu(display("Content is not supported in an Anthropic {role:?} message"))]
    UnsupportedContent {
        role: crate::history::Role,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Anthropic thinking replay requires a nonempty signature"))]
    MissingReplaySignature { backtrace: snafu::Backtrace },
    #[snafu(display("Anthropic does not support image media type {media_type}"))]
    UnsupportedImageMediaType {
        media_type: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Deferred tool references are not supported by this adapter"))]
    UnsupportedLoadedTools { backtrace: snafu::Backtrace },
    #[snafu(display("Anthropic returned an API error: {error}"))]
    Api {
        error: Value,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unexpected Anthropic event {event} in this stream state"))]
    UnexpectedEvent {
        event: &'static str,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Anthropic message_start must contain an assistant message with no content"))]
    InvalidMessageStart { backtrace: snafu::Backtrace },
    #[snafu(display("Expected content block index {expected}, received {index}"))]
    UnexpectedBlockIndex {
        index: u32,
        expected: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unknown content block index {index}"))]
    UnknownBlock {
        index: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Content block {index} has already stopped"))]
    BlockAlreadyStopped {
        index: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Delta is incompatible with content block {index}"))]
    DeltaTypeMismatch {
        index: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Tool call {id} has non-object input"))]
    InvalidToolInput {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Tool call {id} has both initial input and streamed arguments"))]
    ConflictingToolInput {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid JSON arguments for tool call {id}: {source}"))]
    DecodeToolArguments {
        id: String,
        source: serde_json::Error,
    },
    #[snafu(display("Thinking block {index} has no signature"))]
    MissingThinkingSignature {
        index: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Message ended while content blocks were still open"))]
    UnclosedBlocks { backtrace: snafu::Backtrace },
    #[snafu(display("Anthropic stream ended before message_stop"))]
    TruncatedStream { backtrace: snafu::Backtrace },
    #[snafu(display("Anthropic stream has no stop reason"))]
    MissingStopReason { backtrace: snafu::Backtrace },
    #[snafu(display("Unsupported Anthropic stop reason {reason}"))]
    UnsupportedStopReason {
        reason: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Event receiver disconnected: {source}"))]
    EventReceiver { source: SendError<ProviderEvent> },
}
