//! `OpenAI` protocol failures across Chat Completions, Responses, and Embeddings.

use serde_json::Value;
use snafu::Snafu;

use crate::provider::{ProviderEvent, StopReason};

/// Failures raised by the `OpenAI` protocol adapters.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum OpenAIProtocolError {
    #[snafu(display("Embedding input batch must not be empty"))]
    EmptyEmbeddingInput { backtrace: snafu::Backtrace },
    #[snafu(display(
        "Requested embedding dimensions {requested} conflict with expected width {expected}"
    ))]
    EmbeddingDimensionConflict {
        expected: u32,
        requested: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to read embeddings response: {source}"))]
    ReadEmbeddings { source: reqwest::Error },
    #[snafu(display("Invalid embeddings response: {source}"))]
    DecodeEmbeddings { source: serde_json::Error },
    #[snafu(display("Embedding index {index} is outside input count {count}"))]
    EmbeddingIndexOutOfRange {
        index: usize,
        count: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate embedding index {index}"))]
    DuplicateEmbeddingIndex {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Missing embedding index {index}"))]
    MissingEmbedding {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Embedding {index} has {actual} dimensions, expected {expected} positive dimensions"
    ))]
    EmbeddingDimensions {
        index: usize,
        expected: usize,
        actual: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Embedding {index} contains non-finite values"))]
    NonFiniteEmbedding {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to serialize OpenAI request: {source}"))]
    EncodeRequest { source: serde_json::Error },
    #[snafu(display("Invalid OpenAI usage: {source}"))]
    DecodeUsage { source: serde_json::Error },
    #[snafu(display("Invalid OpenAI stream event JSON: {source}"))]
    DecodeEvent { source: serde_json::Error },
    #[snafu(display("Invalid Chat Completions chunk: {source}"))]
    DecodeChunk { source: serde_json::Error },
    #[snafu(display("Invalid arguments for tool call {id}: {source}"))]
    DecodeToolArguments {
        id: String,
        source: serde_json::Error,
    },
    #[snafu(display("Invalid reasoning signature JSON: {source}"))]
    DecodeReasoningSignature { source: serde_json::Error },
    #[snafu(display("Reasoning signature is not a Responses reasoning item"))]
    InvalidReasoningSignature { backtrace: snafu::Backtrace },
    #[snafu(display(
        "Chat Completions reasoning history requires reasoning_content replay support"
    ))]
    ChatReasoningReplayDisabled { backtrace: snafu::Backtrace },
    #[snafu(display("Responses reasoning history requires its original reasoning item"))]
    MissingResponsesReasoningItem { backtrace: snafu::Backtrace },
    #[snafu(display("Missing or invalid string field {field}"))]
    MissingStringField {
        field: &'static str,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Content cannot be replayed through Chat Completions"))]
    UnsupportedChatContent { backtrace: snafu::Backtrace },
    #[snafu(display("Content cannot be replayed through Responses"))]
    UnsupportedResponsesContent { backtrace: snafu::Backtrace },
    #[snafu(display("Chat Completions cannot represent image output for tool call {id}"))]
    UnsupportedChatToolImage {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Tool activation metadata is unsupported for tool call {id}"))]
    UnsupportedLoadedTools {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unsupported completion choice index {index}"))]
    MultipleChoices {
        index: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("The model refused the request"))]
    Refusal { backtrace: snafu::Backtrace },
    #[snafu(display("Tool arguments arrived before identity for index {index}"))]
    ToolArgumentsBeforeIdentity {
        index: u64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unexpected Chat Completions finish reason {reason}"))]
    UnexpectedFinishReason {
        reason: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Chat Completions stream ended before [DONE] (finish reason {finish_reason:?})"
    ))]
    TruncatedChatStream {
        finish_reason: Option<StopReason>,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Chat Completions stream has no finish reason"))]
    MissingFinishReason { backtrace: snafu::Backtrace },
    #[snafu(display("Chat Completions choice arrived after its finish reason"))]
    ChoiceAfterFinish { backtrace: snafu::Backtrace },
    #[snafu(display("Missing or invalid output item index"))]
    MissingOutputIndex { backtrace: snafu::Backtrace },
    #[snafu(display("Duplicate output item index {index}"))]
    DuplicateOutputIndex {
        index: u64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Incomplete identity for tool call index {index}"))]
    IncompleteToolIdentity {
        index: u64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate tool item {id}"))]
    DuplicateToolItem {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unknown tool item {id}"))]
    UnknownToolItem {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("OpenAI returned an API error: {error}"))]
    Api {
        error: Value,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("OpenAI response failed: {error}"))]
    ResponseFailed {
        error: Value,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Responses stream ended before a terminal event"))]
    TruncatedResponsesStream { backtrace: snafu::Backtrace },
    #[snafu(display("Missing response output array"))]
    MissingOutput { backtrace: snafu::Backtrace },
    #[snafu(display("Missing response message content array"))]
    MissingMessageContent { backtrace: snafu::Backtrace },
    #[snafu(display("Unsupported Responses content type {kind}"))]
    UnsupportedContentType {
        kind: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unsupported Responses output item type {kind}"))]
    UnsupportedOutputItem {
        kind: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Unsuccessful Responses status {status}, incomplete reason {reason}"))]
    UnsuccessfulResponse {
        status: Value,
        reason: Value,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Event receiver disconnected: {source}"))]
    EventReceiver {
        source: flume::SendError<ProviderEvent>,
    },
}
