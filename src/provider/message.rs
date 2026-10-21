use crate::{Enum, Struct};
use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
}

#[derive(Enum!, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },
    RedactedThinking {
        data: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thought_signature: Option<String>,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        loaded_tools: Vec<String>,
    },
    /* Image { source: ImageSource } */
}

impl ContentBlock {
    pub fn is_thinking(&self) -> bool {
        matches!(self, Self::Thinking { .. } | Self::RedactedThinking { .. })
    }
}

#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Turn,
    Observation,
}

impl MessageKind {
    fn is_turn(&self) -> bool {
        matches!(self, Self::Turn)
    }
}

#[derive(Struct!, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Message {
    pub role: String,
    pub content: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    #[serde(default, skip_serializing_if = "MessageKind::is_turn")]
    pub kind: MessageKind,
}
