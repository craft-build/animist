//! `OpenAI` protocol adapters and wire options.

pub mod chat_completions;
pub mod embeddings;
pub(crate) mod error;
pub mod responses;
mod types;

use bon::Builder;
pub use chat_completions::ChatCompletions;
pub use embeddings::Embeddings;
pub use error::OpenAIProtocolError;
pub use responses::Responses;
use serde::Serialize;

/// `OpenAI` wire options resolved by the provider's model policy.
#[derive(crate::Struct!, PartialEq, Eq, Builder)]
#[builder(on(_, into))]
pub struct OpenAiOptions {
    /// Reasoning effort forwarded to the API when set.
    pub reasoning_effort: Option<String>,
    /// Native Chat Completions reasoning budget for llama.cpp.
    pub chat_reasoning_budget: Option<u32>,
    /// Explicit template thinking control for oMLX.
    pub chat_enable_thinking: Option<bool>,
    /// Native thinking token budget for oMLX.
    pub chat_thinking_budget: Option<u32>,
    /// Replays unsigned thinking as `reasoning_content` for compatible APIs.
    /// When disabled, unsigned thinking history returns an error.
    #[builder(default)]
    pub replay_reasoning_content: bool,
    /// Selects replay naming and string assistant content for Ollama's dialect.
    #[builder(default)]
    pub chat_reasoning_field: ChatReasoningField,
    /// Native thinking control for dialects that support it.
    pub chat_thinking: Option<ChatThinking>,
    /// Enables streamed tool arguments where the API supports it.
    pub tool_stream: Option<bool>,
    #[builder(default)]
    pub chat_token_limit: ChatTokenLimit,
}

/// Request field carrying the Chat Completions output token limit.
#[derive(crate::Enum!, Default)]
pub enum ChatTokenLimit {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

/// Replay field naming for unsigned assistant thinking.
#[derive(crate::Enum!, Default)]
pub enum ChatReasoningField {
    #[default]
    ReasoningContent,
    Reasoning,
}

/// Native thinking switch serialized into Chat requests.
#[derive(crate::Enum!, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatThinkingMode {
    #[default]
    Enabled,
    Disabled,
}

/// Native thinking control with post-turn clearing.
#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub struct ChatThinking {
    #[serde(rename = "type")]
    pub mode: ChatThinkingMode,
    pub clear_thinking: bool,
}
