use crate::{
    Enum, Struct,
    provider::{Effort, Message},
};
use bon::Builder;
use serde::{Deserialize, Serialize};

/// Incremental transport events for one in-flight attempt.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum ProviderEvent {
    /// A protocol-validated closed block, not authorization to execute it.
    ContentComplete { block: crate::history::ContentBlock },
    /// Latest cumulative usage for this attempt, not an additive delta.
    Usage { usage: TokenUsage },
    /// Incremental assistant text; concatenate deltas for the full text.
    TextDelta { text: String },
    /// Incremental reasoning text; concatenate deltas for the full thinking.
    ThinkingDelta { text: String },
    /// Announces a tool call before its input JSON streams.
    ToolUseStart { id: String, name: String },
    /// Partial JSON for the tool input identified by `id`.
    ToolInputDelta { id: String, partial_json: String },
}

/// Requested reasoning mode for an attempt.
#[derive(Enum!, Default)]
pub enum ThinkingConfig {
    /// Disables reasoning where the model allows it.
    #[default]
    Off,
    /// Lets the model allocate its own reasoning budget.
    Adaptive,
    /// Fixed reasoning intensity.
    Effort(Effort),
    /// Exact reasoning token budget.
    Budget(u32),
}

/// Policy for protocols without at-depth system messages.
#[derive(Enum!, Default)]
pub enum SystemMessagePolicy {
    /// Reject placement that the protocol cannot preserve.
    #[default]
    Reject,
    /// Append history instructions to the separate preamble in history order.
    MoveToPreamble,
}

/// Per-attempt request controls layered over model defaults.
#[derive(Struct!, PartialEq, Builder)]
#[builder(on(_, into))]
pub struct RequestOptions {
    /// Applies only when the protocol cannot preserve system-message placement.
    #[builder(default)]
    pub system_message_policy: SystemMessagePolicy,
    /// Reasoning selection; defaults to off.
    #[builder(default)]
    pub thinking: ThinkingConfig,
    /// Requests the provider's fast variant; models can disable it.
    #[builder(default)]
    pub fast: bool,
    /// Hard cap on generated tokens; `None` uses the model default.
    pub max_output_tokens: Option<u32>,
    /// Sampling temperature; `None` uses the server default.
    pub temperature: Option<f64>,
    /// Nucleus sampling cutoff; `None` uses the server default.
    pub top_p: Option<f64>,
    /// Penalizes repeated tokens by frequency; `None` omits the control.
    pub frequency_penalty: Option<f64>,
    /// Penalizes repeated tokens by presence; `None` omits the control.
    pub presence_penalty: Option<f64>,
    /// `None` omits the control; `Some(vec![])` sends an explicit empty list.
    pub stop_sequences: Option<Vec<String>>,
}

/// Token accounting for one attempt; rates come from `Pricing`.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct TokenUsage {
    /// Prompt tokens billed at the input rate.
    #[serde(rename = "input_tokens")]
    pub input: u32,
    /// Generated tokens billed at the output rate.
    #[serde(rename = "output_tokens")]
    pub output: u32,
    /// Prompt tokens written to the cache, billed at the write rate.
    #[serde(rename = "cache_creation_input_tokens")]
    pub cache_creation: u32,
    /// Prompt tokens served from the cache, billed at the read rate.
    #[serde(rename = "cache_read_input_tokens")]
    pub cache_read: u32,
    /// Estimated USD cost; set only when rates are known.
    #[serde(skip)]
    pub cost: Option<f64>,
}

/// Server-applied input rewrite reported with the response.
#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct InputTransformation {
    /// Provider-defined transformation identifier.
    #[serde(rename = "type")]
    pub kind: String,
    /// Location of the transformed input, when addressable.
    #[serde(default)]
    pub path: String,
    /// Why the transformation was applied.
    #[serde(default)]
    pub reason: String,
}

/// Provider-reported cause of generation stopping.
#[derive(Enum!, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its reply.
    EndTurn,
    /// The model requested a tool call.
    ToolUse,
    /// The output token limit was reached.
    MaxTokens,
    /// The provider paused the turn for a later continuation.
    PauseTurn,
    /// The provider refused the request.
    Refusal,
    /// The prompt exceeded the context window.
    ContextWindowExceeded,
}

/// Fully assembled response for one attempt.
#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct StreamResponse {
    /// Complete assistant message with all content blocks.
    pub message: Message,
    /// Final cumulative usage for the attempt.
    pub usage: TokenUsage,
    /// Provider-reported termination cause, when stated.
    pub stop_reason: Option<StopReason>,
    /// Rewrites the server applied to the prompt.
    pub input_transformations: Vec<InputTransformation>,
}
