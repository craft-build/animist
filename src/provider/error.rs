use snafu::Snafu;

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
    #[snafu(display("Provider returned HTTP {status}: {body}"))]
    Http {
        status: u16,
        body: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(transparent)]
    Protocol {
        source: crate::protocol::ProtocolError,
    },
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
    #[snafu(display("Model listing has no data array"))]
    MissingModelData { backtrace: snafu::Backtrace },
    #[snafu(display("Model listing entry has no model ID"))]
    MissingModelId { backtrace: snafu::Backtrace },
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
