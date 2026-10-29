use snafu::Snafu;

use crate::{history::Role, provider::ThinkingConfig};

/// AWS `EventStream` framing, integrity, and header failures.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum EventStreamError {
    #[snafu(display("Failed to read EventStream: {source}"))]
    Read { source: reqwest::Error },
    #[snafu(display("EventStream exchange exceeds {limit} bytes"))]
    ExchangeTooLarge {
        limit: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid EventStream lengths: total {total}, headers {headers}"))]
    InvalidLengths {
        total: usize,
        headers: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("EventStream {section} CRC mismatch: expected {expected}, got {actual}"))]
    Crc {
        section: &'static str,
        expected: u32,
        actual: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Truncated EventStream frame: {received} of {expected} bytes"))]
    TruncatedFrame {
        received: usize,
        expected: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Truncated EventStream header"))]
    TruncatedHeader { backtrace: snafu::Backtrace },
    #[snafu(display("EventStream header name is empty"))]
    EmptyHeaderName { backtrace: snafu::Backtrace },
    #[snafu(display("Invalid EventStream header UTF-8: {source}"))]
    HeaderUtf8 { source: std::str::Utf8Error },
    #[snafu(display("Unknown EventStream header type {kind}"))]
    UnknownHeaderType {
        kind: u8,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Duplicate EventStream header {name}"))]
    DuplicateHeader {
        name: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Missing or non-string EventStream header {name}"))]
    RequiredHeader {
        name: &'static str,
        backtrace: snafu::Backtrace,
    },
}

/// Converse translation, stream sequencing, and upstream API failures.
#[derive(Debug, Snafu)]
#[snafu(visibility(pub(super)))]
pub enum BedrockProtocolError {
    #[snafu(transparent)]
    EventStream { source: EventStreamError },
    #[snafu(display("Failed to serialize Converse request: {source}"))]
    EncodeRequest { source: serde_json::Error },
    #[snafu(display("Failed to decode Converse {event_type}: {source}"))]
    DecodeEvent {
        event_type: String,
        source: serde_json::Error,
    },
    #[snafu(display("Converse cannot represent content for role {role:?}"))]
    UnsupportedContent {
        role: Role,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse cannot preserve system placement at message {message}"))]
    UnsupportedSystemPlacement {
        message: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse reasoning replay requires a nonempty signature"))]
    MissingReplaySignature { backtrace: snafu::Backtrace },
    #[snafu(display("Converse cannot represent tool activation metadata"))]
    UnsupportedLoadedTools { backtrace: snafu::Backtrace },
    #[snafu(display("Converse tool {id} input must be an object"))]
    InvalidToolInput {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse requires inline images, not remote URLs"))]
    RemoteImage { backtrace: snafu::Backtrace },
    #[snafu(display("Converse does not support image media type {media_type}"))]
    ImageMediaType {
        media_type: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid Converse base64 data: {source}"))]
    Base64 { source: base64::DecodeError },
    #[snafu(display("Converse has no portable translation for thinking {thinking:?}"))]
    UnsupportedThinking {
        thinking: ThinkingConfig,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse has no portable fast-mode setting"))]
    UnsupportedFastMode { backtrace: snafu::Backtrace },
    #[snafu(display("Converse maxTokens must be nonzero"))]
    ZeroMaxTokens { backtrace: snafu::Backtrace },
    #[snafu(display("Converse additionalModelRequestFields must be an object"))]
    InvalidAdditionalFields { backtrace: snafu::Backtrace },
    #[snafu(display("Unexpected Converse event {event_type}"))]
    UnexpectedEvent {
        event_type: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Converse block index {index} is not the next block {expected} or an open block"
    ))]
    BlockIndex {
        index: usize,
        expected: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse block {index} delta does not match its content type"))]
    DeltaMismatch {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse block {index} was already closed"))]
    ClosedBlock {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse tool ID {id:?} is empty or repeated, or its name is empty"))]
    InvalidToolStart {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to decode Converse tool {id} input: {source}"))]
    DecodeToolInput {
        id: String,
        source: serde_json::Error,
    },
    #[snafu(display("Converse block {index} reasoning has no signature"))]
    MissingSignature {
        index: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse message stopped with open content blocks"))]
    UnclosedBlocks { backtrace: snafu::Backtrace },
    #[snafu(display("Unsupported Converse stop reason {reason}"))]
    StopReason {
        reason: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Converse stream ended before message stop and usage metadata"))]
    TruncatedStream { backtrace: snafu::Backtrace },
    #[snafu(display("Converse event receiver disconnected: {source}"))]
    EventReceiver {
        source: flume::SendError<crate::provider::ProviderEvent>,
    },
    #[snafu(display("Bedrock stream exception {kind}"))]
    Api {
        kind: String,
        headers: std::collections::BTreeMap<String, Option<String>>,
        body: Vec<u8>,
        backtrace: snafu::Backtrace,
    },
}
