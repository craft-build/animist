//! Anthropic Messages protocol and provider-resolved wire options.

mod error;
pub mod messages;
mod types;

use bon::Builder;
use serde::{Deserialize, Serialize};

pub use error::AnthropicProtocolError;
pub use messages::Messages;

#[derive(crate::Enum!, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AnthropicThinking {
    Disabled,
    Adaptive,
    Enabled { budget_tokens: u32 },
}

#[derive(crate::Enum!, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnthropicEffort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// Wire options resolved by provider model policy.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct AnthropicOptions {
    pub max_tokens: u32,
    pub thinking: Option<AnthropicThinking>,
    pub effort: Option<AnthropicEffort>,
    pub fast: bool,
}
