use serde_json::Value;
use snafu::Snafu;

use crate::provider::ProviderEvent;

#[derive(Debug, Snafu)]
#[snafu(visibility(pub(crate)))]
pub enum OpenAIProtocolError {
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
    InvalidReasoningSignature,
    #[snafu(display(
        "Chat Completions reasoning history requires reasoning_content replay support"
    ))]
    ChatReasoningReplayDisabled,
    #[snafu(display("Responses reasoning history requires its original reasoning item"))]
    MissingResponsesReasoningItem,
    #[snafu(display("Missing or invalid string field {field}"))]
    MissingStringField { field: &'static str },
    #[snafu(display("Content cannot be replayed through Chat Completions"))]
    UnsupportedChatContent,
    #[snafu(display("Content cannot be replayed through Responses"))]
    UnsupportedResponsesContent,
    #[snafu(display("Unsupported completion choice index {index}"))]
    MultipleChoices { index: u32 },
    #[snafu(display("The model refused the request"))]
    Refusal,
    #[snafu(display("Tool arguments arrived before identity for index {index}"))]
    ToolArgumentsBeforeIdentity { index: u64 },
    #[snafu(display("Unexpected Chat Completions finish reason {reason}"))]
    UnexpectedFinishReason { reason: String },
    #[snafu(display("Chat Completions stream ended before [DONE]"))]
    TruncatedChatStream,
    #[snafu(display("Chat Completions stream has no finish reason"))]
    MissingFinishReason,
    #[snafu(display("Incomplete identity for tool call index {index}"))]
    IncompleteToolIdentity { index: u64 },
    #[snafu(display("Duplicate tool item {id}"))]
    DuplicateToolItem { id: String },
    #[snafu(display("Unknown tool item {id}"))]
    UnknownToolItem { id: String },
    #[snafu(display("OpenAI returned an API error: {error}"))]
    Api { error: Value },
    #[snafu(display("OpenAI response failed: {error}"))]
    ResponseFailed { error: Value },
    #[snafu(display("Responses stream ended before a terminal event"))]
    TruncatedResponsesStream,
    #[snafu(display("Missing response output array"))]
    MissingOutput,
    #[snafu(display("Missing response message content array"))]
    MissingMessageContent,
    #[snafu(display("Unsupported Responses content type {kind}"))]
    UnsupportedContentType { kind: String },
    #[snafu(display("Unsupported Responses output item type {kind}"))]
    UnsupportedOutputItem { kind: String },
    #[snafu(display("Unsuccessful Responses status {status}, incomplete reason {reason}"))]
    UnsuccessfulResponse { status: Value, reason: Value },
    #[snafu(display("Event receiver disconnected: {source}"))]
    EventReceiver {
        source: flume::SendError<ProviderEvent>,
    },
}
