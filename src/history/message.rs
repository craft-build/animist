use crate::{Enum, Struct};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use bon::Builder;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Conversation authority, independent of consumer-facing message kind.
#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    User,
    Assistant,
    /// At-depth instructions; only text content is supported.
    System,
}

/// Inline or remote image reference in messages and tool output.
#[derive(Enum!, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageSource {
    Base64 { media_type: String, data: String },
    Url { url: String },
}

impl ImageSource {
    /// Returns the remote URL or a data URL for inline image content.
    pub fn to_url(&self) -> String {
        match self {
            Self::Url { url } => url.clone(),
            Self::Base64 { media_type, data } => {
                format!("data:{media_type};base64,{data}")
            }
        }
    }

    /// Encodes raw image bytes using standard padded base64.
    pub fn from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        Self::Base64 {
            media_type: media_type.into(),
            data: STANDARD.encode(bytes),
        }
    }
}

/// Model-visible tool output, without recursive conversation blocks.
#[derive(Enum!, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Image { source: ImageSource },
}

/// Ordered tool output; accepts legacy strings when deserializing.
#[derive(Struct!, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct ToolContents(pub Vec<ToolContent>);

impl From<String> for ToolContents {
    fn from(text: String) -> Self {
        Self(vec![ToolContent::Text { text }])
    }
}

impl From<&str> for ToolContents {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl From<Vec<ToolContent>> for ToolContents {
    fn from(content: Vec<ToolContent>) -> Self {
        Self(content)
    }
}

impl<'de> Deserialize<'de> for ToolContents {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Text(String),
            Blocks(Vec<ToolContent>),
        }
        Ok(match Stored::deserialize(deserializer)? {
            Stored::Text(text) => text.into(),
            Stored::Blocks(blocks) => blocks.into(),
        })
    }
}

/// One conversation content block: text, reasoning, tool exchange, or image.
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
        /// History metadata; omitted by protocol adapters and absent in legacy results.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        content: ToolContents,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        loaded_tools: Vec<String>,
    },
    Image {
        source: ImageSource,
    },
}

impl ContentBlock {
    /// Whether the block holds reasoning content, redacted or not.
    pub fn is_thinking(&self) -> bool {
        matches!(self, Self::Thinking { .. } | Self::RedactedThinking { .. })
    }
}

/// Distinguishes user turns from injected observations.
#[derive(Enum!, Default, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Turn,
    Observation,
}

impl MessageKind {
    /// Whether the message is a user-authored turn.
    pub fn is_turn(&self) -> bool {
        matches!(self, Self::Turn)
    }
}

/// One conversation message: role, ordered content blocks, and kind.
#[derive(Struct!, PartialEq, Builder, Deserialize, Serialize)]
#[builder(on(_, into))]
#[serde(rename_all = "snake_case")]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
    /// Optional consumer-facing rendering; never sent to the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// `Turn` messages start user turns; defaults to `Turn`.
    #[serde(default, skip_serializing_if = "MessageKind::is_turn")]
    pub kind: MessageKind,
}
