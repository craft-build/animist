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
    #[snafu(display("OpenAI does not support reasoning token budgets"))]
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
}
