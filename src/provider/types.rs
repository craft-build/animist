use crate::{
    Enum, Struct,
    provider::{Effort, Message},
};
use bon::Builder;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub enum ProviderEvent {
    /// A protocol-validated closed block, not authorization to execute it.
    ContentComplete {
        block: crate::history::ContentBlock,
    },
    /// Latest cumulative usage for this attempt, not an additive delta.
    Usage {
        usage: TokenUsage,
    },
    TextDelta {
        text: String,
    },
    ThinkingDelta {
        text: String,
    },
    ToolUseStart {
        id: String,
        name: String,
    },
    ToolInputDelta {
        id: String,
        partial_json: String,
    },
    PromptProgress {
        processed: u32,
        total: u32,
        cache: u32,
    },
}

#[derive(Enum!, Default)]
pub enum ThinkingConfig {
    #[default]
    Off,
    Adaptive,
    Effort(Effort),
    Budget(u32),
}

#[derive(Struct!, PartialEq, Builder)]
#[builder(on(_, into))]
pub struct RequestOptions {
    #[builder(default)]
    pub thinking: ThinkingConfig,
    #[builder(default)]
    pub fast: bool,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub frequency_penalty: Option<f64>,
    pub presence_penalty: Option<f64>,
    /// `None` omits the control; `Some(vec![])` sends an explicit empty list.
    pub stop_sequences: Option<Vec<String>>,
}

#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct TokenUsage {
    #[serde(rename = "input_tokens")]
    pub input: u32,
    #[serde(rename = "output_tokens")]
    pub output: u32,
    #[serde(rename = "cache_creation_input_tokens")]
    pub cache_creation: u32,
    #[serde(rename = "cache_read_input_tokens")]
    pub cache_read: u32,
    #[serde(skip)]
    pub cost: Option<f64>,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
pub struct InputTransformation {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Enum!, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    ToolUse,
    MaxTokens,
    PauseTurn,
    Refusal,
    ContextWindowExceeded,
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<StopReason>,
    pub input_transformations: Vec<InputTransformation>,
}
