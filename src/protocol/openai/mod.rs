//! OpenAI protocol adapters and wire options.

pub mod chat_completions;
pub(crate) mod error;
pub mod responses;
mod types;

use bon::Builder;
pub use chat_completions::ChatCompletions;
pub use error::OpenAIProtocolError;
pub use responses::Responses;

/// OpenAI wire options resolved by the provider's model policy.
#[derive(crate::Struct!, Builder)]
#[builder(on(_, into))]
pub struct OpenAiOptions {
    pub reasoning_effort: Option<String>,
}
