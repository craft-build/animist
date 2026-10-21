use crate::{
    Enum, Struct,
    provider::{Effort, Message},
};
use bon::Builder;
use serde::{Deserialize, Serialize};

#[derive(Enum!, Deserialize, Serialize)]
pub enum ProviderEvent {
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

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct RequestOptions {
    pub thinking: ThinkingConfig,
    pub fast: bool,
    pub max_output_tokens: Option<u32>,
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
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
}

#[derive(Struct!, Builder)]
#[builder(on(_, into))]
pub struct StreamResponse {
    pub message: Message,
    pub usage: TokenUsage,
    pub stop_reason: Option<StopReason>,
    pub input_transformations: Vec<InputTransformation>,
}
