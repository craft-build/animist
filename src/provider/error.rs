use snafu::Snafu;

use crate::protocol::{
    ProtocolError, anthropic::AnthropicProtocolError, openai::OpenAIProtocolError,
};

/// Failures raised by provider transports, credentials, and model policy.
#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum ProviderError {
    #[cfg(feature = "bedrock")]
    #[snafu(transparent)]
    Bedrock {
        source: super::bedrock::BedrockError,
    },
    #[snafu(display("Provider transport failed: {source}"))]
    Transport { source: reqwest::Error },
    #[snafu(display(
        "Provider returned HTTP {status}{suffix}: {body}",
        suffix = if *body_truncated { " (body capped at 64 KiB; may be incomplete)" } else { "" }
    ))]
    Http {
        status: u16,
        body: String,
        /// The size limit shortened the body or stopped reading before completeness was known.
        body_truncated: bool,
        backtrace: snafu::Backtrace,
    },
    #[snafu(transparent)]
    Protocol {
        source: crate::protocol::ProtocolError,
    },
    #[snafu(transparent)]
    Body { source: crate::protocol::BodyError },
    #[snafu(display("Explicit API key is empty"))]
    EmptyApiKey { backtrace: snafu::Backtrace },
    #[snafu(display("This model requires reasoning"))]
    ReasoningRequired { backtrace: snafu::Backtrace },
    #[snafu(display("Provider does not support reasoning token budgets"))]
    ReasoningBudgetUnsupported { backtrace: snafu::Backtrace },
    #[snafu(display("Unsupported reasoning effort {effort:?}"))]
    UnsupportedReasoningEffort {
        effort: super::Effort,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Model belongs to {actual}, expected {expected}"))]
    ModelProviderMismatch {
        expected: String,
        actual: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Fast mode is disabled for this model"))]
    FastModeDisabled { backtrace: snafu::Backtrace },
    #[snafu(display("Invalid model listing: {source}"))]
    DecodeModels { source: serde_json::Error },
    #[snafu(display("Credential environment variable {variable} is missing or empty"))]
    Credentials {
        variable: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Failed to read credential variable {variable}: {source}"))]
    CredentialEnvironment {
        variable: String,
        source: std::env::VarError,
    },
    #[snafu(display("API key is not a valid HTTP header value: {source}"))]
    CredentialHeader {
        source: reqwest::header::InvalidHeaderValue,
    },
    #[snafu(display("Thinking is not enabled for model {model}"))]
    ThinkingUnsupported {
        model: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Thinking budget {budget} must be at least 1024 and less than max_tokens {max_tokens}"
    ))]
    InvalidThinkingBudget {
        budget: u32,
        max_tokens: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Paginated model listing has no last_id"))]
    MissingModelCursor { backtrace: snafu::Backtrace },
    #[snafu(display("Paginated model listing repeated cursor {cursor}"))]
    RepeatedModelCursor {
        cursor: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid Synthetic model listing: {source}"))]
    DecodeSyntheticModels { source: serde_json::Error },
    #[snafu(display("Invalid Synthetic price for {field}: {value}"))]
    InvalidSyntheticPrice {
        field: &'static str,
        value: serde_json::Value,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Provider {provider} does not support fast mode"))]
    FastModeUnsupported {
        provider: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Maximum output token limit must be positive"))]
    ZeroOutputTokenLimit { backtrace: snafu::Backtrace },
    #[snafu(display("Provider {provider} does not support explicit {control}"))]
    UnsupportedSamplingControl {
        provider: String,
        control: &'static str,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display(
        "Provider {provider}: {control} must be finite and in [{min}, {max}], got {value}"
    ))]
    SamplingOutOfRange {
        provider: String,
        control: &'static str,
        value: f64,
        min: f64,
        max: f64,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Provider {provider} supports at most {max} stop sequences, got {count}"))]
    TooManyStopSequences {
        provider: String,
        count: usize,
        max: usize,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Provider {provider} does not support requesting embedding dimensions"))]
    EmbeddingDimensionsUnsupported {
        provider: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("llama.cpp thinking budget {budget} exceeds its signed 32-bit range"))]
    LlamaCppThinkingBudgetOutOfRange {
        budget: u32,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Invalid oMLX model listing: {source}"))]
    DecodeOmlxModels { source: serde_json::Error },
}

/// Model-authored malformed tool-call arguments, recoverable by feeding
/// the failure back to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedToolArguments<'a> {
    pub id: &'a str,
    pub arguments: &'a str,
    /// Rendered parse failure (serde Display, or "not a JSON object").
    pub reason: String,
}

impl ProviderError {
    /// HTTP status carried by this failure: transport `Http` failures,
    /// Bedrock pre-stream HTTP failures, and OpenAI/Anthropic in-stream API
    /// errors; `None` otherwise.
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Http { status, .. }
            | Self::Protocol {
                source:
                    ProtocolError::OpenAI {
                        source:
                            OpenAIProtocolError::Api { status, .. }
                            | OpenAIProtocolError::ResponseFailed { status, .. },
                    }
                    | ProtocolError::Anthropic {
                        source: AnthropicProtocolError::Api { status, .. },
                    },
            } => Some(*status),
            #[cfg(feature = "bedrock")]
            Self::Bedrock {
                source: crate::provider::bedrock::BedrockError::Http { status, .. },
            } => Some(*status),
            _ => None,
        }
    }

    /// Structured view when this failure is model-authored malformed
    /// tool-call arguments; `None` for every other failure.
    pub fn malformed_tool_arguments(&self) -> Option<MalformedToolArguments<'_>> {
        let Self::Protocol { source } = self else {
            return None;
        };
        match source {
            ProtocolError::OpenAI { source } => match source {
                OpenAIProtocolError::DecodeToolArguments {
                    id,
                    source,
                    arguments,
                } => Some(MalformedToolArguments {
                    id,
                    arguments,
                    reason: source.to_string(),
                }),
                OpenAIProtocolError::InvalidToolInput { id, arguments, .. } => {
                    Some(MalformedToolArguments {
                        id,
                        arguments,
                        reason: "not a JSON object".into(),
                    })
                }
                _ => None,
            },
            ProtocolError::Anthropic { source } => match source {
                AnthropicProtocolError::DecodeToolArguments {
                    id,
                    source,
                    arguments,
                } => Some(MalformedToolArguments {
                    id,
                    arguments,
                    reason: source.to_string(),
                }),
                AnthropicProtocolError::InvalidToolInput { id, arguments, .. } => {
                    Some(MalformedToolArguments {
                        id,
                        arguments,
                        reason: "not a JSON object".into(),
                    })
                }
                _ => None,
            },
            #[cfg(feature = "bedrock")]
            ProtocolError::Bedrock {
                source:
                    crate::protocol::bedrock::BedrockProtocolError::DecodeToolInput {
                        id,
                        source,
                        arguments,
                    },
            } => Some(MalformedToolArguments {
                id,
                arguments,
                reason: source.to_string(),
            }),
            #[cfg(feature = "bedrock")]
            ProtocolError::Bedrock {
                source:
                    crate::protocol::bedrock::BedrockProtocolError::InvalidToolInput {
                        id,
                        arguments,
                        ..
                    },
            } => Some(MalformedToolArguments {
                id,
                arguments,
                reason: "not a JSON object".into(),
            }),
            _ => None,
        }
    }
}
