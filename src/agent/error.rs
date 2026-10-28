use snafu::Snafu;

#[derive(Snafu, Debug)]
pub enum AgentError {
    #[snafu(display("Inference attempt {} failed: {source}", attempt.attempt))]
    Inference {
        source: crate::provider::ProviderError,
        attempt: std::sync::Arc<super::InterruptedAttempt>,
    },
    #[snafu(transparent)]
    Replay {
        source: crate::protocol::ReplayError,
    },
    #[snafu(transparent)]
    Provider {
        source: crate::provider::ProviderError,
    },
    #[snafu(transparent)]
    History {
        #[snafu(source(from(crate::history::HistoryError, Box::new)))]
        source: Box<crate::history::HistoryError>,
    },
    #[snafu(display("Agent already has an active operation"))]
    AlreadyRunning,
    #[snafu(display("Agent has been shut down"))]
    ShutDown,
    #[snafu(display("Agent limits must be positive"))]
    InvalidLimits,
    #[snafu(display("Tool name must not be empty"))]
    EmptyToolName,
    #[snafu(display("Duplicate tool {name}"))]
    DuplicateTool { name: String },
    #[snafu(display("Tool nesting limit exceeded"))]
    ToolDepthExceeded,
    #[snafu(display("Run cancelled"))]
    Cancelled,
    #[snafu(display("Layer stopped the run: {reason}"))]
    Stopped { reason: String },
    #[snafu(display("Layer {layer} failed during {phase}: {source}"))]
    Layer {
        layer: String,
        phase: String,
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[snafu(display("Model returned duplicate or empty tool call ID {id}"))]
    InvalidToolCallId { id: String },
    #[snafu(display("A layer changed tool declarations without changing the registry"))]
    ToolDeclarationsChanged,
    #[snafu(display(
        "History changed during the model step or tool execution; uncommitted messages are retained in the error"
    ))]
    HistoryChanged {
        messages: Vec<crate::history::StoredMessage>,
    },
    #[snafu(display("Expected a {expected:?} message, received {actual:?}"))]
    InvalidRole {
        expected: crate::history::Role,
        actual: crate::history::Role,
    },
    #[snafu(display("Submitted input must not contain tool calls or results"))]
    ToolInputMessage,
    #[snafu(display("Assistant response must not contain tool results"))]
    AssistantToolResult,
    #[snafu(display("Provider stopped for tool use but supplied no tool calls"))]
    MissingToolCalls,
    #[snafu(display("Run failed: {source}; finalization also failed: {cleanup}"))]
    Finalization {
        source: Box<AgentError>,
        cleanup: Box<AgentError>,
    },
}
