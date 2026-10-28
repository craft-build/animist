use snafu::Snafu;

#[derive(Snafu, Debug)]
#[snafu(visibility(pub(super)))]
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
    History {
        #[snafu(source(from(crate::history::HistoryError, Box::new)))]
        source: Box<crate::history::HistoryError>,
    },
    #[snafu(display("Agent already has an active operation"))]
    AlreadyRunning { backtrace: snafu::Backtrace },
    #[snafu(display("Agent has been shut down"))]
    ShutDown { backtrace: snafu::Backtrace },
    #[snafu(display("Agent limits must be positive"))]
    InvalidLimits { backtrace: snafu::Backtrace },
    #[snafu(display("Tool name must not be empty"))]
    EmptyToolName { backtrace: snafu::Backtrace },
    #[snafu(display("Duplicate tool {name}"))]
    DuplicateTool {
        name: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Tool nesting limit exceeded"))]
    ToolDepthExceeded { backtrace: snafu::Backtrace },
    #[snafu(display("Run tool call limit exceeded"))]
    ToolCallLimitExceeded { backtrace: snafu::Backtrace },
    #[snafu(display("Child tool call limit exceeded"))]
    ChildCallLimitExceeded { backtrace: snafu::Backtrace },
    #[snafu(display("Only exclusive tools may dispatch children"))]
    ChildDispatchRequiresExclusive { backtrace: snafu::Backtrace },
    #[snafu(display("Tool {name} timed out; external effects may have occurred"))]
    ToolTimeout {
        name: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Run cancelled"))]
    Cancelled { backtrace: snafu::Backtrace },
    #[snafu(display("Layer stopped the run: {reason}"))]
    Stopped {
        reason: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Model returned duplicate or empty tool call ID {id}"))]
    InvalidToolCallId {
        id: String,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("A layer added or modified a tool declaration"))]
    ToolDeclarationsChanged { backtrace: snafu::Backtrace },
    #[snafu(display(
        "History changed during the model step or tool execution; uncommitted messages are retained in the error"
    ))]
    HistoryChanged {
        messages: Vec<crate::history::StoredMessage>,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Expected a {expected:?} message, received {actual:?}"))]
    InvalidRole {
        expected: crate::history::Role,
        actual: crate::history::Role,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("Submitted input must not contain tool calls or results"))]
    ToolInputMessage { backtrace: snafu::Backtrace },
    #[snafu(display("Assistant response must not contain tool results"))]
    AssistantToolResult { backtrace: snafu::Backtrace },
    #[snafu(display("Provider stopped for tool use but supplied no tool calls"))]
    MissingToolCalls { backtrace: snafu::Backtrace },
    #[snafu(display("Run failed: {source}; finalization also failed: {cleanup}"))]
    Finalization {
        source: Box<AgentError>,
        cleanup: Box<AgentError>,
    },
}
