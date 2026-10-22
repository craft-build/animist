use snafu::Snafu;

#[derive(Snafu, Debug)]
#[snafu(visibility(pub(crate)))]
pub enum ProviderError {
    #[snafu(display("Provider transport failed: {source}"))]
    Transport { source: reqwest::Error },
    #[snafu(display("Provider returned HTTP {status}: {body}"))]
    Http { status: u16, body: String },
    #[snafu(transparent)]
    Protocol {
        source: crate::protocol::ProtocolError,
    },
    #[snafu(display("Explicit API key is empty"))]
    EmptyApiKey,
    #[snafu(display("This model requires reasoning"))]
    ReasoningRequired,
    #[snafu(display("Provider does not support reasoning token budgets"))]
    ReasoningBudgetUnsupported,
    #[snafu(display("Unsupported reasoning effort {effort:?}"))]
    UnsupportedReasoningEffort { effort: super::Effort },
    #[snafu(display("Model belongs to {actual}, expected {expected}"))]
    ModelProviderMismatch { expected: String, actual: String },
    #[snafu(display("Fast mode is disabled for this model"))]
    FastModeDisabled,
    #[snafu(display("Model listing has no data array"))]
    MissingModelData,
    #[snafu(display("Model listing entry has no model ID"))]
    MissingModelId,
    #[snafu(display("Credential environment variable {variable} is missing or empty"))]
    Credentials { variable: String },
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
    ThinkingUnsupported { model: String },
    #[snafu(display(
        "Thinking budget {budget} must be at least 1024 and less than max_tokens {max_tokens}"
    ))]
    InvalidThinkingBudget { budget: u32, max_tokens: u32 },
    #[snafu(display("Paginated model listing has no last_id"))]
    MissingModelCursor,
    #[snafu(display("Paginated model listing repeated cursor {cursor}"))]
    RepeatedModelCursor { cursor: String },
    #[snafu(display("Invalid Synthetic model listing: {source}"))]
    DecodeSyntheticModels { source: serde_json::Error },
    #[snafu(display("Invalid Synthetic price for {field}: {value}"))]
    InvalidSyntheticPrice {
        field: &'static str,
        value: serde_json::Value,
    },
    #[snafu(display("Provider {provider} does not support fast mode"))]
    FastModeUnsupported { provider: String },
    #[snafu(display("Maximum output token limit must be positive"))]
    ZeroOutputTokenLimit,
}
