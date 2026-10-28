use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{AnthropicEffort, AnthropicThinking};
use crate::history::{ImageSource, Role, ToolContents};

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct Request {
    pub model: String,
    pub messages: Vec<InputMessage>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<AnthropicThinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_config: Option<OutputConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speed: Option<Speed>,
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct InputMessage {
    pub role: Role,
    pub content: Vec<InputBlock>,
}

#[derive(crate::Enum!, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum InputBlock {
    Text {
        text: String,
    },
    Image {
        source: ImageSource,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: ToolContents,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(crate::Struct!, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Debug, Clone, PartialEq, Builder, Serialize)]
#[builder(on(_, into))]
pub(super) struct OutputConfig {
    pub effort: AnthropicEffort,
}

#[derive(crate::Enum!, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Speed {
    Fast,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum Event {
    MessageStart {
        message: MessageStart,
    },
    ContentBlockStart {
        index: u32,
        content_block: StartBlock,
    },
    ContentBlockDelta {
        index: u32,
        delta: BlockDelta,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: UsageDelta,
    },
    MessageStop,
    Ping,
    Error {
        error: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(crate::Struct!, PartialEq, Deserialize)]
pub(super) struct MessageStart {
    pub role: Role,
    pub content: Vec<Value>,
    pub usage: InitialUsage,
}

#[derive(crate::Struct!, PartialEq, Deserialize)]
pub(super) struct InitialUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}

#[derive(crate::Struct!, PartialEq, Deserialize)]
pub(super) struct UsageDelta {
    pub output_tokens: u32,
    pub input_tokens: Option<u32>,
    pub cache_creation_input_tokens: Option<u32>,
    pub cache_read_input_tokens: Option<u32>,
}

#[derive(crate::Struct!, PartialEq, Deserialize)]
pub(super) struct MessageDelta {
    pub stop_reason: Option<String>,
}

#[derive(crate::Enum!, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum StartBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        signature: String,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
}

#[derive(crate::Enum!, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(super) enum BlockDelta {
    #[serde(rename = "text_delta")]
    Text { text: String },
    #[serde(rename = "thinking_delta")]
    Thinking { thinking: String },
    #[serde(rename = "signature_delta")]
    Signature { signature: String },
    #[serde(rename = "input_json_delta")]
    InputJson { partial_json: String },
}
