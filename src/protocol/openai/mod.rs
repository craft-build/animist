//! OpenAI protocol adapters and wire options.

pub mod chat_completions;
pub(crate) mod error;
pub mod responses;
mod types;

use bon::Builder;
pub use chat_completions::ChatCompletions;
pub use error::OpenAIProtocolError;
pub use responses::Responses;
use serde::Serialize;

/// OpenAI wire options resolved by the provider's model policy.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct OpenAiOptions {
    pub reasoning_effort: Option<String>,
    /// Replays unsigned thinking as `reasoning_content` for compatible APIs.
    /// When disabled, unsigned thinking history returns an error.
    #[builder(default)]
    pub replay_reasoning_content: bool,
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

#[derive(crate::Enum!, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatThinkingMode {
    #[default]
    Enabled,
    Disabled,
}

#[derive(crate::Struct!, Eq, Builder, Serialize)]
#[builder(on(_, into))]
pub struct ChatThinking {
    #[serde(rename = "type")]
    pub mode: ChatThinkingMode,
    pub clear_thinking: bool,
}
