//! OpenAI protocol adapters and wire options.

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

/// OpenAI wire options resolved by the provider's model policy.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct OpenAiOptions {
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
    pub chat_thinking: Option<ChatThinking>,
    pub tool_stream: Option<bool>,
    #[builder(default)]
    pub chat_token_limit: ChatTokenLimit,
}

#[derive(crate::Enum!, Default)]
pub enum ChatTokenLimit {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

#[derive(crate::Enum!, Default)]
pub enum ChatReasoningField {
    #[default]
    ReasoningContent,
    Reasoning,
}

#[derive(crate::Enum!, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatThinkingMode {
    #[default]
    Enabled,
    Disabled,
}

#[derive(crate::Struct!, PartialEq, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub struct ChatThinking {
    #[serde(rename = "type")]
    pub mode: ChatThinkingMode,
    pub clear_thinking: bool,
}
